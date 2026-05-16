# Project Specification: kt (Knowledge Transfer)

## 1. Meta Information

- **Project Name:** `kt`
- **Description:** A local, privacy-first polyglot codebase RAG via MCP.
- **Implementation Language:** Rust
- **Key Crates:** `tokio` (async runtime), `rmcp` (MCP server), `redis` (Redis client), `tree-sitter` + language parsers, `ort` (ONNX Runtime), `tokenizers` (HuggingFace tokenizer), `git2` (libgit2 bindings), `clap` (CLI), `serde`/`serde_json` (serialization), `sha2` (hashing), `walkdir` (file traversal), `reqwest` (HTTP), `anyhow`/`thiserror` (error handling), `indicatif`/`console`/`dialoguer` (terminal UI)
- **Languages Supported:** Rust, Go, Java, Python, Swift, Objective-C, Markdown, HTML, TypeScript, TSX, JavaScript

---

## 2. Executive Summary

**kt** is a local Retrieval-Augmented Generation (RAG) system that acts as a knowledge transfer bridge between your local codebase and LLM inference engines. By utilizing **Tree-sitter** for AST-based logical code chunking, **ONNX Runtime** for local embeddings (all-MiniLM-L6-v2, 384-dimensional), and **Redis Stack** for high-speed hybrid vector search, kt allows LLMs to query, understand, and reason about polyglot projects. The entire pipeline is exposed as an **MCP (Model Context Protocol) Server** via the `rmcp` crate, granting AI assistants autonomous tool-calling access to the local codebase. kt also provides a CLI for direct indexing, self-upgrade, and MCP harness configuration.

---

## 3. Architecture & Data Flow

The system operates entirely on the local machine and is divided into three core layers:

### A. The Indexing Engine (Write Path)

- **File Discovery:** Scans the target workspace for supported source and document files using `walkdir`, filtering out common ignored directories (`target`, `vendor`, `.git`, `node_modules`, etc.).
- **AST Parser (Tree-sitter):** Slices files into semantic boundaries (functions, structs, impl blocks, classes, methods, etc.) using language-specific Tree-sitter grammars. Injects parent metadata (e.g., impl block headers, class annotations) into child chunk context for richer embeddings.
- **Local Embeddings:** Passes semantic chunks through `all-MiniLM-L6-v2` via ONNX Runtime (using the `ort` crate) running on CPU to generate 384-dimensional dense vectors. Uses mean pooling over token embeddings with attention masking, followed by L2 normalization.
- **Redis Ingestion:** Stores chunk text, metadata, vectors, and file modification timestamps in Redis Hashes using pipeline batching.

### B. The Storage Engine (Redis Stack)

- **Main Index:** `idx:kt_codebase` on `kt:doc:*` hashes — handles all languages.
- **Shadow Index:** `idx:kt_shadow` on `kt:shadow:*` hashes — ephemeral TTL-based index for PR/change workflows.
- **Hybrid Search:** Combines Dense Vector Search (KNN, semantic intent) with BM25 Keyword Search (exact syntax). Uses Redis DIALECT 2 query syntax.

### C. The Interface Layer (MCP Server + CLI)

- **MCP Server:** Rust binary using `rmcp` crate, communicating via stdio JSON-RPC.
- **CLI:** `clap`-based binary with commands: `serve`, `sync`, `upgrade`, `mcp setup/list/show/remove`.
- **Sync Progress:** Terminal UI with progress bars and animations (when TTY detected).

---

## 4. Redis Schema Design

All data is stored in Redis Hashes with the prefix `kt:doc:`. The schema for the RediSearch index (`idx:kt_codebase`) is as follows:

| Field Name | Type | Description |
|---|---|---|
| `chunk_id` | `TAG` | SHA-256 hash of `filepath \0 name \0 start_line`. |
| `codebase_id` | `TAG` | Stable ID derived from the canonical codebase root path. |
| `filepath` | `TEXT` | Repository-relative path (e.g., `src/main.rs`). |
| `language` | `TAG` | Language filter such as `rust`, `python`, `typescript`, or `markdown`. |
| `node_type` | `TAG` | Canonical AST type: `function`, `struct`, `enum`, `impl`, `trait`, `class`, `interface`, `constructor`, `type_alias`, `const`, `text_block`. |
| `name` | `TEXT` | Extracted identifier (function/struct/class name). |
| `signature` | `TEXT` | First-line signature for the node. |
| `content` | `TEXT` | Raw source code of the chunk + injected parent context. |
| `start_line` | `NUMERIC SORTABLE` | Zero-based start line number in the source file. |
| `end_line` | `NUMERIC` | Zero-based end line number in the source file. |
| `file_role` | `TAG` | File role classification: `implementation`, `test`, `fixture`, `generated`, or `config`. |
| `calls` | `TEXT` | Extracted lightweight call references used by BM25 and related context expansion. |
| `parent_context` | `TEXT` | Container node header (first 3 lines for large containers, full text for small). |
| `embedding` | `VECTOR` | 384-dimensional `FLOAT32` vector, HNSW index, `COSINE` distance. |

An unindexed `mtime` field stores the file modification timestamp for incremental sync.

The shadow index (`idx:kt_shadow`) has an identical schema on `kt:shadow:*` hashes. Each shadow key has a configurable TTL (default: 2 hours).

---

## 5. Exposed MCP Tools

The standard `kt` MCP server exposes 7 public tools. Running `kt serve --debug` exposes the same public tools plus experimental `_debug_*` LSP and feedback tools for local dogfooding.

### 1. `kt_search` (Hybrid Knowledge Search)

