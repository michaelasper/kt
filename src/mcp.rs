use crate::debug_lsp::{
    append_feedback_record, feedback_path, file_uri_to_path, read_feedback_records,
    relative_path_for_root, resolve_file_path, DebugFeedbackRecord, DebugFeedbackVerdict,
    DebugLspManager,
};
use crate::diagnostics::{DiagnosticEvent, Diagnostics, DiagnosticsArc};
use crate::embedding::EmbeddingEngine;
use crate::git;
use crate::storage::Storage;
use crate::{Config, FileRole, Language, QueryRequest, QueryResponse, QueryStatus, SearchResult};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, transport::stdio, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

fn mcp_error(msg: impl std::fmt::Display) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(format!("{msg}"), None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolWarning {
    source: &'static str,
    message: String,
}

impl ToolWarning {
    fn shadow_index(message: impl Into<String>) -> Self {
        Self {
            source: "shadow_index",
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct KtServer {
    inner: Arc<KtServerInner>,
}

#[derive(Debug, Clone)]
pub struct DebugKtServer {
    public: KtServer,
    lsp: Arc<DebugLspManager>,
    feedback_path: PathBuf,
}

#[derive(Debug)]
struct KtServerInner {
    storage: RwLock<Storage>,
    embedding: RwLock<Option<Arc<EmbeddingEngine>>>,
    config: Config,
    diagnostics: DiagnosticsArc,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    #[schemars(description = "The search query - natural language or code terms")]
    pub query: String,
    #[schemars(
        description = "Filter by language: rust, go, java, python, swift, objective-c, markdown, html, typescript, tsx, or javascript"
    )]
    pub language: Option<String>,
    #[schemars(description = "Number of results to return (default: 3, max: 10)")]
    pub top_k: Option<usize>,
    #[schemars(
        description = "If true, return only function/type signatures without bodies to save tokens"
    )]
    pub headers_only: Option<bool>,
    #[schemars(description = "Optional directory path to scope search to one indexed codebase")]
    pub directory_path: Option<String>,
    #[schemars(description = "Optional codebase alias to scope search to one indexed codebase")]
    pub codebase_alias: Option<String>,
    #[schemars(
        description = "Filter by file role: implementation, test, fixture, generated, config"
    )]
    pub file_role: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadFileParams {
    #[schemars(description = "Repository-relative path to the file")]
    pub filepath: String,
    #[schemars(
        description = "Optional directory path to scope file reads to one indexed codebase"
    )]
    pub directory_path: Option<String>,
    #[schemars(
        description = "Optional codebase alias to scope file reads to one indexed codebase"
    )]
    pub codebase_alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncParams {
    #[schemars(description = "Path to the directory to sync")]
    pub directory_path: String,
    #[schemars(description = "Force full re-index instead of partial sync")]
    pub full: Option<bool>,
    #[schemars(description = "Optional alias to use for this codebase")]
    pub codebase_alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GitStatusParams {
    #[schemars(description = "Path to the git repository")]
    pub directory_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IndexPrParams {
    #[schemars(description = "Path to the git repository")]
    pub directory_path: String,
    #[schemars(description = "Compare against this branch (default: 'main')")]
    pub base_branch: Option<String>,
    #[schemars(description = "Shadow index TTL in seconds (default: 7200 / 2 hours)")]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListCodebasesParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugCodebaseParams {
    #[schemars(description = "Optional directory path to select a codebase root")]
    pub directory_path: Option<String>,
    #[schemars(description = "Optional codebase alias to select a codebase root")]
    pub codebase_alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugLspPositionParams {
    #[schemars(description = "Repository-relative or absolute file path")]
    pub filepath: String,
    #[schemars(description = "Zero-based line number")]
    pub line: usize,
    #[schemars(description = "Zero-based character offset")]
    pub character: usize,
    #[schemars(description = "Optional directory path to select a codebase root")]
    pub directory_path: Option<String>,
    #[schemars(description = "Optional codebase alias to select a codebase root")]
    pub codebase_alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugLspReferencesParams {
    #[schemars(description = "Repository-relative or absolute file path")]
    pub filepath: String,
    #[schemars(description = "Zero-based line number")]
    pub line: usize,
    #[schemars(description = "Zero-based character offset")]
    pub character: usize,
    #[schemars(description = "Whether to include the symbol declaration in references")]
    pub include_declaration: Option<bool>,
    #[schemars(description = "Optional directory path to select a codebase root")]
    pub directory_path: Option<String>,
    #[schemars(description = "Optional codebase alias to select a codebase root")]
    pub codebase_alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugLspDocumentSymbolsParams {
    #[schemars(description = "Repository-relative or absolute file path")]
    pub filepath: String,
    #[schemars(description = "Maximum symbol nodes to return; default 80, set 0 for unlimited")]
    pub max_symbols: Option<usize>,
    #[schemars(description = "Optional directory path to select a codebase root")]
    pub directory_path: Option<String>,
    #[schemars(description = "Optional codebase alias to select a codebase root")]
    pub codebase_alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugChunkAtParams {
    #[schemars(description = "Repository-relative or absolute file path")]
    pub filepath: String,
    #[schemars(description = "Zero-based line number")]
    pub line: usize,
    #[schemars(description = "Optional directory path to select a codebase root")]
    pub directory_path: Option<String>,
    #[schemars(description = "Optional codebase alias to select a codebase root")]
    pub codebase_alias: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugFeedbackParams {
    #[schemars(description = "One of: helpful, not_helpful, bug, idea")]
    pub verdict: String,
    #[schemars(description = "Short feedback summary")]
    pub summary: String,
    #[schemars(description = "Optional scenario being debugged")]
    pub scenario: Option<String>,
    #[schemars(description = "Optional evidence for the verdict")]
    pub evidence: Option<String>,
    #[schemars(description = "Optional recommendation for kt")]
    pub recommendation: Option<String>,
    #[schemars(description = "Optional active tool name that produced the result")]
    pub active_tool: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugFeedbackReadParams {
    #[schemars(description = "Maximum number of most recent feedback entries to read")]
    pub limit: Option<usize>,
}

impl KtServer {
    async fn with_diagnostics<F, Fut, T>(&self, tool_name: &str, f: F) -> Result<T, rmcp::ErrorData>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, rmcp::ErrorData>>,
    {
        if !self.inner.diagnostics.is_enabled() {
            return f().await;
        }

        let start = std::time::Instant::now();
        let result = f().await;
        let success = result.is_ok();

        self.inner
            .diagnostics
            .emit(DiagnosticEvent::ToolInvoke {
                name: tool_name.to_string(),
                duration_ms: start.elapsed().as_millis(),
                success,
            })
            .await;

        result
    }

    pub fn new(config: Config) -> anyhow::Result<Self> {
        let storage = Storage::new(&config)?;
        let global_manager = crate::global_config::GlobalConfigManager::new()?;
        let diagnostics = Arc::new(Diagnostics::new(
            config.diagnostics.clone(),
            global_manager.get_config_dir(),
        ));

        Ok(Self {
            inner: Arc::new(KtServerInner {
                storage: RwLock::new(storage),
                embedding: RwLock::new(None),
                config,
                diagnostics,
            }),
        })
    }

    pub async fn ensure_ready(&self) -> anyhow::Result<()> {
        {
            let storage = self.inner.storage.read().await;
            storage.ensure_index().await?;
        }

        {
            let mut embedding = self.inner.embedding.write().await;
            if embedding.is_none() {
                let engine = EmbeddingEngine::new(&self.inner.config).await?;
                *embedding = Some(Arc::new(engine));
            }
        }

        Ok(())
    }
}

impl DebugKtServer {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let public = KtServer::new(config)?;
        let global_manager = crate::global_config::GlobalConfigManager::new()?;
        let feedback_path = feedback_path(global_manager.get_config_dir());

        Ok(Self {
            public,
            lsp: Arc::new(DebugLspManager::new()),
            feedback_path,
        })
    }

    async fn resolve_debug_root(
        &self,
        directory_path: Option<&str>,
        codebase_alias: Option<&str>,
    ) -> Result<PathBuf, rmcp::ErrorData> {
        match (directory_path, codebase_alias) {
            (Some(path), None) => validate_directory_path(path),
            (_, Some(_)) => {
                let storage = self.public.inner.storage.read().await;
                let codebase = resolve_codebase_selector(&storage, directory_path, codebase_alias)
                    .await?
                    .ok_or_else(|| {
                        mcp_error("directory_path or codebase_alias did not resolve to a codebase")
                    })?;
                validate_directory_path(&codebase.root_path)
            }
            (None, None) => std::env::current_dir().map_err(mcp_error).and_then(|path| {
                path.canonicalize()
                    .map_err(|error| mcp_error(format!("Failed to resolve current dir: {error}")))
            }),
        }
    }

    async fn resolve_debug_codebase(
        &self,
        root: &Path,
        directory_path: Option<&str>,
        codebase_alias: Option<&str>,
    ) -> Result<crate::Codebase, rmcp::ErrorData> {
        if codebase_alias.is_some() {
            let storage = self.public.inner.storage.read().await;
            if let Some(codebase) =
                resolve_codebase_selector(&storage, directory_path, codebase_alias).await?
            {
                return Ok(codebase);
            }
        }

        crate::Codebase::from_root(root, None).map_err(mcp_error)
    }
}

#[tool_router]
impl KtServer {
    #[tool(
        description = "Search the indexed codebase using hybrid vector + keyword search. Use this to find code by semantic intent (e.g. 'how do we handle passwords') or exact names (e.g. 'BcryptHasher')."
    )]
    async fn kt_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.with_diagnostics("kt_search", || self.kt_search_inner(params))
            .await
    }

    async fn kt_search_inner(
        &self,
        params: SearchParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let start = std::time::Instant::now();
        let query_len = params.query.len();

        self.ensure_ready().await.map_err(mcp_error)?;

        let top_k = params.top_k.unwrap_or(3).min(10);
        let headers_only = params.headers_only.unwrap_or(false);
        let language = params.language.as_deref().and_then(Language::parse);
        let file_role = params.file_role.as_deref().and_then(FileRole::parse);

        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let query_embedding = engine.embed(&params.query).await.map_err(mcp_error)?;

        let storage = self.inner.storage.read().await;
        let codebase = resolve_codebase_selector(
            &storage,
            params.directory_path.as_deref(),
            params.codebase_alias.as_deref(),
        )
        .await
        .map_err(mcp_error)?;
        let codebase_id = codebase.as_ref().map(|c| c.codebase_id.as_str());

        let main_results = storage
            .hybrid_search_scoped(
                &query_embedding,
                &params.query,
                language.as_ref(),
                codebase_id,
                file_role.as_ref(),
                top_k,
            )
            .await
            .map_err(mcp_error)?;

        let mut warnings = Vec::new();
        let shadow_results = match storage
            .search_shadow_scoped(
                &query_embedding,
                &params.query,
                language.as_ref(),
                codebase_id,
                file_role.as_ref(),
                top_k,
            )
            .await
        {
            Ok(results) => results,
            Err(error) => {
                let message = format!("Shadow search failed: {error}");
                warn!(error = %error, "Shadow search failed");
                warnings.push(ToolWarning::shadow_index(message));
                Vec::new()
            }
        };

        let shadow_ids: std::collections::HashSet<String> =
            shadow_results.iter().map(|r| r.chunk_id.clone()).collect();

        let filtered_main: Vec<SearchResult> = main_results
            .into_iter()
            .filter(|r| !shadow_ids.contains(&r.chunk_id))
            .collect();

        let results = merge_and_deduplicate(filtered_main, shadow_results, top_k);

        self.inner
            .diagnostics
            .emit(DiagnosticEvent::Search {
                query_len,
                results_count: results.len(),
                duration_ms: start.elapsed().as_millis(),
                source: "hybrid".to_string(),
            })
            .await;

        let one_hop = resolve_one_hop_context(&storage, &results).await;
        let related = resolve_call_context(&results, &storage, codebase_id).await;

        let xml = format_search_results(
            &results,
            &params.query,
            headers_only,
            &one_hop,
            &related,
            &warnings,
        );
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Read the full contents of a file by its repository-relative path. Bypasses vector search and returns all indexed chunks for the file."
    )]
    async fn kt_read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.with_diagnostics("kt_read_file", || self.kt_read_file_inner(params))
            .await
    }

    async fn kt_read_file_inner(
        &self,
        params: ReadFileParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ready().await.map_err(mcp_error)?;

        let storage = self.inner.storage.read().await;
        let codebase = resolve_codebase_selector(
            &storage,
            params.directory_path.as_deref(),
            params.codebase_alias.as_deref(),
        )
        .await
        .map_err(mcp_error)?;
        let codebase_id = codebase.as_ref().map(|c| c.codebase_id.as_str());

        let mut warnings = Vec::new();
        let shadow_results = match storage
            .read_shadow_file_chunks_scoped(&params.filepath, codebase_id)
            .await
        {
            Ok(results) => results,
            Err(error) => {
                let message = format!("Shadow file read failed for {}: {error}", params.filepath);
                warn!(filepath = %params.filepath, error = %error, "Shadow file read failed");
                warnings.push(ToolWarning::shadow_index(message));
                Vec::new()
            }
        };

        let main_results = storage
            .read_file_chunks_scoped(&params.filepath, codebase_id)
            .await
            .map_err(mcp_error)?;

        let shadow_ids: std::collections::HashSet<String> =
            shadow_results.iter().map(|r| r.chunk_id.clone()).collect();
        let mut results = main_results
            .into_iter()
            .filter(|r| !shadow_ids.contains(&r.chunk_id))
            .collect::<Vec<_>>();
        results.extend(shadow_results);

        if results.is_empty() {
            let mut err_msg = format!("No chunks found for file: {}", params.filepath);
            if !warnings.is_empty() {
                err_msg.push_str("\nWarnings:");
                for w in &warnings {
                    err_msg.push_str(&format!("\n- [{}] {}", w.source, w.message));
                }
            }
            return Err(mcp_error(err_msg));
        }

        let results = deduplicate_results(results);
        let xml = format_file_results(&results, &params.filepath, false, &warnings);
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Sync (index) a directory into the knowledge base. Parses all .rs, .go, and .java files using Tree-sitter, generates embeddings, and stores them in Redis for search."
    )]
    async fn kt_sync(
        &self,
        Parameters(params): Parameters<SyncParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.with_diagnostics("kt_sync", || self.kt_sync_inner(params))
            .await
    }

    async fn kt_sync_inner(&self, params: SyncParams) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ready().await.map_err(mcp_error)?;

        let root = validate_directory_path(&params.directory_path)?;

        let full = params.full.unwrap_or(false);
        info!(
            "Starting sync for {} (full: {})",
            params.directory_path, full
        );

        let storage = self.inner.storage.read().await;
        let codebase = storage
            .register_codebase(&root, params.codebase_alias.as_deref())
            .await
            .map_err(mcp_error)?;
        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let discovery_options = self.inner.config.discovery_options();
        let plan = crate::sync::plan_with_options(
            &root,
            &storage,
            &codebase,
            full,
            &discovery_options,
            self.inner.diagnostics.clone(),
        )
        .await
        .map_err(mcp_error)?;

        if plan.files.is_empty() {
            crate::sync::finalize(&root, &codebase, &plan.strategy, &storage)
                .await
                .map_err(mcp_error)?;
            return Ok(CallToolResult::success(vec![Content::text(
                "<result>No supported files found to sync</result>".to_string(),
            )]));
        }

        let strategy = plan.strategy.clone();
        let progress = Arc::new(tokio::sync::Mutex::new(crate::sync::NoopProgress));
        let stats = crate::sync::execute(
            plan,
            &codebase,
            &storage,
            engine.clone(),
            progress,
            self.inner.diagnostics.clone(),
        )
        .await
        .map_err(mcp_error)?;

        crate::sync::finalize(&root, &codebase, &strategy, &storage)
            .await
            .map_err(mcp_error)?;

        let msg = format!(
            "Sync complete: {} files, {} chunks indexed, {} errors",
            stats.total_files, stats.total_chunks, stats.errors
        );

        if stats.errors > 0 {
            warn!("{msg}");
            return Err(rmcp::ErrorData::internal_error(
                msg,
                Some(serde_json::json!({
                    "codebase_id": codebase.codebase_id,
                    "codebase_alias": codebase.alias,
                    "root_path": codebase.root_path,
                })),
            ));
        }

        let xml = format!(
            "<result codebase_id=\"{}\" codebase_alias=\"{}\" root_path=\"{}\">{}</result>",
            xml_escape(&codebase.codebase_id),
            xml_escape(codebase.alias.as_deref().unwrap_or("")),
            xml_escape(&codebase.root_path),
            xml_escape(&msg)
        );
        info!("{xml}");
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Get git status information including current branch, commit SHA, and changed files."
    )]
    async fn kt_git_status(
        &self,
        Parameters(params): Parameters<GitStatusParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.with_diagnostics("kt_git_status", || self.kt_git_status_inner(params))
            .await
    }

    async fn kt_git_status_inner(
        &self,
        params: GitStatusParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let root = validate_directory_path(&params.directory_path)?;
        let git_info = tokio::task::spawn_blocking(move || git::get_git_info(&root))
            .await
            .map_err(mcp_error)?
            .map_err(mcp_error)?;

        let branch = git_info.branch.unwrap_or_else(|| "detached".to_string());
        let commit_sha = git_info.commit_sha.unwrap_or_else(|| "unknown".to_string());

        let mut changed_files_xml = String::new();
        for file in &git_info.changed_files {
            changed_files_xml.push_str(&format!(
                "  <changed_file status=\"{}\">{}</changed_file>\n",
                file.status,
                xml_escape(&file.path)
            ));
        }

        let xml = format!(
            "<git_status branch=\"{}\" commit=\"{}\" dirty=\"{}\">\n{}</git_status>",
            xml_escape(&branch),
            xml_escape(&commit_sha),
            git_info.is_dirty,
            changed_files_xml
        );

        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Index a pull request or working tree changes into the shadow (ephemeral) index. Only changed files are indexed. Shadow chunks auto-expire after TTL (default: 2 hours)."
    )]
    async fn kt_index_pr(
        &self,
        Parameters(params): Parameters<IndexPrParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.with_diagnostics("kt_index_pr", || self.kt_index_pr_inner(params))
            .await
    }

    async fn kt_index_pr_inner(
        &self,
        params: IndexPrParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let start = std::time::Instant::now();
        self.ensure_ready().await.map_err(mcp_error)?;

        let root = validate_directory_path(&params.directory_path)?;

        let storage = self.inner.storage.read().await;
        let codebase = storage
            .register_codebase(&root, None)
            .await
            .map_err(mcp_error)?;
        storage.ensure_shadow_index().await.map_err(mcp_error)?;

        let base_ref = params.base_branch.as_deref().unwrap_or("main").to_string();
        let ttl_seconds = params.ttl_seconds.unwrap_or(7200);

        let root_buf = root.to_path_buf();
        let base_ref_clone = base_ref.clone();
        let changed_files = tokio::task::spawn_blocking(move || {
            git::get_worktree_diff_files(&root_buf, &base_ref_clone)
        })
        .await
        .map_err(mcp_error)?
        .map_err(mcp_error)?;

        if changed_files.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "<result>No changed files found to index.</result>".to_string(),
            )]));
        }

        info!("Indexing PR: {} changed files", changed_files.len());

        let mut total_chunks = 0usize;
        let mut total_files = 0usize;

        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let discovery_options = self.inner.config.discovery_options();
        for filepath in &changed_files {
            if discovery_options.is_excluded_relative_path(std::path::Path::new(filepath)) {
                info!("Skipping excluded file: {}", filepath);
                continue;
            }

            let file_path = root.join(filepath);
            if !file_path.exists() {
                info!("Skipping deleted file: {}", filepath);
                continue;
            }

            let language = crate::Language::from_extension(
                file_path.extension().and_then(|e| e.to_str()).unwrap_or(""),
            );

            if language.is_none() {
                continue;
            }

            let language = language.unwrap();
            let file_role = crate::FileRole::detect(filepath, language);
            let chunks = match crate::indexing::parse_file_async(
                file_path,
                filepath.to_string(),
                language,
                codebase.codebase_id.clone(),
                file_role,
            )
            .await
            {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to parse {}: {e}", filepath);
                    continue;
                }
            };

            if chunks.is_empty() {
                continue;
            }

            let texts: Vec<String> = chunks
                .iter()
                .map(crate::embedding::chunk_embedding_text)
                .collect();
            let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let embeddings_result = engine.embed_batch(&text_refs).await;

            match embeddings_result {
                Ok(embeddings) => {
                    if let Err(e) = storage
                        .store_shadow_chunks_batch(&chunks, &embeddings, ttl_seconds)
                        .await
                    {
                        warn!("Failed to store shadow chunks for {}: {e}", filepath);
                        continue;
                    }
                    total_chunks += chunks.len();
                    total_files += 1;
                }
                Err(e) => {
                    warn!("Failed to embed chunks for {}: {e}", filepath);
                }
            }
        }

        self.inner
            .diagnostics
            .emit(DiagnosticEvent::ShadowIndexUpdate {
                files: total_files,
                chunks: total_chunks,
                duration_ms: start.elapsed().as_millis(),
            })
            .await;

        let msg = format!(
            "<shadow_index codebase_id=\"{}\" codebase_alias=\"{}\" root_path=\"{}\" files=\"{}\" chunks=\"{}\" ttl=\"{}\" base=\"{}\" />",
            xml_escape(&codebase.codebase_id),
            xml_escape(codebase.alias.as_deref().unwrap_or("")),
            xml_escape(&codebase.root_path),
            total_files,
            total_chunks,
            ttl_seconds,
            base_ref
        );
        info!("{msg}");
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    #[tool(
        description = "List indexed codebases, including codebase_id, alias, root path, last synced commit, and indexed status."
    )]
    async fn kt_list_codebases(
        &self,
        Parameters(_params): Parameters<ListCodebasesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.with_diagnostics("kt_list_codebases", || self.kt_list_codebases_inner())
            .await
    }

    async fn kt_list_codebases_inner(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ready().await.map_err(mcp_error)?;

        let storage = self.inner.storage.read().await;
        let codebases = storage.list_codebases().await.map_err(mcp_error)?;

        let mut xml = "<codebases>\n".to_string();
        for codebase in codebases {
            xml.push_str(&format!(
                "  <codebase codebase_id=\"{}\" codebase_alias=\"{}\" root_path=\"{}\" last_synced_commit=\"{}\" indexed=\"{}\" />\n",
                xml_escape(&codebase.codebase_id),
                xml_escape(codebase.alias.as_deref().unwrap_or("")),
                xml_escape(&codebase.root_path),
                xml_escape(codebase.last_synced_commit.as_deref().unwrap_or("")),
                codebase.indexed
            ));
        }
        xml.push_str("</codebases>");

        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Ask a high-level codebase question. The agentic RAG layer will plan and execute multiple retrieval steps to provide a grounded answer with citations."
    )]
    async fn kt_query(
        &self,
        Parameters(params): Parameters<QueryRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.with_diagnostics("kt_query", || self.kt_query_inner(params))
            .await
    }

    async fn kt_query_inner(
        &self,
        params: QueryRequest,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ready().await.map_err(mcp_error)?;

        let storage = self.inner.storage.read().await;
        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let planner = crate::agent::Planner;
        let plan = planner.plan(&params);

        let executor = crate::agent::AgentExecutor::new(Arc::new(storage.clone()), engine.clone());
        let response = executor.execute(&params, plan).await;

        let xml = format_query_response(&response);
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }
}

