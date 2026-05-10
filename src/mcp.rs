use crate::embedding::EmbeddingEngine;
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
    #[schemars(description = "If true, return only function/type signatures without bodies to save tokens")]
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
        let results = storage
            .hybrid_search(&query_embedding, &params.query, language.as_ref(), top_k)
            .await
            .map_err(mcp_error)?;

        let results = deduplicate_results(results);

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
            .read_file_chunks(&params.filepath)
            .await
            .map_err(mcp_error)?;

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

        info!("Starting sync for {}", params.directory_path);

        let files = crate::discovery::discover_files(root);
        if files.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "<result>No supported files found to index.</result>".to_string(),
            )]));
        }

        let mut total_chunks = 0usize;
        let mut total_files = 0usize;
        let mut errors = 0usize;

        let embedding_guard = self.inner.embedding.read().await;
        let engine = embedding_guard
            .as_ref()
            .ok_or_else(|| mcp_error("Embedding engine not available"))?;

        let storage = self.inner.storage.read().await;

        for file in &files {
            let chunks =
                crate::indexing::parse_file(&file.path, &file.relative_path, file.language);

            if chunks.is_empty() {
                continue;
            }

            if let Err(e) = storage.remove_file_chunks(&file.relative_path).await {
                warn!("Failed to clean old chunks for {}: {e}", file.relative_path);
            }

            let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
            match engine.embed_batch(&texts) {
                Ok(embeddings) => {
                    if let Err(e) = storage.store_chunks_batch(&chunks, &embeddings).await {
                        warn!("Failed to store chunks for {}: {e}", file.relative_path);
                        errors += 1;
                        continue;
                    }
                    total_chunks += chunks.len();
                    total_files += 1;
                }
                Err(e) => {
                    warn!("Failed to embed chunks for {}: {e}", file.relative_path);
                    errors += 1;
                }
            }
        }

        let msg = format!(
            "<result>Sync complete: {} files, {} chunks indexed, {} errors</result>",
            total_files, total_chunks, errors
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
            if let Some(parent_result) = extract_parent_type_name(parent_ctx)
                .and_then(|name| one_hop.get(&name))
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

fn format_file_results(
    results: &[SearchResult],
    filepath: &str,
    headers_only: bool,
) -> String {
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
