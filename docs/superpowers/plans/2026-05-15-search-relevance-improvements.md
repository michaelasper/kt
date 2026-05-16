# Search Relevance Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve kt search relevance by adding file role classification (test vs. implementation), boilerplate stripping for embeddings, and call-graph indexing for flow discovery.

**Architecture:** Three independent features delivered sequentially. File role classification adds a `FileRole` enum and `file_role` field to `Chunk`, detected at discovery time and stored as a Redis TAG field for search-time ranking. Boierplate stripping modifies `chunk_embedding_text()` to produce cleaner embedding input without changing stored content. Call-graph indexing adds a `calls` field to `Chunk` (extracted during Tree-sitter traversal), stored as a Redis TEXT field for BM25 matching, with post-search expansion at the MCP layer.

**Tech Stack:** Rust, Tree-sitter, Redis (FT.SEARCH with TAG + TEXT fields), ONNX Runtime (all-MiniLM-L6-v2)

---

## Task 1: Add `FileRole` enum and detection logic

**Files:**
- Modify: `src/lib.rs:182-195` (Chunk struct)
- Modify: `src/discovery.rs:62-67` (DiscoveredFile struct)
- Create: `src/file_role.rs`

- [ ] **Step 1: Write the `FileRole` enum**

Create `src/file_role.rs` with the enum and detection function:

```rust
use crate::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileRole {
    Implementation,
    Test,
    Fixture,
    Generated,
    Config,
}

impl FileRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Test => "test",
            Self::Fixture => "fixture",
            Self::Generated => "generated",
            Self::Config => "config",
        }
    }

    pub fn detect(relative_path: &str, language: Language) -> Self {
        let path = relative_path.replace('\\', "/");
        let path_lower = path.to_ascii_lowercase();
        let filename = path.rsplit('/').next().unwrap_or(&path);

        if Self::is_test_path(&path, &path_lower, filename, language) {
            return Self::Test;
        }
        if Self::is_generated_path(&path_lower, filename, language) {
            return Self::Generated;
        }
        if Self::is_fixture_path(&path, &path_lower) {
            return Self::Fixture;
        }
        if Self::is_config_path(&path, filename, language) {
            return Self::Config;
        }
        Self::Implementation
    }

    fn is_test_path(path: &str, path_lower: &str, filename: &str, language: Language) -> bool {
        if path_lower.contains("/test/")
            || path_lower.contains("/tests/")
            || path_lower.contains("/__tests__/")
            || path_lower.contains("/spec/")
        {
            return true;
        }
        match language {
            Language::Rust => filename.ends_with("_test.rs") || path_lower.contains("/tests/"),
            Language::Go => filename.ends_with("_test.go"),
            Language::Java => {
                filename.ends_with("test.java")
                    || filename.ends_with("tests.java")
                    || filename.ends_with("it.java")
                    || path_lower.contains("src/test/")
            }
            Language::Python => {
                filename.starts_with("test_")
                    || filename.ends_with("_test.py")
                    || filename.ends_with("_tests.py")
            }
            Language::Swift => filename.ends_with("tests.swift") || filename.contains("test"),
            Language::ObjectiveC => {
                filename.ends_with("test.m")
                    || filename.ends_with("tests.m")
                    || filename.ends_with("spec.m")
                    || filename.ends_with("specs.m")
            }
            Language::TypeScript | Language::Tsx | Language::Javascript => {
                filename.ends_with(".test.ts")
                    || filename.ends_with(".test.tsx")
                    || filename.ends_with(".test.js")
                    || filename.ends_with(".spec.ts")
                    || filename.ends_with(".spec.tsx")
                    || filename.ends_with(".spec.js")
            }
            Language::Markdown | Language::Html => false,
        }
    }

    fn is_generated_path(path_lower: &str, filename: &str, language: Language) -> bool {
        match language {
            Language::Go => {
                filename.ends_with(".pb.go")
                    || filename.ends_with(".generated.go")
                    || path_lower.contains(".pb.")
            }
            Language::Python => {
                filename.ends_with("_pb2.py")
                    || filename.ends_with("_pb2_grpc.py")
                    || filename.contains(".generated.")
            }
            Language::Java => filename.contains(".generated.") || filename.ends_with(".grpc.java"),
            _ => filename.contains(".generated."),
        }
    }

    fn is_fixture_path(path: &str, path_lower: &str) -> bool {
        path_lower.contains("/fixtures/")
            || path_lower.contains("/__fixtures__/")
            || path_lower.contains("/testdata/")
            || path_lower.contains("/test_data/")
            || path_lower.contains("/testfixtures/")
    }

    fn is_config_path(path: &str, filename: &str, language: Language) -> bool {
        match language {
            Language::Rust => filename.ends_with(".config.rs") || path.contains("config/"),
            Language::Go => filename.ends_with("_config.go") || path.contains("config/"),
            Language::Java => path.contains("config/") || filename.ends_with("Config.java"),
            Language::Python => path.contains("config/") || filename.starts_with("config_"),
            _ => path.contains("config/"),
        }
    }
}

impl std::fmt::Display for FileRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
```

