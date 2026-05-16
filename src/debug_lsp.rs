use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugFeedbackVerdict {
    Helpful,
    NotHelpful,
    Bug,
    Idea,
}

impl DebugFeedbackVerdict {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "helpful" => Some(Self::Helpful),
            "not_helpful" => Some(Self::NotHelpful),
            "bug" => Some(Self::Bug),
            "idea" => Some(Self::Idea),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Helpful => "helpful",
            Self::NotHelpful => "not_helpful",
            Self::Bug => "bug",
            Self::Idea => "idea",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugFeedbackRecord {
    pub timestamp: String,
    pub kt_version: String,
    pub verdict: DebugFeedbackVerdict,
    pub summary: String,
    pub scenario: Option<String>,
    pub evidence: Option<String>,
    pub recommendation: Option<String>,
    pub active_tool: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DebugLspStatus {
    pub root_path: PathBuf,
    pub analyzer: String,
    pub running: bool,
}

#[derive(Debug, Default)]
pub struct DebugLspManager {
    sessions: RwLock<HashMap<PathBuf, Arc<RustAnalyzerSession>>>,
}

impl DebugLspManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn status(&self, root: Option<&Path>) -> Vec<DebugLspStatus> {
        let sessions = self.sessions.read().await;
        let mut statuses = Vec::new();

        for (session_root, session) in sessions.iter() {
            if root.is_some_and(|root| root != session_root) {
                continue;
            }
            statuses.push(DebugLspStatus {
                root_path: session_root.clone(),
                analyzer: session.analyzer.clone(),
                running: session.is_running().await,
            });
        }

        statuses
    }

    pub async fn definition(
        &self,
        root: &Path,
        filepath: &Path,
        line: usize,
        character: usize,
    ) -> Result<Value> {
        let session = self.session_for(root).await?;
        session
            .request_document_locations_with_retry(filepath, "textDocument/definition", || {
                Ok(json!({
                    "textDocument": {"uri": path_to_file_uri(filepath)?},
                    "position": {"line": line, "character": character}
                }))
            })
            .await
    }

    pub async fn references(
        &self,
        root: &Path,
        filepath: &Path,
        line: usize,
        character: usize,
        include_declaration: bool,
    ) -> Result<Value> {
        let session = self.session_for(root).await?;
        session
            .request_document_locations_with_retry(filepath, "textDocument/references", || {
                Ok(json!({
                    "textDocument": {"uri": path_to_file_uri(filepath)?},
                    "position": {"line": line, "character": character},
                    "context": {"includeDeclaration": include_declaration}
                }))
            })
            .await
    }

    pub async fn document_symbols(&self, root: &Path, filepath: &Path) -> Result<Value> {
        let session = self.session_for(root).await?;
        session
            .request_document_with_retry(filepath, "textDocument/documentSymbol", || {
                Ok(json!({
                    "textDocument": {"uri": path_to_file_uri(filepath)?}
                }))
            })
            .await
    }

    async fn session_for(&self, root: &Path) -> Result<Arc<RustAnalyzerSession>> {
        let root = root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize LSP root path {}", root.display()))?;

        if let Some(session) = self.sessions.read().await.get(&root).cloned() {
            if session.is_running().await {
                return Ok(session);
            }
        }

        let session = Arc::new(RustAnalyzerSession::start(root.clone()).await?);
        self.sessions
            .write()
            .await
            .insert(root.clone(), session.clone());
        Ok(session)
    }
}

#[derive(Debug)]
struct RustAnalyzerSession {
    root: PathBuf,
    analyzer: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    request_lock: Mutex<()>,
    next_id: AtomicU64,
    opened_documents: Mutex<HashMap<String, OpenedDocument>>,
}

#[derive(Debug, Clone)]
struct OpenedDocument {
    version: i32,
    content_hash: u64,
}

impl RustAnalyzerSession {
    async fn start(root: PathBuf) -> Result<Self> {
        let analyzer =
            std::env::var("KT_RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".to_string());
        let mut child = Command::new(&analyzer)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to start rust-analyzer from {analyzer}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("rust-analyzer stdin was not available"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("rust-analyzer stdout was not available"))?;

        let session = Self {
            root,
            analyzer,
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            request_lock: Mutex::new(()),
            next_id: AtomicU64::new(1),
            opened_documents: Mutex::new(HashMap::new()),
        };
        session.initialize().await?;
        Ok(session)
    }