#[tool_router]
impl DebugKtServer {
    #[tool(
        description = "Search the indexed codebase using hybrid vector + keyword search. Use this to find code by semantic intent (e.g. 'how do we handle passwords') or exact names (e.g. 'BcryptHasher')."
    )]
    async fn kt_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.public
            .with_diagnostics("kt_search", || self.public.kt_search_inner(params))
            .await
    }

    #[tool(
        description = "Read the full contents of a file by its repository-relative path. Bypasses vector search and returns all indexed chunks for the file."
    )]
    async fn kt_read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.public
            .with_diagnostics("kt_read_file", || self.public.kt_read_file_inner(params))
            .await
    }

    #[tool(
        description = "Sync (index) a directory into the knowledge base. Parses all .rs, .go, and .java files using Tree-sitter, generates embeddings, and stores them in Redis for search."
    )]
    async fn kt_sync(
        &self,
        Parameters(params): Parameters<SyncParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.public
            .with_diagnostics("kt_sync", || self.public.kt_sync_inner(params))
            .await
    }

    #[tool(
        description = "Get git status information including current branch, commit SHA, and changed files."
    )]
    async fn kt_git_status(
        &self,
        Parameters(params): Parameters<GitStatusParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.public
            .with_diagnostics("kt_git_status", || self.public.kt_git_status_inner(params))
            .await
    }

    #[tool(
        description = "Index a pull request or working tree changes into the shadow (ephemeral) index. Only changed files are indexed. Shadow chunks auto-expire after TTL (default: 2 hours)."
    )]
    async fn kt_index_pr(
        &self,
        Parameters(params): Parameters<IndexPrParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.public
            .with_diagnostics("kt_index_pr", || self.public.kt_index_pr_inner(params))
            .await
    }

    #[tool(
        description = "List indexed codebases, including codebase_id, alias, root path, last synced commit, and indexed status."
    )]
    async fn kt_list_codebases(
        &self,
        Parameters(_params): Parameters<ListCodebasesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.public
            .with_diagnostics("kt_list_codebases", || {
                self.public.kt_list_codebases_inner()
            })
            .await
    }

    #[tool(
        description = "Ask a high-level codebase question. The agentic RAG layer will plan and execute multiple retrieval steps to provide a grounded answer with citations."
    )]
    async fn kt_query(
        &self,
        Parameters(params): Parameters<QueryRequest>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.public
            .with_diagnostics("kt_query", || self.public.kt_query_inner(params))
            .await
    }

    #[tool(
        description = "Experimental debug tool: report cached rust-analyzer LSP session status for a selected codebase root."
    )]
    async fn _debug_lsp_status(
        &self,
        Parameters(params): Parameters<DebugCodebaseParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let root = self
            .resolve_debug_root(
                params.directory_path.as_deref(),
                params.codebase_alias.as_deref(),
            )
            .await?;
        let statuses = self.lsp.status(Some(&root)).await;
        let xml = format_lsp_status(&root, &statuses);
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Experimental debug tool: ask rust-analyzer for the definition at a zero-based position in a Rust file."
    )]
    async fn _debug_lsp_definition(
        &self,
        Parameters(params): Parameters<DebugLspPositionParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let root = self
            .resolve_debug_root(
                params.directory_path.as_deref(),
                params.codebase_alias.as_deref(),
            )
            .await?;
        let filepath = resolve_file_path(&root, &params.filepath).map_err(mcp_error)?;
        let result = self
            .lsp
            .definition(&root, &filepath, params.line, params.character)
            .await
            .map_err(mcp_error)?;
        let xml = format_lsp_locations(
            "debug_lsp_definition",
            &root,
            &params.filepath,
            params.line,
            Some(params.character),
            &result,
        );
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Experimental debug tool: ask rust-analyzer for references at a zero-based position in a Rust file."
    )]
    async fn _debug_lsp_references(
        &self,
        Parameters(params): Parameters<DebugLspReferencesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let root = self
            .resolve_debug_root(
                params.directory_path.as_deref(),
                params.codebase_alias.as_deref(),
            )
            .await?;
        let filepath = resolve_file_path(&root, &params.filepath).map_err(mcp_error)?;
        let result = self
            .lsp
            .references(
                &root,
                &filepath,
                params.line,
                params.character,
                params.include_declaration.unwrap_or(true),
            )
            .await
            .map_err(mcp_error)?;
        let xml = format_lsp_locations(
            "debug_lsp_references",
            &root,
            &params.filepath,
            params.line,
            Some(params.character),
            &result,
        );
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Experimental debug tool: ask rust-analyzer for document symbols in a Rust file."
    )]
    async fn _debug_lsp_document_symbols(
        &self,
        Parameters(params): Parameters<DebugLspDocumentSymbolsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let root = self
            .resolve_debug_root(
                params.directory_path.as_deref(),
                params.codebase_alias.as_deref(),
            )
            .await?;
        let filepath = resolve_file_path(&root, &params.filepath).map_err(mcp_error)?;
        let result = self
            .lsp
            .document_symbols(&root, &filepath)
            .await
            .map_err(mcp_error)?;
        let xml = format_lsp_symbols(
            &root,
            &params.filepath,
            &result,
            debug_symbol_limit(params.max_symbols),
        );
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Experimental debug tool: show the indexed kt chunk containing a zero-based line in a file."
    )]
    async fn _debug_chunk_at(
        &self,
        Parameters(params): Parameters<DebugChunkAtParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let root = self
            .resolve_debug_root(
                params.directory_path.as_deref(),
                params.codebase_alias.as_deref(),
            )
            .await?;
        let codebase = self
            .resolve_debug_codebase(
                &root,
                params.directory_path.as_deref(),
                params.codebase_alias.as_deref(),
            )
            .await?;
        let relative = relative_path_for_root(&root, &params.filepath).map_err(mcp_error)?;
        let storage = self.public.inner.storage.read().await;
        let mut results = storage
            .read_file_chunks_scoped(&relative, Some(&codebase.codebase_id))
            .await
            .map_err(mcp_error)?;

        match storage
            .read_shadow_file_chunks_scoped(&relative, Some(&codebase.codebase_id))
            .await
        {
            Ok(shadow_results) => results.extend(shadow_results),
            Err(error) => {
                warn!(filepath = %relative, error = %error, "Debug chunk shadow read failed")
            }
        }

        let results = deduplicate_results(results);
        let chunk = chunk_at_line(&results, params.line)
            .ok_or_else(|| mcp_error(format!("No chunk found at {}:{}", relative, params.line)))?;
        let xml = format_debug_chunk_at(chunk, &relative, params.line);
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Experimental debug tool: append agent-written feedback about whether a debug/LSP result helped."
    )]
    async fn _debug_feedback(
        &self,
        Parameters(params): Parameters<DebugFeedbackParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let verdict = DebugFeedbackVerdict::parse(&params.verdict)
            .ok_or_else(|| mcp_error("verdict must be one of: helpful, not_helpful, bug, idea"))?;
        let (git_branch, git_commit) = current_git_identity().await;
        let record = DebugFeedbackRecord {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            kt_version: env!("CARGO_PKG_VERSION").to_string(),
            verdict,
            summary: params.summary,
            scenario: params.scenario,
            evidence: params.evidence,
            recommendation: params.recommendation,
            active_tool: params.active_tool,
            git_branch,
            git_commit,
        };

        append_feedback_record(&self.feedback_path, &record).map_err(mcp_error)?;
        let xml = format!(
            "<debug_feedback path=\"{}\" verdict=\"{}\" timestamp=\"{}\">recorded</debug_feedback>",
            xml_escape(&self.feedback_path.display().to_string()),
            record.verdict.as_str(),
            xml_escape(&record.timestamp)
        );
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Experimental debug tool: read recent agent-written debug feedback entries."
    )]
    async fn _debug_feedback_read(
        &self,
        Parameters(params): Parameters<DebugFeedbackReadParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let records =
            read_feedback_records(&self.feedback_path, params.limit).map_err(mcp_error)?;
        let xml = format_feedback_records(&self.feedback_path, &records);
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }
}