- [ ] **Step 2: Add `FileRole` to `DiscoveredFile`**

In `src/discovery.rs`, add `file_role` to `DiscoveredFile` and compute it during discovery.

Change `DiscoveredFile` struct:

```rust
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub relative_path: String,
    pub language: Language,
    pub file_role: crate::file_role::FileRole,
}
```

In `discover_files_with_options`, set `file_role` when creating `DiscoveredFile`:

```rust
let file_role = crate::file_role::FileRole::detect(&relative_path, language);

Some(DiscoveredFile {
    path,
    relative_path,
    language,
    file_role,
})
```

- [ ] **Step 3: Add `FileRole` to `Chunk` struct**

In `src/lib.rs`, add the `file_role` field to `Chunk`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub chunk_id: String,
    pub codebase_id: String,
    pub filepath: String,
    pub language: Language,
    pub node_type: String,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub parent_context: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub file_role: FileRole,
    pub calls: Vec<CallRef>,
}
```

Add `CallRef` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRef {
    pub name: String,
    pub receiver: Option<String>,
}
```

Add `FileRole` import and re-export in `src/lib.rs`:

```rust
pub mod file_role;
pub use file_role::FileRole;
```

Also update `SearchResult` to include `file_role`:

```rust
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub codebase_id: String,
    pub codebase_alias: Option<String>,
    pub root_path: String,
    pub filepath: String,
    pub language: Language,
    pub node_type: String,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub parent_context: Option<String>,
    pub score: f64,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub file_role: FileRole,
}
```

Default `FileRole::Implementation` for backward compatibility in all creation sites.

- [ ] **Step 4: Propagate `FileRole` through the sync pipeline**

In `src/sync.rs`, update `parse_file_async` call sites to pass `file_role` from `DiscoveredFile` into the chunking pipeline.

The `parse_file_async` and `parse_file` functions need a new `file_role: FileRole` parameter. Pass it through to `extract_chunks` and `fallback_line_chunks`, and set it on each produced `Chunk`.

In `src/indexing.rs`:

```rust
pub async fn parse_file_async(
    path: PathBuf,
    relative_path: String,
    language: Language,
    codebase_id: String,
    file_role: FileRole,
) -> crate::error::Result<Vec<Chunk>> {
    tokio::task::spawn_blocking(move || parse_file(&path, &relative_path, language, &codebase_id, file_role))
        .await
        .map_err(crate::error::KtError::from)?
}

pub fn parse_file(
    path: &Path,
    relative_path: &str,
    language: Language,
    codebase_id: &str,
    file_role: FileRole,
) -> crate::error::Result<Vec<Chunk>> {
    // ... existing code ...
    // Pass file_role to extract_chunks and fallback_line_chunks
}
```

In `extract_chunks` and `fallback_line_chunks`, set `file_role` on each `Chunk`:

```rust
Chunk {
    // ... existing fields ...
    file_role,
    calls: Vec::new(), // placeholder for Task 3
}
```

In `src/sync.rs`, update the sync `execute` function to pass `file.file_role`:

```rust
let chunks = match crate::indexing::parse_file_async(
    file.path.clone(),
    file.relative_path.clone(),
    file.language,
    codebase_id.clone(),
    file.file_role,
)
.await
{
    // ...
};
```

- [ ] **Step 5: Update all `Chunk` construction sites for default `FileRole`**

Search the codebase for all places where `Chunk { ... }` is constructed (tests, storage commands, etc.) and add `file_role: FileRole::Implementation` and `calls: Vec::new()`. This includes:
- `src/embedding.rs` tests (the `sample_chunk` helper)
- `src/storage/commands.rs` (indirectly via Chunk struct)
- `src/mcp.rs` (any Chunk construction in shadow indexing)
- Any `index_pr` or shadow indexing code

- [ ] **Step 6: Write unit tests for `FileRole::detect`**

