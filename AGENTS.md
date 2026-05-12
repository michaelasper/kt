# kt (Knowledge Transfer)

A local polyglot codebase RAG via MCP.

## Architecture

- `src/lib.rs` — Core types: `Language`, `Chunk`, `SearchResult`
- `src/codebase.rs` — Codebase identity model (`codebase_id`, alias, canonical root path)
- `src/config.rs` — Config from env vars (`KT_REDIS_URL`, `KT_MODEL_CACHE_DIR`)
- `src/error.rs` — Centralized `KtError` enum (Redis, Ort, Io, ParseFailed, etc.)
- `src/discovery.rs` — File walker with ignored directory filtering
- `src/indexing.rs` — Tree-sitter AST chunker with parent context injection
- `src/indexing/languages.rs` — Per-language Tree-sitter configs (Rust, Go, Java)
- `src/embedding.rs` — ONNX Runtime embedding engine (all-MiniLM-L6-v2, 384-dim)
- `src/storage/` — Redis CRUD, FT.CREATE index, hybrid vector+BM25 search, shadow index, codebase registry
- `src/sync.rs` — Shared sync pipeline: `SyncStrategy`, `SyncPlan`, `SyncStats`, `SyncProgress`
- `src/git.rs` — Git integration via git2 (branch, commit SHA, diff, status)
- `src/mcp.rs` — MCP server with 6 tools (kt_search, kt_read_file, kt_sync, kt_git_status, kt_index_pr, kt_list_codebases)
- `src/mcp_setup.rs` — Interactive MCP harness setup (OpenCode, Claude Desktop, Cline, Continue, Pi)
- `src/global_config.rs` — Global configuration management (`~/.config/kt/config.json`)
- `src/sync_ui.rs` — Terminal sync progress UI (pretty + plain modes)
- `src/upgrade.rs` — Self-upgrader from GitHub releases
- `src/main.rs` — clap CLI: `kt serve`, `kt sync`, `kt upgrade`, `kt mcp setup/list/show/remove`

## Testing

```bash
cargo test              # unit tests
cargo test --test adversarial  # integration tests (requires Redis)
cargo clippy --all-targets --all-features
```

## Using kt

The `kt` MCP server is configured in OpenCode. Use `kt_search` to search all indexed codebases semantically, `kt_read_file` to read file chunks, `kt_sync` to index a directory, `kt_git_status` for branch/commit context, `kt_index_pr` to shadow-index working tree changes, and `kt_list_codebases` to discover indexed roots and aliases.

**Multi-codebase indexing**: Prefer syncing with an alias, e.g. `kt sync --name kt /Users/michaelasper/source/kt`. By default, `kt_search` searches globally across all indexed codebases. Scope `kt_search` or `kt_read_file` with `codebase_alias` or `directory_path` when you need one repo. Unscoped `kt_read_file` returns grouped matches for that repo-relative filepath across all codebases.

**Partial Sync**: `kt_sync` automatically detects git repositories and only syncs changed files (using git2 to compare commits) or files with modified timestamps (for non-git repos). This makes incremental syncs fast. Use `kt sync --full <dir>` to force a complete re-index.

**Schema v2 migration**: `ensure_index` sets `kt:schema_version = 2` and intentionally drops old non-codebase-scoped `kt:doc:*`, `kt:shadow:*`, and legacy sync-state keys. After upgrading to v2, repositories must be re-synced.

When exploring this codebase, prefer using `kt_search` to find relevant code before reading files directly.

## Harness Encouragement

Prefer this execution pattern in evaluations and automated assistants:

- **Start each task with MCP discovery**  
  - `kt_search` with a short query that captures intent before opening files.
  - If search is inconclusive, refine query or try one of `language`/`headers_only` options.
- **Then read only what search returns**  
  - Call `kt_read_file` on returned file paths to inspect exact chunks.
  - Avoid direct `cat/less` before at least one MCP retrieval attempt, unless the exact artifact is already known.
- **Keep working tree changes visible**
  - Run `kt_git_status` for branch/dirtiness context before and during edits.
  - Run `kt_index_pr` after edits so shadow-indexed changes are searchable with branch-local context.
- **Before concluding**  
  - Re-run `kt_search` to validate that the changes/intent are discoverable in results.

Suggested default flow for harnesses:
1) `kt_sync <repo>` with `codebase_alias` when useful (initial baseline - will use partial sync if git repo)
2) `kt_search "<task intent>"`  
3) `kt_read_file "<path>"` for shortlisted files; add `codebase_alias` or `directory_path` if a path is ambiguous
4) edit code  
5) `kt_git_status` (optional) and `kt_index_pr`  
6) `kt_search "<same intent>"` (smoke check)  

Use `kt sync --full <repo>` to force complete re-index.  

This keeps the MCP server the primary source of discovery and improves signal for agent workflows and automated evaluators.