fn format_query_response(response: &QueryResponse) -> String {
    let status_str = match response.status {
        QueryStatus::Success => "success",
        QueryStatus::Partial => "partial",
        QueryStatus::Failure => "failure",
    };

    let mut xml = format!("<query_response status=\"{}\">\n", status_str);

    xml.push_str("  <answer>\n");
    xml.push_str(&xml_escape(&response.answer));
    xml.push_str("\n  </answer>\n");

    if !response.evidence.is_empty() {
        xml.push_str("  <evidence>\n");
        for citation in &response.evidence {
            xml.push_str(&format!(
                "    <citation filepath=\"{}\" start_line=\"{}\" end_line=\"{}\" symbol=\"{}\" />\n",
                xml_escape(&citation.filepath),
                citation.start_line.unwrap_or(0),
                citation.end_line.unwrap_or(0),
                xml_escape(citation.symbol.as_deref().unwrap_or(""))
            ));
        }
        xml.push_str("  </evidence>\n");
    }

    if !response.trace.is_empty() {
        xml.push_str("  <trace>\n");
        for step in &response.trace {
            xml.push_str(&format!(
                "    <step name=\"{}\" query=\"{}\" filepath=\"{}\" results=\"{}\" />\n",
                xml_escape(&step.name),
                xml_escape(step.query.as_deref().unwrap_or("")),
                xml_escape(step.filepath.as_deref().unwrap_or("")),
                step.results.unwrap_or(0)
            ));
        }
        xml.push_str("  </trace>\n");
    }

    if let Some(warning) = &response.warning {
        xml.push_str(&format!("  <warning>{}</warning>\n", xml_escape(warning)));
    }

    xml.push_str("</query_response>");
    xml
}

