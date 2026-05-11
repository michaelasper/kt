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

#[derive(Debug, Clone)]
pub struct KtServer {
    inner: Arc<KtServerInner>,
}

#[derive(Debug)]
struct KtServerInner {
    storage: RwLock<Storage>,
    embedding: RwLock<Option<EmbeddingEngine>>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadFileParams {
    #[schemars(description = "Repository-relative path to the file")]
    pub filepath: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncParams {
    #[schemars(description = "Path to the directory to sync")]
    pub directory_path: String,
    #[schemars(description = "Force full re-index instead of partial sync")]
    pub full: Option<bool>,
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
                *embedding = Some(engine);
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
        let language = params.language.as_deref().and_then(parse_language);

        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let query_embedding = engine.embed(&params.query).map_err(mcp_error)?;

        let storage = self.inner.storage.read().await;

        let main_results = storage
            .hybrid_search(&query_embedding, &params.query, language.as_ref(), top_k)
            .await
            .map_err(mcp_error)?;

        let shadow_results = storage
            .search_shadow(&query_embedding, &params.query, language.as_ref(), top_k)
            .await
            .unwrap_or_default();

        let shadow_ids: std::collections::HashSet<String> =
            shadow_results.iter().map(|r| r.chunk_id.clone()).collect();

        let filtered_main: Vec<SearchResult> = main_results
            .into_iter()
            .filter(|r| !shadow_ids.contains(&r.chunk_id))
            .collect();

        let results = merge_and_deduplicate(filtered_main, shadow_results, top_k);

        let one_hop = resolve_one_hop_context(&storage, &results).await;

        let xml = format_search_results(&results, &params.query, headers_only, &one_hop);
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

        let results = storage
            .read_shadow_file_chunks(&params.filepath)
            .await
            .unwrap_or_default();

        let results = if !results.is_empty() {
            results
        } else {
            storage
                .read_file_chunks(&params.filepath)
                .await
                .map_err(mcp_error)?
        };

        if results.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "<error>No chunks found for file: {}</error>",
                params.filepath
            ))]));
        }

        let results = deduplicate_results(results);
        let xml = format_file_results(&results, &params.filepath, false);
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
        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let plan = crate::sync::plan(root, &storage, full)
            .await
            .map_err(mcp_error)?;

        if plan.files.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "<result>No supported files found to sync</result>".to_string(),
            )]));
        }

        let mut progress = crate::sync::NoopProgress;
        let stats = crate::sync::execute(&plan, &storage, engine, &mut progress)
            .await
            .map_err(mcp_error)?;

        crate::sync::finalize(root, &plan.strategy, &storage)
            .await
            .map_err(mcp_error)?;

        let msg = format!(
            "<result>Sync complete: {} files, {} chunks indexed, {} errors</result>",
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
        storage.ensure_shadow_index().await.map_err(mcp_error)?;

        let base_ref = params.base_branch.as_deref().unwrap_or("main");
        let ttl_seconds = params.ttl_seconds.unwrap_or(7200);

        let changed_files = git::get_diff_files(root, base_ref).map_err(mcp_error)?;

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

        for filepath in &changed_files {
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
            let chunks = crate::indexing::parse_file(&file_path, filepath, language);

            if chunks.is_empty() {
                continue;
            }

            let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
            match engine.embed_batch(&texts) {
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

        let msg = format!(
            "<shadow_index files=\"{}\" chunks=\"{}\" ttl=\"{}\" base=\"{}\" />",
            total_files, total_chunks, ttl_seconds, base_ref
        );
        info!("{msg}");
        Ok(CallToolResult::success(vec![Content::text(msg)]))
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
            "kt (Knowledge Transfer) - A local codebase RAG system. Use kt_search for semantic code search, kt_read_file to read specific files, and kt_sync to index/update a directory.".to_string(),
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

fn parse_language(s: &str) -> Option<Language> {
    match s.to_lowercase().as_str() {
        "rust" | "rs" => Some(Language::Rust),
        "go" | "golang" => Some(Language::Go),
        "java" => Some(Language::Java),
        _ => None,
    }
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
) -> std::collections::HashMap<String, SearchResult> {
    let mut context_map = std::collections::HashMap::new();
    let mut needed_names: Vec<String> = Vec::new();

    for result in results {
        if let Some(ref parent_ctx) = result.parent_context {
            if let Some(name) = extract_parent_type_name(parent_ctx) {
                if !context_map.contains_key(&name) && !needed_names.contains(&name) {
                    needed_names.push(name);
                }
            }
        }
    }

    if needed_names.is_empty() {
        return context_map;
    }

    match storage.lookup_chunks_by_name(&needed_names).await {
        Ok(parent_results) => {
            for pr in parent_results {
                context_map.insert(pr.name.clone(), pr);
            }
        }
        Err(e) => {
            warn!("Failed to resolve one-hop context: {e}");
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
    one_hop: &std::collections::HashMap<String, SearchResult>,
) -> String {
    let mut xml = format!("<search_results query=\"{}\">\n", xml_escape(query));
    let mut total_len = 0usize;

    for result in results {
        let content = if headers_only {
            result.signature.clone()
        } else {
            truncate_content(&result.content).to_string()
        };

        let parent_xml = if let Some(ref parent_ctx) = result.parent_context {
            if let Some(parent_result) =
                extract_parent_type_name(parent_ctx).and_then(|name| one_hop.get(&name))
            {
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
            "  <chunk filepath=\"{}\" language=\"{}\" type=\"{}\" name=\"{}\" signature=\"{}\" score=\"{:.4}\">\n{}    {}\n  </chunk>\n",
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

fn format_file_results(results: &[SearchResult], filepath: &str, headers_only: bool) -> String {
    let mut xml = format!("<file filepath=\"{}\">\n", xml_escape(filepath));
    for result in results {
        let content = if headers_only {
            xml_escape(&result.signature)
        } else {
            xml_escape(&result.content)
        };
        xml.push_str(&format!(
            "  <chunk type=\"{}\" name=\"{}\" signature=\"{}\">\n    {}\n  </chunk>\n",
            xml_escape(&result.node_type),
            xml_escape(&result.name),
            xml_escape(&result.signature),
            content,
        ));
    }
    xml.push_str("</file>");
    xml
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
            filepath: "src/example.rs".to_string(),
            language: Language::Rust,
            node_type: "function".to_string(),
            name: format!("name_{chunk_id}"),
            signature: String::new(),
            content: String::new(),
            parent_context: None,
            score,
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
}
