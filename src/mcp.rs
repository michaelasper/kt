use crate::embedding::EmbeddingEngine;
use crate::git;
use crate::storage::Storage;
use crate::{Config, Language, SearchResult};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, transport::stdio, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashSet;
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

#[derive(Debug)]
struct KtServerInner {
    storage: RwLock<Storage>,
    embedding: RwLock<Option<Arc<EmbeddingEngine>>>,
    config: Config,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    #[schemars(description = "The search query - natural language or code terms")]
    pub query: String,
    #[schemars(description = "Filter by language: rust, go, or java")]
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

impl KtServer {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let storage = Storage::new(&config)?;
        Ok(Self {
            inner: Arc::new(KtServerInner {
                storage: RwLock::new(storage),
                embedding: RwLock::new(None),
                config,
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

#[tool_router]
impl KtServer {
    #[tool(
        description = "Search the indexed codebase using hybrid vector + keyword search. Use this to find code by semantic intent (e.g. 'how do we hash passwords') or exact names (e.g. 'BcryptHasher')."
    )]
    async fn kt_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ready().await.map_err(mcp_error)?;

        let top_k = params.top_k.unwrap_or(3).min(10);
        let headers_only = params.headers_only.unwrap_or(false);
        let language = params.language.as_deref().and_then(Language::parse);

        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let query_embedding = engine.embed(&params.query).map_err(mcp_error)?;

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

        let one_hop = resolve_one_hop_context(&storage, &results).await;

        let xml = format_search_results(&results, &params.query, headers_only, &one_hop, &warnings);
        Ok(CallToolResult::success(vec![Content::text(xml)]))
    }

    #[tool(
        description = "Read the full contents of a file by its repository-relative path. Bypasses vector search and returns all indexed chunks for the file."
    )]
    async fn kt_read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
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
            let xml = if warnings.is_empty() {
                format!(
                    "<error>No chunks found for file: {}</error>",
                    xml_escape(&params.filepath)
                )
            } else {
                format_file_not_found(&params.filepath, &warnings)
            };
            return Ok(CallToolResult::success(vec![Content::text(xml)]));
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
        self.ensure_ready().await.map_err(mcp_error)?;

        let root = std::path::Path::new(&params.directory_path);
        if !root.exists() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "<error>Directory not found: {}</error>",
                params.directory_path
            ))]));
        }

        let full = params.full.unwrap_or(false);
        info!(
            "Starting sync for {} (full: {})",
            params.directory_path, full
        );

        let storage = self.inner.storage.read().await;
        let codebase = storage
            .register_codebase(root, params.codebase_alias.as_deref())
            .await
            .map_err(mcp_error)?;
        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let discovery_options = self.inner.config.discovery_options();
        let plan =
            crate::sync::plan_with_options(root, &storage, &codebase, full, &discovery_options)
                .await
                .map_err(mcp_error)?;

        if plan.files.is_empty() {
            crate::sync::finalize(root, &codebase, &plan.strategy, &storage)
                .await
                .map_err(mcp_error)?;
            return Ok(CallToolResult::success(vec![Content::text(
                "<result>No supported files found to sync</result>".to_string(),
            )]));
        }

        let strategy = plan.strategy.clone();
        let progress = Arc::new(tokio::sync::Mutex::new(crate::sync::NoopProgress));
        let stats = crate::sync::execute(plan, &codebase, &storage, engine.clone(), progress)
            .await
            .map_err(mcp_error)?;

        crate::sync::finalize(root, &codebase, &strategy, &storage)
            .await
            .map_err(mcp_error)?;

        let msg = format!(
            "<result codebase_id=\"{}\" codebase_alias=\"{}\" root_path=\"{}\">Sync complete: {} files, {} chunks indexed, {} errors</result>",
            xml_escape(&codebase.codebase_id),
            xml_escape(codebase.alias.as_deref().unwrap_or("")),
            xml_escape(&codebase.root_path),
            stats.total_files, stats.total_chunks, stats.errors
        );
        info!("{msg}");
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    #[tool(
        description = "Get git status information including current branch, commit SHA, and changed files."
    )]
    async fn kt_git_status(
        &self,
        Parameters(params): Parameters<GitStatusParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let root = std::path::Path::new(&params.directory_path);
        let git_info = git::get_git_info(root).map_err(mcp_error)?;

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
        self.ensure_ready().await.map_err(mcp_error)?;

        let root = std::path::Path::new(&params.directory_path);
        if !root.exists() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "<error>Directory not found: {}</error>",
                params.directory_path
            ))]));
        }

        let storage = self.inner.storage.read().await;
        let codebase = storage
            .register_codebase(root, None)
            .await
            .map_err(mcp_error)?;
        storage.ensure_shadow_index().await.map_err(mcp_error)?;

        let base_ref = params.base_branch.as_deref().unwrap_or("main");
        let ttl_seconds = params.ttl_seconds.unwrap_or(7200);

        let changed_files = git::get_worktree_diff_files(root, base_ref).map_err(mcp_error)?;

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
            let chunks = crate::indexing::parse_file_async(
                file_path,
                filepath.to_string(),
                language,
                codebase.codebase_id.clone(),
            )
            .await;

            if chunks.is_empty() {
                continue;
            }

            let texts: Vec<String> = chunks
                .iter()
                .map(crate::embedding::chunk_embedding_text)
                .collect();
            let engine_clone = engine.clone();
            let embeddings_result = tokio::task::spawn_blocking(move || {
                let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                engine_clone.embed_batch(&text_refs)
            })
            .await;

            match embeddings_result {
                Ok(Ok(embeddings)) => {
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
                Ok(Err(e)) => {
                    warn!("Failed to embed chunks for {}: {e}", filepath);
                }
                Err(e) => {
                    warn!("Task join error during embedding: {e}");
                }
            }
        }

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
}