In `src/file_role.rs`, add tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_test_file() {
        assert_eq!(FileRole::detect("src/foo_test.rs", Language::Rust), FileRole::Test);
        assert_eq!(FileRole::detect("tests/integration.rs", Language::Rust), FileRole::Test);
    }

    #[test]
    fn test_java_test_file() {
        assert_eq!(FileRole::detect("src/test/java/com/example/UserTest.java", Language::Java), FileRole::Test);
        assert_eq!(FileRole::detect("src/main/java/com/example/User.java", Language::Java), FileRole::Implementation);
    }

    #[test]
    fn test_go_test_file() {
        assert_eq!(FileRole::detect("handler_test.go", Language::Go), FileRole::Test);
        assert_eq!(FileRole::detect("handler.go", Language::Go), FileRole::Implementation);
    }

    #[test]
    fn test_python_test_file() {
        assert_eq!(FileRole::detect("tests/test_auth.py", Language::Python), FileRole::Test);
        assert_eq!(FileRole::detect("auth/__tests__/test_login.py", Language::Python), FileRole::Test);
    }

    #[test]
    fn test_typescript_test_file() {
        assert_eq!(FileRole::detect("app/login.test.ts", Language::TypeScript), FileRole::Test);
        assert_eq!(FileRole::detect("app/login.spec.tsx", Language::Tsx), FileRole::Test);
    }

    #[test]
    fn test_generated_file() {
        assert_eq!(FileRole::detect("api.pb.go", Language::Go), FileRole::Generated);
        assert_eq!(FileRole::detect("foo_pb2.py", Language::Python), FileRole::Generated);
    }

    #[test]
    fn test_fixture_path() {
        assert_eq!(FileRole::detect("tests/fixtures/data.json", Language::Python), FileRole::Fixture);
    }

    #[test]
    fn test_implementation_default() {
        assert_eq!(FileRole::detect("src/main.rs", Language::Rust), FileRole::Implementation);
        assert_eq!(FileRole::detect("lib.rs", Language::Rust), FileRole::Implementation);
    }
}
```

- [ ] **Step 7: Run tests and verify**

Run: `cargo test --lib file_role`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/lib.rs src/discovery.rs src/file_role.rs src/indexing.rs src/sync.rs src/embedding.rs src/storage/ src/mcp.rs
git commit -m "feat: add FileRole enum and detection for test/implementation classification"
```

---

## Task 2: Store `file_role` and `calls` in Redis schema

**Files:**
- Modify: `src/storage/index.rs`
- Modify: `src/storage/commands.rs`
- Modify: `src/storage/search.rs`

- [ ] **Step 1: Add `file_role` and `calls` fields to Redis schema**

In `src/storage/index.rs`, update `build_schema_args` to include the new fields:

After the `end_line` / `NUMERIC` entry and before `embedding`, add:

```rust
"file_role", "TAG",
"calls", "TEXT",
"parent_context", "TEXT",
```

Wait — `parent_context` already exists. Add `file_role` and `calls` after `end_line`:

```rust
// In build_schema_args, after "end_line", "NUMERIC", "SORTABLE":
"file_role", "TAG",
"calls", "TEXT",
```

Update `SCHEMA_VERSION` from `"3"` to `"4"`.

- [ ] **Step 2: Add `file_role` and `calls` to `SEARCH_RETURN_FIELDS`**

In `src/storage/search.rs`, update the `SEARCH_RETURN_FIELDS` array:

```rust
const SEARCH_RETURN_FIELDS: [&str; 13] = [
    "chunk_id",
    "codebase_id",
    "filepath",
    "language",
    "node_type",
    "name",
    "signature",
    "content",
    "parent_context",
    "start_line",
    "end_line",
    "file_role",
    "calls",
];
```

- [ ] **Step 3: Store `file_role` and `calls` in `append_hset_fields` macro**

In `src/storage/commands.rs`, update the `append_hset_fields!` macro to include the two new fields after the `parent_context` block:

```rust
// After the parent_context block:
$cmd.arg("file_role").arg($chunk.file_role.as_str());
let calls_str = $chunk.calls.iter().map(|c| {
    match &c.receiver {
        Some(r) => format!("{}::{}", r, c.name),
        None => c.name.clone(),
    }
}).collect::<Vec<_>>().join(" ");
if !calls_str.is_empty() {
    $cmd.arg("calls").arg(&calls_str);
}
```

- [ ] **Step 4: Parse `file_role` and `calls` in `parse_search_results`**

In `src/storage/search.rs`, update the `parse_search_page` function. Add variables after the existing field declarations:

```rust
let mut file_role = FileRole::Implementation;
let mut calls = String::new();
```

Add matching in the field loop:

```rust
"file_role" => {
    if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
        file_role = FileRole::parse(&val).unwrap_or(FileRole::Implementation);
    }
}
"calls" => {
    if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
        calls = val;
    }
}
```

Add `use crate::FileRole;` at the top of `search.rs`.

Update `SearchResult` construction to include `file_role` and `calls`:

```rust
results.push(SearchResult {
    // ... existing fields ...
    file_role,
});
```