- **Inputs:** `query` (string, required), `language` (optional string for any supported language), `top_k` (optional int, default 3, max 10), `headers_only` (optional bool), `directory_path` or `codebase_alias` (optional scope), `file_role` (optional role filter).
- **Behavior:** Embeds the query via ONNX Runtime, executes Redis hybrid search on main + shadow indexes, merges results (shadow takes priority over main chunks with the same ID), resolves one-hop parent context, returns top chunks as structured XML.

### 2. `kt_read_file` (Exact File Lookup)

- **Inputs:** `filepath` (string, required), `directory_path` or `codebase_alias` (optional scope).
- **Behavior:** Bypasses vector search. Queries shadow index first, then main index for all chunks matching the filepath. Returns chunks in source order as structured XML, including zero-based `start_line` and `end_line` attributes when line metadata is available.

### 3. `kt_sync` (Directory Indexing)

- **Inputs:** `directory_path` (string, required), `full` (optional bool), `codebase_alias` (optional alias).
- **Behavior:** Triggers the sync pipeline — full re-index, git-aware partial (diff-based via `git2`), or mtime-based partial. Parses changed files, generates embeddings, updates Redis.

### 4. `kt_git_status` (Repository Status)

- **Inputs:** `directory_path` (string, required).
- **Behavior:** Returns current branch, HEAD commit SHA, dirty state, and list of changed files as structured XML.

### 5. `kt_index_pr` (Shadow Index for PR Workflows)

- **Inputs:** `directory_path` (string, required), `base_branch` (optional string, default `"main"`), `ttl_seconds` (optional u64, default 7200).
- **Behavior:** Computes git diff against base branch, indexes changed files into the shadow index with TTL. Shadow chunks merge with main results in `kt_search`, taking priority over main chunks with the same ID.

### 6. `kt_list_codebases` (Codebase Registry)

- **Inputs:** none.
- **Behavior:** Lists indexed codebase IDs, aliases, root paths, and last sync metadata.

### 7. `kt_query` (Agentic Codebase Query)

- **Inputs:** `question` (string, required), `directory_path` or `codebase_alias` (optional scope), `max_steps` (optional).
- **Behavior:** Runs the built-in query planner over indexed chunks and returns an XML-wrapped answer, evidence, and trace.

---

## 6. CLI Commands

```
kt serve                          Start MCP server (stdio transport)
kt sync <dir> [--full]            Index directory (partial by default)
kt upgrade [--force] [--version]  Self-upgrade from GitHub releases
kt mcp setup [--harness ...] [--global] [--create-agents]
kt mcp list                       List detected MCP harnesses
kt mcp show                       Show global configuration
kt mcp remove <harness>...        Remove kt from harness config
```

---

## 7. Sync Strategies

kt supports three sync strategies, selected automatically:

- **Full Sync:** Discovers all supported files in the directory, removes existing chunks, re-parses and re-embeds everything.
- **Git-Aware Partial Sync** (default for git repos): Compares the current HEAD commit SHA against the last synced commit. If unchanged, skips. If changed, computes a `git diff` tree-to-tree comparison via `git2`, indexes only changed files, and removes deleted files from the index.
- **Mtime-Based Partial Sync** (non-git repos): Compares file modification timestamps against the stored `mtime` values, indexes only files with changed or new timestamps.

The sync module (`src/sync.rs`) exposes a structured pipeline: `plan()` determines strategy and file set, `execute()` runs chunking/embedding/storage with a `SyncProgress` trait for UI feedback, and `finalize()` persists sync state (commit SHA or cleared state).

---

## 8. Implementation Milestones

### Phase 1: Core Foundation

- Rust project setup with `tokio`, `redis`, `clap`.
- Redis CRUD operations, index creation, hybrid search.
- ONNX Runtime embedding engine with `all-MiniLM-L6-v2`.
- Basic CLI with `serve` and `sync` commands.

### Phase 2: Parsing & Indexing

- Tree-sitter integration for supported code and document languages.
- AST-based chunking with parent context injection.
- File discovery with ignored directory filtering.
- Batch embedding and Redis pipeline ingestion.

### Phase 3: MCP Server

- `rmcp`-based MCP server with public search, read, sync, git status, shadow indexing, codebase registry, and query tools.
- XML response formatting with content truncation (8,000 chars per chunk, 32,000 chars total response).
- `headers_only` mode for token savings.

### Phase 4: Git Integration

- `git2`-based partial sync with commit tracking.
- Diff-based change detection for incremental indexing.
- `kt_git_status` tool for repository status.

### Phase 5: PR Workflow

- Shadow index (`idx:kt_shadow`) with per-key TTL.
- `kt_index_pr` tool for indexing changed files from a branch diff.
- Shadow/main result merging with deduplication (shadow takes priority).

### Phase 6: Distribution

- Self-upgrade from GitHub releases via `self-replace`.
- Interactive MCP harness setup (OpenCode, Claude Desktop, Cline, Continue, Pi).
- Global configuration management (`~/.config/kt/config.json`).
- Terminal sync progress UI with progress bars.

---

## 9. Known Risks & Mitigations

- **Risk:** *AST Parsing Failures on Incomplete Code.* If you are mid-edit and syntax is broken, Tree-sitter may fail to generate a complete AST.
  - **Mitigation:** Falls back to line-based chunking (30-line `text_block` chunks) to ensure the code remains searchable.

- **Risk:** *Context Window Saturation.* Over-fetching code chunks will blow out the LLM's context window.
  - **Mitigation:** Per-chunk content is truncated at 8,000 characters. Total response is capped at 32,000 characters. `headers_only` mode returns only signatures for token savings.

- **Risk:** *ONNX Runtime Compatibility.* ONNX Runtime requires native libraries that may not be available on all platforms.
  - **Mitigation:** The `ort` crate with default features downloads pre-built ONNX Runtime libraries. Builds are tested on linux-amd64 and darwin-arm64.