fn format_lsp_status(root: &Path, statuses: &[crate::debug_lsp::DebugLspStatus]) -> String {
    let mut xml = format!(
        "<debug_lsp_status root_path=\"{}\" cached_sessions=\"{}\">\n",
        xml_escape(&root.display().to_string()),
        statuses.len()
    );

    if statuses.is_empty() {
        xml.push_str("  <session running=\"false\" />\n");
    } else {
        for status in statuses {
            xml.push_str(&format!(
                "  <session root_path=\"{}\" analyzer=\"{}\" running=\"{}\" />\n",
                xml_escape(&status.root_path.display().to_string()),
                xml_escape(&status.analyzer),
                status.running
            ));
        }
    }

    xml.push_str("</debug_lsp_status>");
    xml
}

fn format_lsp_locations(
    tag: &str,
    root: &Path,
    filepath: &str,
    line: usize,
    character: Option<usize>,
    result: &serde_json::Value,
) -> String {
    let mut xml = format!(
        "<{} root_path=\"{}\" filepath=\"{}\" line=\"{}\" character=\"{}\">\n",
        tag,
        xml_escape(&root.display().to_string()),
        xml_escape(filepath),
        line,
        character.unwrap_or(0)
    );

    append_lsp_locations_xml(&mut xml, root, result);
    xml.push_str(&format!(
        "  <raw_json>{}</raw_json>\n",
        xml_escape(&serde_json::to_string(result).unwrap_or_else(|_| "null".to_string()))
    ));
    xml.push_str(&format!("</{}>", tag));
    xml
}