(Note: `calls` will be parsed from the stored string back into `Vec<CallRef>` in Task 3's search-time expansion. For now, we store the raw string and can parse it later.)

Actually, to keep `SearchResult` clean, add only `file_role` to `SearchResult` for now. The `calls` field will be used for BM25 matching and embedding text — it doesn't need to be in `SearchResult` until we add call-context expansion.

- [ ] **Step 5: Add `FileRole::parse` method**

In `src/file_role.rs`, add:

```rust
pub fn parse(s: &str) -> Option<Self> {
    match s {
        "implementation" => Some(Self::Implementation),
        "test" => Some(Self::Test),
        "fixture" => Some(Self::Fixture),
        "generated" => Some(Self::Generated),
        "config" => Some(Self::Config),
        _ => None,
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: All existing tests pass. Some tests may need `file_role` and `calls` fields added to manual `Chunk` construction.

- [ ] **Step 7: Commit**

```bash
git add src/storage/index.rs src/storage/commands.rs src/storage/search.rs src/file_role.rs src/lib.rs
git commit -m "feat: add file_role and calls fields to Redis schema and storage"
```

---

## Task 3: Include `file_role` and `calls` in embedding text

**Files:**
- Modify: `src/embedding.rs`

- [ ] **Step 1: Update `chunk_embedding_text` to include `file_role` and `calls`**

In `src/embedding.rs`, update `chunk_embedding_text`:

```rust
pub(crate) fn chunk_embedding_text(chunk: &Chunk) -> String {
    let mut text = format!(
        "filepath: {}\nlanguage: {}\nnode_type: {}\nfile_role: {}\nname: {}\nsignature: {}\n",
        chunk.filepath,
        chunk.language.as_str(),
        chunk.node_type,
        chunk.file_role.as_str(),
        chunk.name,
        chunk.signature
    );

    if !chunk.calls.is_empty() {
        text.push_str("calls: ");
        let calls_str = chunk.calls.iter().map(|c| {
            match &c.receiver {
                Some(r) => format!("{}::{}", r, c.name),
                None => c.name.clone(),
            }
        }).collect::<Vec<_>>().join(" ");
        text.push_str(&calls_str);
        text.push('\n');
    }

    if let Some(parent_context) = chunk
        .parent_context
        .as_deref()
        .map(str::trim)
        .filter(|ctx| !ctx.is_empty())
    {
        text.push_str("parent_context:\n");
        text.push_str(parent_context);
        text.push('\n');
    }

    let content_for_embedding = strip_boilerplate(&chunk.content, chunk.language);
    text.push_str("content:\n");
    text.push_str(&content_for_embedding);
    text
}
```

- [ ] **Step 2: Write the `strip_boilerplate` function**

Add `strip_boilerplate` to `src/embedding.rs`. This function strips import/use statements, package declarations, and blank lines from the content before embedding:

```rust
fn strip_boilerplate(content: &str, language: Language) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result_lines = Vec::with_capacity(lines.len());
    let mut past_header = false;

    for line in lines {
        let trimmed = line.trim();

        if !past_header {
            match language {
                Language::Rust => {
                    if trimmed.starts_with("use ")
                        || trimmed.starts_with("use{")
                        || trimmed.starts_with("extern crate ")
                        || trimmed.is_empty()
                    {
                        continue;
                    }
                }
                Language::Go => {
                    if trimmed.starts_with("import ")
                        || trimmed.starts_with("import(")
                        || trimmed.starts_with("package ")
                        || trimmed.is_empty()
                    {
                        continue;
                    }
                }
                Language::Java => {
                    if trimmed.starts_with("import ")
                        || trimmed.starts_with("package ")
                        || trimmed.is_empty()
                    {
                        continue;
                    }
                }
                Language::Python => {
                    if trimmed.starts_with("import ")
                        || trimmed.starts_with("from ")
                        || trimmed.is_empty()
                    {
                        continue;
                    }
                }
                Language::Swift => {
                    if trimmed.starts_with("import ") || trimmed.is_empty() {
                        continue;
                    }
                }
                Language::ObjectiveC => {
                    if trimmed.starts_with("#import ")
                        || trimmed.starts_with("@import ")
                        || trimmed.is_empty()
                    {
                        continue;
                    }
                }
                Language::TypeScript | Language::Tsx | Language::Javascript => {
                    if trimmed.starts_with("import ")
                        || trimmed.starts_with("import{")
                        || trimmed.starts_with("} from")
                        || trimmed.is_empty()
                    {
                        continue;
                    }
                }
                Language::Markdown | Language::Html => {}
            }
        }

        if !trimmed.is_empty() {
            past_header = true;
        }

        result_lines.push(line);
    }

    result_lines.join("\n")
}
```

- [ ] **Step 3: Write tests for `strip_boilerplate`**

In `src/embedding.rs` tests, add:

```rust
#[test]
fn strip_boilerplate_removes_rust_imports() {
    let content = "use std::collections::HashMap;\nuse anyhow::Result;\n\nfn main() {\n    println!(\"hello\");\n}\n";
    let stripped = strip_boilerplate(content, Language::Rust);
    assert!(!stripped.contains("use std"));
    assert!(!stripped.contains("use anyhow"));
    assert!(stripped.contains("fn main"));
}

#[test]
fn strip_boilerplate_removes_java_imports() {
    let content = "package com.example;\n\nimport java.util.List;\n\npublic class Main {\n}\n";
    let stripped = strip_boilerplate(content, Language::Java);
    assert!(!stripped.contains("package com.example"));
    assert!(!stripped.contains("import java.util"));
    assert!(stripped.contains("public class Main"));
}