#[tool_handler]
impl ServerHandler for KtServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_server_info(Implementation::new("kt", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "kt (Knowledge Transfer) - A local multi-codebase RAG system. Use kt_search for global semantic code search, kt_read_file to read specific files across codebases, kt_sync to index/update a directory, and kt_list_codebases to discover aliases and roots. Scope search/read with directory_path or codebase_alias when needed.".to_string(),
        )
    }
}

pub async fn run_server(config: Config) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let server = KtServer::new(config)?;
    server.ensure_ready().await?;

    info!("Starting kt MCP server on stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}

async fn resolve_codebase_selector(
    storage: &Storage,
    directory_path: Option<&str>,
    codebase_alias: Option<&str>,
) -> anyhow::Result<Option<crate::Codebase>> {
    let directory = directory_path.map(std::path::Path::new);
    storage.resolve_codebase(directory, codebase_alias).await
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

fn format_file_not_found(filepath: &str, warnings: &[ToolWarning]) -> String {
    let mut xml = format!("<files filepath=\"{}\">\n", xml_escape(filepath));
    append_warnings_xml(&mut xml, warnings);
    xml.push_str(&format!(
        "  <error>No chunks found for file: {}</error>\n",
        xml_escape(filepath)
    ));
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

        let xml = format_search_results(&results, "query", false, &one_hop, &[]);

        assert!(xml.contains("codebase_id=\"codebase-a\""));
        assert!(xml.contains("codebase_alias=\"alpha\""));
        assert!(xml.contains("root_path=\"/tmp/alpha\""));
        assert!(xml.contains("filepath=\"src/example.rs\""));
    }

    #[test]
    fn test_format_search_results_emits_shadow_warnings() {
        let one_hop = std::collections::HashMap::new();
        let warnings = vec![ToolWarning::shadow_index(
            "Shadow search failed: redis <down> & retry",
        )];

        let xml = format_search_results(&[], "query", false, &one_hop, &warnings);

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
    fn test_format_file_not_found_preserves_shadow_warnings() {
        let warnings = vec![ToolWarning::shadow_index(
            "Shadow file read failed: redis down",
        )];

        let xml = format_file_not_found("src/example.rs", &warnings);

        assert!(xml.contains(
            "<warning source=\"shadow_index\">Shadow file read failed: redis down</warning>"
        ));
        assert!(xml.contains("<error>No chunks found for file: src/example.rs</error>"));
    }
}