fn append_lsp_locations_xml(xml: &mut String, root: &Path, value: &serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                append_lsp_locations_xml(xml, root, value);
            }
        }
        serde_json::Value::Object(object) => {
            let uri = object
                .get("uri")
                .or_else(|| object.get("targetUri"))
                .and_then(serde_json::Value::as_str);
            let range = object.get("range").or_else(|| object.get("targetRange"));
            if let (Some(uri), Some(range)) = (uri, range) {
                append_lsp_location_xml(xml, root, uri, range);
            } else {
                for value in object.values() {
                    append_lsp_locations_xml(xml, root, value);
                }
            }
        }
        _ => {}
    }
}

fn append_lsp_location_xml(xml: &mut String, root: &Path, uri: &str, range: &serde_json::Value) {
    let filepath = file_uri_to_path(uri).ok();
    let relative = filepath
        .as_ref()
        .and_then(|path| path.strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .or_else(|| filepath.as_ref().map(|path| path.display().to_string()))
        .unwrap_or_default();

    let (start_line, start_character, end_line, end_character) = lsp_range_parts(range);
    xml.push_str(&format!(
        "  <location uri=\"{}\" filepath=\"{}\" start_line=\"{}\" start_character=\"{}\" end_line=\"{}\" end_character=\"{}\" />\n",
        xml_escape(uri),
        xml_escape(&relative),
        start_line,
        start_character,
        end_line,
        end_character
    ));
}

fn format_lsp_symbols(
    root: &Path,
    filepath: &str,
    result: &serde_json::Value,
    max_symbols: Option<usize>,
) -> String {
    let mut xml = format!(
        "<debug_lsp_document_symbols root_path=\"{}\" filepath=\"{}\">\n",
        xml_escape(&root.display().to_string()),
        xml_escape(filepath)
    );

    let symbols_total = count_lsp_symbols(result);
    let mut symbols_remaining = max_symbols.unwrap_or(usize::MAX);
    let mut symbols_returned = 0usize;

    if let serde_json::Value::Array(symbols) = result {
        for symbol in symbols {
            append_lsp_symbol_xml(
                &mut xml,
                symbol,
                1,
                &mut symbols_remaining,
                &mut symbols_returned,
            );
            if symbols_remaining == 0 {
                break;
            }
        }
    }

    if symbols_returned < symbols_total {
        xml.push_str(&format!(
            "  <truncated symbols_returned=\"{}\" symbols_total=\"{}\" />\n",
            symbols_returned, symbols_total
        ));
        xml.push_str("  <raw_json omitted=\"true\" reason=\"symbol_output_truncated\" />\n");
    } else {
        xml.push_str(&format!(
            "  <raw_json>{}</raw_json>\n",
            xml_escape(&serde_json::to_string(result).unwrap_or_else(|_| "null".to_string()))
        ));
    }
    xml.push_str("</debug_lsp_document_symbols>");
    xml
}

fn debug_symbol_limit(max_symbols: Option<usize>) -> Option<usize> {
    const DEFAULT_DEBUG_SYMBOL_LIMIT: usize = 80;

    match max_symbols {
        Some(0) => None,
        Some(value) => Some(value),
        None => Some(DEFAULT_DEBUG_SYMBOL_LIMIT),
    }
}

fn count_lsp_symbols(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(symbols) => symbols.iter().map(count_lsp_symbols).sum(),
        serde_json::Value::Object(symbol) => {
            1 + symbol
                .get("children")
                .map(count_lsp_symbols)
                .unwrap_or_default()
        }
        _ => 0,
    }
}