#[test]
fn strip_boilerplate_removes_python_imports() {
    let content = "import os\nfrom pathlib import Path\n\ndef main():\n    pass\n";
    let stripped = strip_boilerplate(content, Language::Python);
    assert!(!stripped.contains("import os"));
    assert!(!stripped.contains("from pathlib"));
    assert!(stripped.contains("def main"));
}

#[test]
fn strip_boilerplate_preserves_content_after_header() {
    let content = "use std::io;\n\nfn read() -> String {\n    String::new()\n}\n";
    let stripped = strip_boilerplate(content, Language::Rust);
    assert!(stripped.contains("fn read"));
    assert!(stripped.contains("String::new"));
}

#[test]
fn chunk_embedding_text_includes_file_role() {
    let mut chunk = sample_chunk(None);
    chunk.file_role = FileRole::Test;
    chunk.calls = vec![CallRef { name: "setup".to_string(), receiver: Some("Server".to_string()) }];
    let text = chunk_embedding_text(&chunk);
    assert!(text.contains("file_role: test"));
    assert!(text.contains("calls: Server::setup"));
}

#[test]
fn chunk_embedding_text_includes_calls_when_empty() {
    let mut chunk = sample_chunk(None);
    chunk.file_role = FileRole::Implementation;
    let text = chunk_embedding_text(&chunk);
    assert!(text.contains("file_role: implementation"));
    assert!(!text.contains("calls:"));
}
```

- [ ] **Step 4: Update `sample_chunk` helper in tests**

In `src/embedding.rs` tests, update `sample_chunk` to include `file_role` and `calls`:

```rust
fn sample_chunk(parent_context: Option<String>) -> Chunk {
    Chunk {
        chunk_id: "chunk-a".to_string(),
        codebase_id: "codebase-a".to_string(),
        filepath: "src/auth.rs".to_string(),
        language: Language::Rust,
        node_type: "function".to_string(),
        name: "verify_token".to_string(),
        signature: "fn verify_token(token: &str) -> bool".to_string(),
        content: "fn verify_token(token: &str) -> bool {\n    !token.is_empty()\n}".to_string(),
        parent_context,
        start_line: 10,
        end_line: 12,
        file_role: FileRole::Implementation,
        calls: Vec::new(),
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib embedding`
Expected: All embedding tests pass including new ones.

- [ ] **Step 6: Commit**

```bash
git add src/embedding.rs
git commit -m "feat: include file_role and calls in embedding text, strip boilerplate from content"
```

---

## Task 4: Add file_role ranking boost in search results

**Files:**
- Modify: `src/storage/search.rs`
- Modify: `src/mcp.rs` (SearchParams and search handler)

- [ ] **Step 1: Add `file_role` filter to `SearchParams` in MCP**

In `src/mcp.rs`, add a `file_role` parameter to `SearchParams`:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    #[schemars(description = "The search query - natural language or code terms")]
    pub query: String,
    #[schemars(description = "Filter by language: rust, go, java, python, swift, objective-c, typescript, tsx, javascript")]
    pub language: Option<String>,
    #[schemars(description = "Number of results to return (default: 3, max: 10)")]
    pub top_k: Option<usize>,
    #[schemars(description = "If true, return only function/type signatures without bodies to save tokens")]
    pub headers_only: Option<bool>,
    #[schemars(description = "Optional directory path to scope search to one indexed codebase")]
    pub directory_path: Option<String>,
    #[schemars(description = "Optional codebase alias to scope search to one indexed codebase")]
    pub codebase_alias: Option<String>,
    #[schemars(description = "Filter by file role: implementation, test, fixture, generated, config")]
    pub file_role: Option<String>,
}
```

- [ ] **Step 2: Add `FileRole` filter to search functions**

In `src/storage/search.rs`, update `build_scope_filters` to accept an optional `FileRole`:

```rust
fn build_scope_filters(
    language: Option<&Language>,
    codebase_id: Option<&str>,
    file_role: Option<&FileRole>,
) -> Vec<String> {
    let mut filters = Vec::new();
    if let Some(id) = codebase_id {
        filters.push(build_codebase_filter(id));
    }
    if let Some(lang) = language {
        filters.push(tag_filter("language", lang.as_str()));
    }
    if let Some(role) = file_role {
        filters.push(tag_filter("file_role", role.as_str()));
    }
    filters
}
```

Thread `file_role` through `build_semantic_query`, `build_lexical_query`, `hybrid_search_impl`, and `execute_hybrid_lane` by adding an `Option<&FileRole>` parameter.

- [ ] **Step 3: Add file_role boost to RRF scoring**

Since `SearchResult` now has `file_role` (added in Task 2), add a boost function and apply it inside `fuse_search_lanes`. The function signature doesn't need to change — just add the boost logic inside:

```rust
fn file_role_boost(role: &FileRole) -> f64 {
    match role {
        FileRole::Implementation => 2.0,
        FileRole::Config => 2.0,
        FileRole::Fixture => 1.4,
        FileRole::Generated => 1.2,
        FileRole::Test => 1.0,
    }
}
```

In `fuse_search_lanes`, after merging and before sorting, apply the boost:

```rust
// ... existing RRF merging via add_rrf_lane calls ...
let mut results: Vec<FusedSearchResult> = fused.into_values().collect();

// Apply file_role boost: implementation code ranks higher
for result in &mut results {
    let boost = file_role_boost(&result.result.file_role);
    result.rrf_score *= boost;
}

// Sort (descending by rrf_score, then by first_seen, then by chunk_id)
results.sort_by(|a, b| { /* unchanged */ });
results.truncate(top_k);
```

Wait — the current sort is `b.rrf_score` first (descending), then `first_seen` (ascending), then `chunk_id`. The boost multiplies `rrf_score` so implementation code gets higher scores. This is correct since higher `rrf_score` = better ranking.

**Actually, let me reconsider.** Looking at the current code, the sort is by `b.rrf_score partial_cmp a.rrf_score` which sorts descending by rrf_score (highest first). Then the final score is `1.0 / rrf_score` (lower = better). So multiplying `rrf_score` by a boost > 1.0 for implementation means implementation results have higher rrf_score, which means they sort first (higher rrf_score = better), and then get lower `1.0/rrf_score` (lower = better). This is correct.

- [ ] **Step 4: Thread `file_role` through the `Storage` layer**

In `src/storage/mod.rs`, update `hybrid_search`, `hybrid_search_scoped`, and their shadow counterparts to accept `Option<&FileRole>` and pass it through to the search functions.

- [ ] **Step 5: Thread `file_role` through MCP search handler**

In `src/mcp.rs`, parse the `file_role` parameter and pass it to storage search:

```rust
let file_role = params.file_role.as_deref()
    .and_then(FileRole::parse);
```

- [ ] **Step 6: Update existing tests**

Update `fuse_search_lanes` tests in `src/storage/search.rs` to use `FileRole::Implementation` in `sample_result`:

```rust
fn sample_result(chunk_id: &str) -> SearchResult {
    SearchResult {
        // ... existing fields ...
        file_role: FileRole::Implementation,
    }
}
```

- [ ] **Step 7: Write test for file_role boost**

```rust
#[test]
fn fuse_search_lanes_boosts_implementation_over_test() {
    let mut impl_result = sample_result("impl_fn");
    impl_result.file_role = FileRole::Implementation;
    let mut test_result = sample_result("test_fn");
    test_result.file_role = FileRole::Test;

    // Same position in both lanes — implementation should rank higher
    let merged = fuse_search_lanes(
        vec![impl_result.clone()],
        vec![test_result.clone()],
        2,
    );

    assert_eq!(merged[0].chunk_id, "impl_fn");
    assert_eq!(merged[1].chunk_id, "test_fn");
}
```

- [ ] **Step 8: Run all tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 9: Commit**

```bash
git add src/storage/search.rs src/storage/mod.rs src/mcp.rs
git commit -m "feat: add file_role filter and ranking boost in search results"
```

---

## Task 5: Extract call references during indexing

**Files:**
- Modify: `src/indexing/languages.rs` (add call node types)
- Modify: `src/indexing.rs` (extract calls during traversal)

- [ ] **Step 1: Add `call_node_types` to `LanguageConfig`**

In `src/indexing/languages.rs`, add a `call_node_types` field to `LanguageConfig`:

```rust
#[derive(Clone, Copy)]
pub struct LanguageConfig {
    pub language: Language,
    pub target_node_types: &'static [&'static str],
    pub container_node_types: &'static [&'static str],
    pub call_node_types: &'static [&'static str],
    ts_language: LanguageFn,
}
```

Add call node types per language:

| Language | Call Node Types |
|----------|----------------|
| Rust | `call_expression`, `method_call_expression` |
| Go | `call_expression` |
| Java | `method_invocation`, `class_instance_creation_expression` |
| Python | `call` |
| Swift | `call_expression` |
| ObjectiveC | `message_expression` |
| TypeScript/Tsx/JS | `call_expression` |
| Markdown | (empty) |
| HTML | (empty) |

- [ ] **Step 2: Implement call extraction in the tree walk**

In `src/indexing.rs`, add a function to extract calls from a node:

```rust
use crate::CallRef;

fn extract_calls_from_node(node: Node, source: &str, config: &LanguageConfig) -> Vec<CallRef> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();

    fn walk_for_calls(
        cursor: &mut tree_sitter::TreeCursor,
        source: &str,
        config: &LanguageConfig,
        calls: &mut Vec<CallRef>,
    ) {
        let node = cursor.node();
        if config.call_node_types.contains(&node.kind()) {
            if let Some(call_ref) = extract_single_call(node, source) {
                calls.push(call_ref);
            }
        }

        if cursor.goto_first_child() {
            loop {
                walk_for_calls(cursor, source, config, calls);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }

    walk_for_calls(&mut cursor, source, config, &mut calls);
    calls
}
```

And `extract_single_call`:

```rust
fn extract_single_call(node: Node, source: &str) -> Option<CallRef> {
    match node.kind() {
        "call_expression" | "method_call_expression" => {
            // Try to get the function name
            let func = node.child_by_field_name("function")
                .or_else(|| node.child(0));
            if let Some(func) = func {
                let text = node_text(func, source);
                let name = text.trim().to_string();
                if !name.is_empty() {
                    // Check if there's a receiver (e.g., self.method or Foo::method)
                    if let Some(dot) = text.find('.') {
                        let receiver = text[..dot].trim().to_string();
                        let method = text[dot + 1..].trim()
                            .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_')
                            .to_string();
                        return Some(CallRef {
                            name: method,
                            receiver: Some(receiver),
                        });
                    } else if let Some(colons) = text.find("::") {
                        let receiver = text[..colons].trim().to_string();
                        let method = text[colons + 2..].trim().to_string();
                        return Some(CallRef {
                            name: method,
                            receiver: Some(receiver),
                        });
                    } else {
                        return Some(CallRef {
                            name,
                            receiver: None,
                        });
                    }
                }
            }
            None
        }
        "method_invocation" => {
            // Java: object.method(args)
            let name_node = node.child_by_field_name("name");
            let name = name_node.map(|n| node_text(n, source).trim().to_string());
            let receiver = node.child_by_field_name("object")
                .map(|n| node_text(n, source).trim().to_string());
            name.map(|n| CallRef { name: n, receiver })
        }
        "class_instance_creation_expression" => {
            // Java: new ClassName(args)
            let type_node = node.child_by_field_name("type")
                .or_else(|| {
                    // Fall back to finding "new" then the type
                    node.children(&mut node.walk())
                        .find(|c| c.kind() == "type_identifier" || c.kind() == "class_type")
                });
            type_node.map(|n| CallRef {
                name: node_text(n, source).trim().to_string(),
                receiver: None,
            })
        }
        "message_expression" => {
            // ObjC: [object method:args]
            let text = node_text(node, source);
            let inner = text.trim_start_matches('[').trim_end_matches(']');
            let name = inner.split_whitespace().nth(1).unwrap_or("").to_string();
            let receiver = inner.split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty() {
                Some(CallRef { name, receiver: Some(receiver) })
            } else {
                None
            }
        }
        "call" => {
            // Python: func(args) or obj.method(args)
            let func = node.child(0);
            if let Some(func) = func {
                let text = node_text(func, source);
                if let Some(dot) = text.rfind('.') {
                    let receiver = text[..dot].trim().to_string();
                    let method = text[dot + 1..].trim().to_string();
                    Some(CallRef { name: method, receiver: Some(receiver) })
                } else {
                    Some(CallRef { name: text.trim().to_string(), receiver: None })
                }
            } else {
                None
            }
        }
        _ => None,
    }
}
```

- [ ] **Step 3: Populate `calls` field in `build_chunk`**

In `src/indexing.rs`, update `build_chunk` to extract calls:

```rust
fn build_chunk(
    node: Node,
    ctx: &ExtractionContext<'_>,
    parent_context: &Option<ParentContext>,
) -> Option<Chunk> {
    // ... existing content extraction ...

    let calls = extract_calls_from_node(node, ctx.source, ctx.config);

    // ... rest of build_chunk ...

    Some(Chunk {
        // ... existing fields ...
        file_role: ctx.file_role,
        calls,
    })
}
```

Add `file_role: FileRole` to `ExtractionContext`:

```rust
struct ExtractionContext<'a> {
    source: &'a str,
    relative_path: &'a str,
    language: Language,
    config: &'a LanguageConfig,
    codebase_id: &'a str,
    file_role: FileRole,
}
```

Pass `file_role` from `parse_file` → `extract_chunks` → `ExtractionContext`.

- [ ] **Step 4: Write tests for call extraction**

```rust
#[test]
fn test_extract_calls_from_rust_method_call() {
    let source = r#"
fn main() {
    let result = server.start();
    let val = create_config();
}
"#;
    let config = LanguageConfig::for_language(Language::Rust);
    let mut parser = Parser::new();
    parser.set_language(&config.tree_sitter_language()).unwrap();
    let tree = parser.parse(source, None).unwrap();

    let chunks = extract_chunks(&tree, source, "test.rs", Language::Rust, &config, "test-codebase", FileRole::Implementation);
    let main_fn = find_chunk(&chunks, "main");
    assert!(main_fn.calls.iter().any(|c| c.name == "start" && c.receiver.as_deref() == Some("server")));
    assert!(main_fn.calls.iter().any(|c| c.name == "create_config" && c.receiver.is_none()));
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib indexing`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/indexing.rs src/indexing/languages.rs src/lib.rs
git commit -m "feat: extract call references during Tree-sitter indexing"
```

---

## Task 6: Add call-context expansion in MCP search results

**Files:**
- Modify: `src/mcp.rs` (search result expansion)

- [ ] **Step 1: Expand search results with called-function context**

In `src/mcp.rs`, after the primary search results are collected, add a step to look up call targets that aren't already in the results. This mirrors the existing `resolve_one_hop_context` pattern.

Add a new function `resolve_call_context` that:
1. Collects call names from results' `calls` fields
2. Filters out names already present in the result set
3. Looks up the remaining names using `lookup_chunks_by_name_scoped`
4. Returns up to N (default: 2) additional chunks

```rust
async fn resolve_call_context(
    results: &[SearchResult],
    storage: &Storage,
    codebase_id: Option<&str>,
) -> Vec<SearchResult> {
    let max_related = 2;
    let result_names: std::collections::HashSet<String> = results.iter()
        .map(|r| r.name.clone())
        .collect();

    let mut call_names: Vec<String> = Vec::new();
    for result in results {
        for call in &result.calls {
            let qualified = match &call.receiver {
                Some(r) => format!("{}::{}", r, call.name),
                None => call.name.clone(),
            };
            if !result_names.contains(&call.name) && !call_names.contains(&call.name) {
                call_names.push(call.name.clone());
            }
        }
    }

    if call_names.is_empty() {
        return Vec::new();
    }

    call_names.truncate(5); // limit lookups

    match storage.lookup_chunks_by_name_scoped(&call_names, codebase_id).await {
        Ok(related) => related.into_iter()
            .filter(|r| !result_names.contains(&r.name))
            .take(max_related)
            .collect(),
        Err(_) => Vec::new(),
    }
}
```

- [ ] **Step 2: Add `<related>` XML elements to the MCP output**

In the `format_search_results` function, add a `<related>` section for call context chunks, similar to the existing `<parent_struct>` pattern.

- [ ] **Step 3: Parse `calls` field from Redis into `Vec<CallRef>` in SearchResult**

Actually, at this point we need the `calls` field in `SearchResult`. We stored it as a space-separated TEXT field. Add a `calls` field to `SearchResult` and parse it from the stored string:

```rust
pub struct SearchResult {
    // ... existing fields ...
    pub file_role: FileRole,
    pub calls: Vec<CallRef>,
}
```

In `parse_search_page`, parse the `calls` field:

```rust
"calls" => {
    if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
        calls = val.split_whitespace()
            .filter_map(|s| {
                if s.contains("::") {
                    let mut parts = s.splitn(2, "::");
                    let receiver = parts.next().unwrap_or("").to_string();
                    let name = parts.next().unwrap_or("").to_string();
                    if !name.is_empty() {
                        Some(CallRef { name, receiver: Some(receiver) })
                    } else {
                        None
                    }
                } else if !s.is_empty() {
                    Some(CallRef { name: s.to_string(), receiver: None })
                } else {
                    None
                }
            })
            .collect();
    }
}
```

- [ ] **Step 4: Integrate call context into the search flow**

In the `kt_search_inner` function, after merging main and shadow results, call `resolve_call_context`:

```rust
let related = resolve_call_context(&results, &storage, codebase_id.as_deref()).await;
```

Include related chunks in the XML output.

- [ ] **Step 5: Update MCP SearchParams JSON schema to include `file_role`**

Already done in Task 4. Ensure the MCP tool description mentions `file_role` parameter.

- [ ] **Step 6: Run all tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/mcp.rs src/lib.rs src/storage/search.rs
git commit -m "feat: add call-context expansion in MCP search results"
```

---

## Task 7: Update shadow/index_pr indexing to include `file_role` and `calls`

**Files:**
- Modify: `src/mcp.rs` (index_pr function)
- Verify: `src/storage/` shadow chunk storage

- [ ] **Step 1: Pass `file_role` through shadow/index_pr pipeline**

In `src/mcp.rs`, find the `kt_index_pr` handler that indexes working tree changes. Ensure the `DiscoveredFile` → `parse_file_async` pipeline passes `file_role` through. The `file_role` for shadow-indexed files should also be detected using `FileRole::detect`.

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/mcp.rs
git commit -m "feat: pass file_role through shadow/index_pr pipeline"
```

---

## Task 8: End-to-end integration test

**Files:**
- Create: `tests/integration_search_relevance.rs` (or add to existing integration tests)

- [ ] **Step 1: Write an integration test that verifies file_role ranking**

Create a test that indexes a small codebase with both implementation and test files, then searches and verifies that implementation results rank higher than test results for the same query.

This test requires Redis. Add it behind the same integration test infrastructure as existing tests.

- [ ] **Step 2: Write an integration test that verifies call context expansion**

Index a codebase where function A calls function B, search for function A, and verify that function B appears in the `<related>` context.

- [ ] **Step 3: Write a unit test verifying boilerplate stripping**

Test that `strip_boilerplate` correctly removes imports, package declarations, and blank lines from multi-language input.

- [ ] **Step 4: Run `cargo clippy` and `cargo test`**

Run: `cargo clippy --all-targets --all-features && cargo test`
Expected: No warnings, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add tests/
git commit -m "test: add integration tests for search relevance improvements"
```