    async fn initialize(&self) -> Result<()> {
        let root_uri = path_to_file_uri(&self.root)?;
        self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "rootPath": self.root.to_string_lossy(),
                "capabilities": {},
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": self.root.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("workspace")
                }]
            }),
        )
        .await?;
        self.notify("initialized", json!({})).await
    }

    async fn is_running(&self) -> bool {
        match self.child.lock().await.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => false,
        }
    }

    async fn request_document_with_retry<F>(
        &self,
        filepath: &Path,
        method: &str,
        build_params: F,
    ) -> Result<Value>
    where
        F: Fn() -> Result<Value>,
    {
        let mut last_error = None;

        for _ in 0..3 {
            self.open_or_update_document(filepath).await?;
            match self.request(method, build_params()?).await {
                Ok(value) => return Ok(value),
                Err(error) if is_content_modified_error(&error) => {
                    last_error = Some(error);
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                Err(error) => return Err(error),
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("LSP request {method} failed")))
    }

    async fn request_document_locations_with_retry<F>(
        &self,
        filepath: &Path,
        method: &str,
        build_params: F,
    ) -> Result<Value>
    where
        F: Fn() -> Result<Value>,
    {
        let mut last = Value::Null;

        for attempt in 0..20 {
            let value = self
                .request_document_with_retry(filepath, method, &build_params)
                .await?;
            if contains_lsp_location(&value) {
                return Ok(value);
            }
            last = value;

            if attempt + 1 < 20 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }

        Ok(last)
    }

    async fn open_or_update_document(&self, filepath: &Path) -> Result<()> {
        let _guard = self.request_lock.lock().await;
        let uri = path_to_file_uri(filepath)?;
        let text = tokio::fs::read_to_string(filepath)
            .await
            .with_context(|| format!("failed to read {}", filepath.display()))?;
        let content_hash = hash_text(&text);
        let mut opened_documents = self.opened_documents.lock().await;

        if let Some(document) = opened_documents.get_mut(&uri) {
            if document.content_hash == content_hash {
                return Ok(());
            }
            document.version += 1;
            document.content_hash = content_hash;
            self.write_message(json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "version": document.version
                    },
                    "contentChanges": [{"text": text}]
                }
            }))
            .await?;
        } else {
            opened_documents.insert(
                uri.clone(),
                OpenedDocument {
                    version: 1,
                    content_hash,
                },
            );
            self.write_message(json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "languageId": "rust",
                        "version": 1,
                        "text": text
                    }
                }
            }))
            .await?;
        }

        Ok(())
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let _guard = self.request_lock.lock().await;
        self.write_message(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let _guard = self.request_lock.lock().await;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.write_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;

        loop {
            let message = {
                let mut stdout = self.stdout.lock().await;
                read_json_rpc_message(&mut stdout).await?
            };

            if is_server_request(&message, id) {
                self.respond_to_server_request(&message).await?;
                continue;
            }

            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }

            if let Some(error) = message.get("error") {
                return Err(anyhow!("LSP request {method} failed: {error}"));
            }

            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn write_message(&self, message: Value) -> Result<()> {
        let framed = encode_json_rpc_message(&message)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&framed).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn respond_to_server_request(&self, message: &Value) -> Result<()> {
        let Some(request_id) = message.get("id").cloned() else {
            return Ok(());
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = match method {
            "workspace/configuration" => Value::Array(Vec::new()),
            _ => Value::Null,
        };

        self.write_message(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": result
        }))
        .await
    }
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn is_content_modified_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("\"code\":-32801") || message.contains("content modified")
}

fn contains_lsp_location(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_lsp_location),
        Value::Object(object) => {
            (object.get("uri").is_some() && object.get("range").is_some())
                || (object.get("targetUri").is_some() && object.get("targetRange").is_some())
                || object.values().any(contains_lsp_location)
        }
        _ => false,
    }
}

fn is_server_request(message: &Value, active_request_id: u64) -> bool {
    message.get("method").is_some()
        && message.get("id").and_then(Value::as_u64) != Some(active_request_id)
        && message.get("id").is_some()
}

pub fn feedback_path(config_dir: &Path) -> PathBuf {
    config_dir.join("debug-feedback.jsonl")
}

pub fn append_feedback_record(path: &Path, record: &DebugFeedbackRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, record)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn read_feedback_records(
    path: &Path,
    limit: Option<usize>,
) -> Result<Vec<DebugFeedbackRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)?;
    let mut records = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if let Some(limit) = limit {
        if records.len() > limit {
            records = records.split_off(records.len() - limit);
        }
    }

    Ok(records)
}