fn append_lsp_symbol_xml(
    xml: &mut String,
    symbol: &serde_json::Value,
    depth: usize,
    symbols_remaining: &mut usize,
    symbols_returned: &mut usize,
) {
    if *symbols_remaining == 0 {
        return;
    }

    *symbols_remaining -= 1;
    *symbols_returned += 1;

    let indent = "  ".repeat(depth);
    let name = symbol
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let detail = symbol
        .get("detail")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let kind = symbol
        .get("kind")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let range = symbol
        .get("range")
        .or_else(|| symbol.pointer("/location/range"));
    let (start_line, start_character, end_line, end_character) =
        range.map(lsp_range_parts).unwrap_or((0, 0, 0, 0));

    xml.push_str(&format!(
        "{indent}<symbol name=\"{}\" detail=\"{}\" kind=\"{}\" start_line=\"{}\" start_character=\"{}\" end_line=\"{}\" end_character=\"{}\">\n",
        xml_escape(name),
        xml_escape(detail),
        kind,
        start_line,
        start_character,
        end_line,
        end_character
    ));

    if let Some(children) = symbol.get("children").and_then(serde_json::Value::as_array) {
        for child in children {
            append_lsp_symbol_xml(xml, child, depth + 1, symbols_remaining, symbols_returned);
            if *symbols_remaining == 0 {
                break;
            }
        }
    }

    xml.push_str(&format!("{indent}</symbol>\n"));
}

fn lsp_range_parts(range: &serde_json::Value) -> (u64, u64, u64, u64) {
    let start_line = range
        .pointer("/start/line")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let start_character = range
        .pointer("/start/character")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let end_line = range
        .pointer("/end/line")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let end_character = range
        .pointer("/end/character")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    (start_line, start_character, end_line, end_character)
}

fn chunk_at_line(results: &[SearchResult], line: usize) -> Option<&SearchResult> {
    results
        .iter()
        .filter(|result| match (result.start_line, result.end_line) {
            (Some(start), Some(end)) => start <= line && line <= end,
            _ => false,
        })
        .min_by_key(|result| {
            let start = result.start_line.unwrap_or(0);
            let end = result.end_line.unwrap_or(usize::MAX);
            (end.saturating_sub(start), start, &result.chunk_id)
        })
}

fn format_debug_chunk_at(result: &SearchResult, filepath: &str, line: usize) -> String {
    let line_attrs = match (result.start_line, result.end_line) {
        (Some(start), Some(end)) => format!(" start_line=\"{}\" end_line=\"{}\"", start, end),
        _ => String::new(),
    };

    format!(
        "<debug_chunk_at filepath=\"{}\" line=\"{}\">\n  <chunk codebase_id=\"{}\" codebase_alias=\"{}\" root_path=\"{}\" type=\"{}\" name=\"{}\" signature=\"{}\"{}>\n    {}\n  </chunk>\n</debug_chunk_at>",
        xml_escape(filepath),
        line,
        xml_escape(&result.codebase_id),
        xml_escape(result.codebase_alias.as_deref().unwrap_or("")),
        xml_escape(&result.root_path),
        xml_escape(&result.node_type),
        xml_escape(&result.name),
        xml_escape(&result.signature),
        line_attrs,
        xml_escape(&result.content)
    )
}

fn format_feedback_records(path: &Path, records: &[DebugFeedbackRecord]) -> String {
    let mut xml = format!(
        "<debug_feedback_entries path=\"{}\" count=\"{}\">\n",
        xml_escape(&path.display().to_string()),
        records.len()
    );

    for record in records {
        xml.push_str(&format!(
            "  <entry timestamp=\"{}\" verdict=\"{}\" kt_version=\"{}\" active_tool=\"{}\" git_branch=\"{}\" git_commit=\"{}\" scenario=\"{}\">\n    <summary>{}</summary>\n    <evidence>{}</evidence>\n    <recommendation>{}</recommendation>\n  </entry>\n",
            xml_escape(&record.timestamp),
            record.verdict.as_str(),
            xml_escape(&record.kt_version),
            xml_escape(record.active_tool.as_deref().unwrap_or("")),
            xml_escape(record.git_branch.as_deref().unwrap_or("")),
            xml_escape(record.git_commit.as_deref().unwrap_or("")),
            xml_escape(record.scenario.as_deref().unwrap_or("")),
            xml_escape(&record.summary),
            xml_escape(record.evidence.as_deref().unwrap_or("")),
            xml_escape(record.recommendation.as_deref().unwrap_or(""))
        ));
    }

    xml.push_str("</debug_feedback_entries>");
    xml
}

async fn current_git_identity() -> (Option<String>, Option<String>) {
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(_) => return (None, None),
    };

    tokio::task::spawn_blocking(move || git::get_git_info(&root))
        .await
        .ok()
        .and_then(Result::ok)
        .map(|info| (info.branch, info.commit_sha))
        .unwrap_or((None, None))
}

#[tool_handler]
impl ServerHandler for KtServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_server_info(Implementation::new("kt", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "kt (Knowledge Transfer) - A local multi-codebase RAG system. Use kt_search for semantic code search, kt_read_file to read specific files across codebases, kt_sync to index/update a directory, kt_git_status for repository status, kt_index_pr for shadow-indexing branch changes, kt_list_codebases to discover aliases and roots, and kt_query for high-level abstract questions. Scope search/read/query with directory_path or codebase_alias when needed.".to_string(),
        )
    }
}

#[tool_handler]
impl ServerHandler for DebugKtServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("kt-debug", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "kt debug mode - public kt tools plus experimental _debug_* tools for rust-analyzer LSP dogfooding and feedback capture. Debug tools are live-only and do not enrich Redis chunks.".to_string(),
            )
    }
}