pub fn resolve_file_path(root: &Path, filepath: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = PathBuf::from(filepath);
    let absolute = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("failed to resolve file path {}", absolute.display()))?;

    if !canonical.starts_with(&root) {
        return Err(anyhow!(
            "file {} is outside LSP root {}",
            canonical.display(),
            root.display()
        ));
    }

    Ok(canonical)
}

pub fn relative_path_for_root(root: &Path, filepath: &str) -> Result<String> {
    let root = root.canonicalize()?;
    let path = PathBuf::from(filepath);
    let absolute = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("failed to resolve file path {}", absolute.display()))?;
    let relative = canonical.strip_prefix(&root).with_context(|| {
        format!(
            "file {} is outside codebase root {}",
            canonical.display(),
            root.display()
        )
    })?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

pub fn path_to_file_uri(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path_str = absolute
        .to_str()
        .ok_or_else(|| anyhow!("path contains invalid UTF-8: {}", absolute.display()))?;
    Ok(format!("file://{}", percent_encode_path(path_str)))
}

pub fn file_uri_to_path(uri: &str) -> Result<PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| anyhow!("not a file URI: {uri}"))?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    Ok(PathBuf::from(percent_decode(path)?))
}

pub fn encode_json_rpc_message(message: &Value) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(message)?;
    let mut framed = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    framed.extend(payload);
    Ok(framed)
}

pub fn decode_json_rpc_message(frame: &[u8]) -> Result<Value> {
    let split = frame
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing JSON-RPC header terminator"))?;
    let headers = std::str::from_utf8(&frame[..split])?;
    let content_length = headers
        .lines()
        .find_map(parse_content_length)
        .ok_or_else(|| anyhow!("missing Content-Length header"))?;
    let payload_start = split + 4;
    let payload_end = payload_start + content_length;
    if frame.len() < payload_end {
        return Err(anyhow!("incomplete JSON-RPC payload"));
    }
    Ok(serde_json::from_slice(&frame[payload_start..payload_end])?)
}

async fn read_json_rpc_message(reader: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut content_length = None;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(anyhow!("LSP process closed stdout"));
        }

        let line = line.trim_end_matches(&['\r', '\n'][..]);
        if line.is_empty() {
            break;
        }

        if let Some(length) = parse_content_length(line) {
            content_length = Some(length);
        }
    }

    let content_length =
        content_length.ok_or_else(|| anyhow!("missing Content-Length header from LSP"))?;
    let mut payload = vec![0; content_length];
    reader.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

fn parse_content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    if !name.eq_ignore_ascii_case("content-length") {
        return None;
    }
    value.trim().parse().ok()
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                encoded.push(*byte as char)
            }
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(anyhow!("truncated percent escape in file URI"));
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])?;
            let byte = u8::from_str_radix(hex, 16)
                .with_context(|| format!("invalid percent escape %{hex}"))?;
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    Ok(String::from_utf8(decoded)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_rpc_frame_round_trips_content_length_payload() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {"ok": true}
        });

        let framed = encode_json_rpc_message(&message).unwrap();
        let decoded = decode_json_rpc_message(&framed).unwrap();

        assert_eq!(decoded, message);
    }

    #[test]
    fn file_uri_round_trip_escapes_spaces() {
        let path = std::path::Path::new("/tmp/kt debug/src/lib.rs");

        let uri = path_to_file_uri(path).unwrap();
        let decoded = file_uri_to_path(&uri).unwrap();

        assert_eq!(uri, "file:///tmp/kt%20debug/src/lib.rs");
        assert_eq!(decoded, path);
    }

    #[test]
    fn feedback_append_and_read_limit_preserves_jsonl_order() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("debug-feedback.jsonl");
        let first = DebugFeedbackRecord {
            timestamp: "2026-05-16T12:00:00Z".to_string(),
            kt_version: "0.1.0".to_string(),
            verdict: DebugFeedbackVerdict::Helpful,
            summary: "definition resolved the target".to_string(),
            scenario: Some("definition".to_string()),
            evidence: None,
            recommendation: None,
            active_tool: Some("_debug_lsp_definition".to_string()),
            git_branch: Some("main".to_string()),
            git_commit: Some("abc123".to_string()),
        };
        let second = DebugFeedbackRecord {
            summary: "symbols were noisy".to_string(),
            verdict: DebugFeedbackVerdict::NotHelpful,
            ..first.clone()
        };

        append_feedback_record(&path, &first).unwrap();
        append_feedback_record(&path, &second).unwrap();

        let records = read_feedback_records(&path, Some(1)).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].summary, "symbols were noisy");
        assert_eq!(records[0].verdict, DebugFeedbackVerdict::NotHelpful);
    }
}