pub async fn run_server(config: Config) -> anyhow::Result<()> {
    init_tracing();

    let server = KtServer::new(config)?;
    server.ensure_ready().await?;

    info!("Starting kt MCP server on stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}

pub async fn run_debug_server(config: Config) -> anyhow::Result<()> {
    init_tracing();

    let server = DebugKtServer::new(config)?;

    info!("Starting kt debug MCP server on stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

fn validate_directory_path(path: &str) -> Result<std::path::PathBuf, rmcp::ErrorData> {
    let path = std::path::PathBuf::from(path);
    if !path.exists() {
        return Err(mcp_error(format!(
            "Directory not found: {}",
            path.display()
        )));
    }

    // Canonicalize to resolve .. and symlinks
    let canonical = path
        .canonicalize()
        .map_err(|e| mcp_error(format!("Failed to resolve path: {e}")))?;

    if !canonical.is_dir() {
        return Err(mcp_error(format!(
            "Path is not a directory: {}",
            canonical.display()
        )));
    }

    // Basic safety: block root and common system top-level dirs on Unix
    #[cfg(unix)]
    {
        if canonical.parent().is_none() {
            return Err(mcp_error(format!(
                "Access to root directory denied: {}",
                canonical.display()
            )));
        }

        let forbidden = ["/bin", "/sbin", "/boot", "/dev", "/root", "/sys", "/proc"];
        for f in forbidden {
            if canonical.starts_with(f) {
                return Err(mcp_error(format!(
                    "Access to system directory denied: {}",
                    canonical.display()
                )));
            }
        }
    }

    Ok(canonical)
}

async fn resolve_codebase_selector(
    storage: &Storage,
    directory_path: Option<&str>,
    codebase_alias: Option<&str>,
) -> Result<Option<crate::Codebase>, rmcp::ErrorData> {
    let directory = match directory_path {
        Some(path) => Some(validate_directory_path(path)?),
        None => None,
    };
    storage
        .resolve_codebase(directory.as_deref(), codebase_alias)
        .await
        .map_err(mcp_error)
}

fn deduplicate_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    results
        .into_iter()
        .filter(|r| seen.insert(r.chunk_id.clone()))
        .collect()
}

fn merge_and_deduplicate(
    main_results: Vec<SearchResult>,
    shadow_results: Vec<SearchResult>,
    top_k: usize,
) -> Vec<SearchResult> {
    let mut all_results = Vec::with_capacity(main_results.len() + shadow_results.len());
    all_results.extend(main_results);
    all_results.extend(shadow_results);

    all_results.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen = HashSet::new();
    let mut deduped = all_results
        .into_iter()
        .filter(|r| seen.insert(r.chunk_id.clone()))
        .collect::<Vec<_>>();

    if deduped.len() > top_k {
        deduped.truncate(top_k);
    }

    deduped
}

async fn resolve_one_hop_context(
    storage: &Storage,
    results: &[SearchResult],
) -> std::collections::HashMap<(String, String), SearchResult> {
    let mut context_map = std::collections::HashMap::new();
    let mut needed_by_codebase: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for result in results {
        if let Some(ref parent_ctx) = result.parent_context {
            if let Some(name) = extract_parent_type_name(parent_ctx) {
                let key = (result.codebase_id.clone(), name.clone());
                if !context_map.contains_key(&key) {
                    let names = needed_by_codebase
                        .entry(result.codebase_id.clone())
                        .or_default();
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
        }
    }

    if needed_by_codebase.is_empty() {
        return context_map;
    }

    for (codebase_id, needed_names) in needed_by_codebase {
        match storage
            .lookup_chunks_by_name_scoped(&needed_names, Some(&codebase_id))
            .await
        {
            Ok(parent_results) => {
                for pr in parent_results {
                    context_map.insert((pr.codebase_id.clone(), pr.name.clone()), pr);
                }
            }
            Err(e) => {
                warn!("Failed to resolve one-hop context: {e}");
            }
        }
    }

    context_map
}

async fn resolve_call_context(
    results: &[SearchResult],
    storage: &Storage,
    codebase_id: Option<&str>,
) -> Vec<SearchResult> {
    const MAX_CALL_CONTEXT_NAMES: usize = 5;
    const MAX_RELATED_CALL_CONTEXT: usize = 2;

    let result_names: HashSet<String> = results.iter().map(|r| r.name.clone()).collect();

    let mut call_names: Vec<String> = Vec::new();
    for result in results {
        for call in &result.calls {
            if !result_names.contains(&call.name) && !call_names.contains(&call.name) {
                call_names.push(call.name.clone());
            }
        }
    }

    if call_names.len() > MAX_CALL_CONTEXT_NAMES {
        call_names.truncate(MAX_CALL_CONTEXT_NAMES);
    }

    if call_names.is_empty() {
        return Vec::new();
    }

    match storage
        .lookup_chunks_by_name_scoped(&call_names, codebase_id)
        .await
    {
        Ok(related) => related
            .into_iter()
            .filter(|r| !result_names.contains(&r.name))
            .take(MAX_RELATED_CALL_CONTEXT)
            .collect(),
        Err(e) => {
            tracing::warn!("Failed to resolve call context for {:?}: {e}", call_names);
            Vec::new()
        }
    }
}

fn extract_parent_type_name(context: &str) -> Option<String> {
    for line in context.lines() {
        let trimmed = line.trim();

        if let Some(after_impl) = trimmed.strip_prefix("impl") {
            if let Some(for_pos) = after_impl.find(" for ") {
                let impl_target = after_impl[..for_pos].trim();
                if !impl_target.is_empty() {
                    let name = impl_target
                        .split(|c: char| c.is_whitespace() || c == '<')
                        .next_back()
                        .unwrap_or("")
                        .trim()
                        .trim_start_matches('&')
                        .to_string();
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
                let after_for = after_impl[for_pos + 5..].trim();
                let name = after_for
                    .split(|c: char| c.is_whitespace() || c == '<' || c == '{')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            } else {
                let after_impl = after_impl.trim();
                let name = after_impl
                    .split(|c: char| c.is_whitespace() || c == '<' || c == '{')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_start_matches('<')
                    .to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("type ") {
            let name = rest
                .split(|c: char| c.is_whitespace() || c == '{')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }

        if let Some(rest) = trimmed.strip_prefix("class ") {
            let name = rest
                .split(|c: char| c.is_whitespace() || c == '{' || c == '<')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }

        if let Some(rest) = trimmed.strip_prefix("interface ") {
            let name = rest
                .split(|c: char| c.is_whitespace() || c == '{' || c == '<')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

const MAX_CONTENT_CHARS: usize = 8000;
const MAX_RESPONSE_CHARS: usize = 32000;

fn truncate_content(content: &str) -> &str {
    if content.chars().count() <= MAX_CONTENT_CHARS {
        return content;
    }
    match content.char_indices().nth(MAX_CONTENT_CHARS) {
        Some((idx, _)) => &content[..idx],
        None => content,
    }
}

fn format_search_results(
    results: &[SearchResult],
    query: &str,
    headers_only: bool,
    one_hop: &std::collections::HashMap<(String, String), SearchResult>,
    related: &[SearchResult],
    warnings: &[ToolWarning],
) -> String {
    let mut xml = format!("<search_results query=\"{}\">\n", xml_escape(query));
    let mut total_len = 0usize;
    append_warnings_xml(&mut xml, warnings);

    for result in results {
        let content = if headers_only {
            result.signature.clone()
        } else {
            truncate_content(&result.content).to_string()
        };

        let parent_xml = if let Some(ref parent_ctx) = result.parent_context {
            let parent_key =
                extract_parent_type_name(parent_ctx).map(|name| (result.codebase_id.clone(), name));
            if let Some(parent_result) = parent_key.as_ref().and_then(|key| one_hop.get(key)) {
                format!(
                    "  <parent_struct>\n    {}\n  </parent_struct>\n",
                    xml_escape(&parent_result.content)
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let chunk_xml = format!(
            "  <chunk codebase_id=\"{}\" codebase_alias=\"{}\" root_path=\"{}\" filepath=\"{}\" language=\"{}\" type=\"{}\" name=\"{}\" signature=\"{}\" score=\"{:.4}\">\n{}    {}\n  </chunk>\n",
            xml_escape(&result.codebase_id),
            xml_escape(result.codebase_alias.as_deref().unwrap_or("")),
            xml_escape(&result.root_path),
            xml_escape(&result.filepath),
            result.language,
            xml_escape(&result.node_type),
            xml_escape(&result.name),
            xml_escape(&result.signature),
            result.score,
            parent_xml,
            xml_escape(&content),
        );
        total_len += chunk_xml.len();
        if total_len > MAX_RESPONSE_CHARS {
            break;
        }
        xml.push_str(&chunk_xml);
    }
    if !related.is_empty() {
        xml.push_str("  <related_chunks>\n");
        for rc in related {
            xml.push_str(&format!(
                "    <chunk name=\"{}\" filepath=\"{}\" signature=\"{}\" score=\"{}\" />\n",
                xml_escape(&rc.name),
                xml_escape(&rc.filepath),
                xml_escape(&rc.signature),
                rc.score,
            ));
        }
        xml.push_str("  </related_chunks>\n");
    }
    xml.push_str("</search_results>");
    xml
}

fn format_file_results(
    results: &[SearchResult],
    filepath: &str,
    headers_only: bool,
    warnings: &[ToolWarning],
) -> String {
    let mut xml = format!("<files filepath=\"{}\">\n", xml_escape(filepath));
    append_warnings_xml(&mut xml, warnings);
    let mut ordered = results.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        (&a.codebase_id, a.start_line, a.end_line, &a.chunk_id).cmp(&(
            &b.codebase_id,
            b.start_line,
            b.end_line,
            &b.chunk_id,
        ))
    });

    let mut current_codebase: Option<&str> = None;
    for result in ordered {
        if current_codebase != Some(result.codebase_id.as_str()) {
            if current_codebase.is_some() {
                xml.push_str("  </codebase>\n");
            }
            current_codebase = Some(result.codebase_id.as_str());
            xml.push_str(&format!(
                "  <codebase codebase_id=\"{}\" codebase_alias=\"{}\" root_path=\"{}\">\n",
                xml_escape(&result.codebase_id),
                xml_escape(result.codebase_alias.as_deref().unwrap_or("")),
                xml_escape(&result.root_path),
            ));
        }
        let content = if headers_only {
            xml_escape(&result.signature)
        } else {
            xml_escape(&result.content)
        };
        let line_attrs = match (result.start_line, result.end_line) {
            (Some(start_line), Some(end_line)) => {
                format!(" start_line=\"{}\" end_line=\"{}\"", start_line, end_line)
            }
            _ => String::new(),
        };
        xml.push_str(&format!(
            "    <chunk type=\"{}\" name=\"{}\" signature=\"{}\"{}>\n      {}\n    </chunk>\n",
            xml_escape(&result.node_type),
            xml_escape(&result.name),
            xml_escape(&result.signature),
            line_attrs,
            content,
        ));
    }
    if current_codebase.is_some() {
        xml.push_str("  </codebase>\n");
    }
    xml.push_str("</files>");
    xml
}

fn append_warnings_xml(xml: &mut String, warnings: &[ToolWarning]) {
    if warnings.is_empty() {
        return;
    }

    xml.push_str("  <warnings>\n");
    for warning in warnings {
        xml.push_str(&format!(
            "    <warning source=\"{}\">{}</warning>\n",
            xml_escape(warning.source),
            xml_escape(&warning.message)
        ));
    }
    xml.push_str("  </warnings>\n");
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_result(chunk_id: &str, score: f64) -> SearchResult {
        SearchResult {
            chunk_id: chunk_id.to_string(),
            codebase_id: "codebase-a".to_string(),
            codebase_alias: Some("alpha".to_string()),
            root_path: "/tmp/alpha".to_string(),
            filepath: "src/example.rs".to_string(),
            language: Language::Rust,
            node_type: "function".to_string(),
            name: format!("name_{chunk_id}"),
            signature: String::new(),
            content: String::new(),
            parent_context: None,
            score,
            start_line: None,
            end_line: None,
            file_role: crate::FileRole::Implementation,
            calls: Vec::new(),
        }
    }

    fn file_result(
        chunk_id: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> SearchResult {
        SearchResult {
            chunk_id: chunk_id.to_string(),
            codebase_id: "codebase-a".to_string(),
            codebase_alias: Some("alpha".to_string()),
            root_path: "/tmp/alpha".to_string(),
            filepath: "src/example.rs".to_string(),
            language: Language::Rust,
            node_type: "function".to_string(),
            name: format!("name_{chunk_id}"),
            signature: format!("fn name_{chunk_id}()"),
            content: format!("fn name_{chunk_id}() {{}}"),
            parent_context: None,
            score: 0.0,
            start_line,
            end_line,
            file_role: crate::FileRole::Implementation,
            calls: Vec::new(),
        }
    }

    #[test]
    fn test_merge_and_deduplicate_sorts_and_truncates() {
        let main = vec![
            sample_result("a", 0.7),
            sample_result("b", 0.2),
            sample_result("d", 0.1),
        ];
        let shadow = vec![sample_result("c", 0.4), sample_result("e", 0.3)];

        let merged = merge_and_deduplicate(main, shadow, 3);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].chunk_id, "d");
        assert_eq!(merged[1].chunk_id, "b");
        assert_eq!(merged[2].chunk_id, "e");
    }

    #[test]
    fn test_merge_and_deduplicate_deduplicates_chunk_ids() {
        let main = vec![
            sample_result("a", 0.8),
            sample_result("a", 0.1),
            sample_result("b", 0.9),
        ];
        let shadow = vec![sample_result("c", 0.5), sample_result("c", 0.2)];

        let merged = merge_and_deduplicate(main, shadow, 10);
        assert_eq!(merged.len(), 3);

        let ids = merged
            .iter()
            .map(|r| r.chunk_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["a", "c", "b"]);
    }

    #[test]
    fn test_format_file_results_orders_by_line_range_and_emits_line_attributes() {
        let results = vec![
            file_result("later", Some(10), Some(12)),
            file_result("earlier", Some(2), Some(4)),
        ];

        let xml = format_file_results(&results, "src/example.rs", false, &[]);

        let earlier = xml.find("name_earlier").unwrap();
        let later = xml.find("name_later").unwrap();
        assert!(earlier < later);
        assert!(xml.contains("<files filepath=\"src/example.rs\">"));
        assert!(xml.contains("codebase_id=\"codebase-a\""));
        assert!(xml.contains("codebase_alias=\"alpha\""));
        assert!(xml.contains("root_path=\"/tmp/alpha\""));
        assert!(xml.contains("start_line=\"2\" end_line=\"4\""));
        assert!(xml.contains("start_line=\"10\" end_line=\"12\""));
    }

    #[test]
    fn test_format_search_results_emits_codebase_metadata() {
        let results = vec![sample_result("result", 0.1)];
        let one_hop = std::collections::HashMap::new();

        let related: Vec<SearchResult> = Vec::new();
        let xml = format_search_results(&results, "query", false, &one_hop, &related, &[]);

        assert!(xml.contains("codebase_id=\"codebase-a\""));
        assert!(xml.contains("codebase_alias=\"alpha\""));
        assert!(xml.contains("root_path=\"/tmp/alpha\""));
        assert!(xml.contains("filepath=\"src/example.rs\""));
    }

    #[test]
    fn test_format_search_results_emits_shadow_warnings() {
        let one_hop = std::collections::HashMap::new();
        let related: Vec<SearchResult> = Vec::new();
        let warnings = vec![ToolWarning::shadow_index(
            "Shadow search failed: redis <down> & retry",
        )];

        let xml = format_search_results(&[], "query", false, &one_hop, &related, &warnings);

        assert!(xml.contains(
            "<warning source=\"shadow_index\">Shadow search failed: redis &lt;down&gt; &amp; retry</warning>"
        ));
    }

    #[test]
    fn test_format_file_results_emits_shadow_warnings() {
        let warnings = vec![ToolWarning::shadow_index(
            "Shadow file read failed: redis <down> & retry",
        )];

        let xml = format_file_results(&[], "src/example.rs", false, &warnings);

        assert!(xml.contains(
            "<warning source=\"shadow_index\">Shadow file read failed: redis &lt;down&gt; &amp; retry</warning>"
        ));
    }

    #[test]
    fn test_format_lsp_symbols_truncates_large_outputs() {
        let symbols = serde_json::Value::Array(vec![
            serde_json::json!({
                "name": "first",
                "kind": 12,
                "range": {
                    "start": {"line": 1, "character": 2},
                    "end": {"line": 3, "character": 4}
                }
            }),
            serde_json::json!({
                "name": "second",
                "kind": 12,
                "range": {
                    "start": {"line": 5, "character": 6},
                    "end": {"line": 7, "character": 8}
                }
            }),
        ]);

        let xml = format_lsp_symbols(
            std::path::Path::new("/repo"),
            "src/lib.rs",
            &symbols,
            Some(1),
        );

        assert!(xml.contains("name=\"first\""));
        assert!(!xml.contains("name=\"second\""));
        assert!(xml.contains("<truncated symbols_returned=\"1\" symbols_total=\"2\" />"));
    }

    #[test]
    fn test_format_lsp_locations_recurses_nested_results() {
        let result = serde_json::json!({
            "items": [{
                "uri": "file:///repo/src/lib.rs",
                "range": {
                    "start": {"line": 2, "character": 4},
                    "end": {"line": 2, "character": 9}
                }
            }]
        });

        let xml = format_lsp_locations(
            "debug_lsp_definition",
            std::path::Path::new("/repo"),
            "src/main.rs",
            1,
            Some(2),
            &result,
        );

        assert!(xml.contains("filepath=\"src/lib.rs\""));
        assert!(xml.contains("start_line=\"2\" start_character=\"4\""));
    }

    #[test]
    fn test_chunk_at_line_selects_narrowest_matching_range() {
        let results = vec![
            file_result("outer", Some(10), Some(30)),
            file_result("inner", Some(12), Some(14)),
            file_result("other", Some(40), Some(42)),
        ];

        let chunk = chunk_at_line(&results, 13).unwrap();

        assert_eq!(chunk.chunk_id, "inner");
    }
}
