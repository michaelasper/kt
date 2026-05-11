# Design: Spec Rewrite + AGENTS.md Update + Sync Pipeline Refactor

**Issue**: [#3 — spec.md is severely outdated and contradicts actual implementation](https://github.com/michaelasper/kt/issues/3)

**Date**: 2026-05-11

## Problem Statement

Three interrelated issues:

1. **spec.md describes a Python system** that no longer exists. The actual implementation is Rust using `ort`, `redis`, and `rmcp` crates. The spec references `sentence-transformers`, `redis-py`, and `@modelcontextprotocol/sdk` — all Python libraries. It documents only 3 MCP tools (actual: 5), 6 Redis schema fields (actual: 11), and 9 source modules (actual: 15).

2. **AGENTS.md is incomplete**. It lists 9 source files but the codebase has 15. It says the MCP server has "3 tools" when it has 5. CLI description is outdated.

3. **Sync pipeline duplication**. The sync logic (file discovery strategy, per-file parse→embed→store loop, sync state management) is duplicated between `main.rs::run_sync()` (~170 lines) and `mcp.rs::kt_sync()` (~180 lines). These have drifted in error handling and will continue to diverge.

## Solution Overview

Three deliverables:

| # | Deliverable | Scope |
|---|------------|-------|
| 1 | Rewrite `spec.md` | Replace entire file with accurate Rust implementation spec |
| 2 | Update `AGENTS.md` | Document all 15+ modules, 5 tools, full CLI |
| 3 | Extract `src/sync.rs` | Shared `SyncPipeline` struct used by both CLI and MCP |

## Deliverable 1: spec.md Rewrite

### Content

Replace the entire file. New spec covers:

- **Meta Information**: Rust project, crates used (`ort`, `redis`, `rmcp`, `tree-sitter`, `git2`, `clap`)
- **Executive Summary**: Local RAG via MCP, Tree-sitter AST chunking, Redis hybrid vector+BM25 search, ONNX Runtime embeddings
- **Architecture**: Three layers — Indexing Engine (Tree-sitter + ONNX), Storage Engine (Redis Stack with main + shadow indexes), Interface Layer (MCP server via `rmcp` on stdio)
- **Redis Schema**: 11 fields — `chunk_id` (TAG), `filepath` (TEXT), `language` (TAG), `node_type` (TAG), `name` (TEXT), `signature` (TEXT), `content` (TEXT), `start_line` (NUMERIC), `end_line` (NUMERIC), `parent_context` (TEXT), `embedding` (VECTOR FLAT FLOAT32 384 COSINE). Plus `mtime` field stored but not indexed.
- **Shadow Index**: Identical schema under `idx:kt_shadow` with `kt:shadow:` prefix. TTL-based expiration (default 2 hours).
- **MCP Tools**: All 5 tools with full parameter schemas
- **CLI Commands**: `serve`, `sync`, `upgrade`, `mcp setup/list/show/remove`
- **Sync Strategies**: Full, git-aware partial (diff-based), mtime-based partial
- **Implementation Milestones**: Accurate history of what was built
- **Known Risks**: Updated for Rust implementation realities

### No Python References

The new spec contains zero references to Python, `sentence-transformers`, `redis-py`, or `@modelcontextprotocol/sdk`.

## Deliverable 2: AGENTS.md Update

### Architecture Section

Add 6 missing modules:

```
- `src/error.rs` — Centralized KtError enum with 9 variants
- `src/git.rs` — Git integration via git2 (branch, commit, diff, status)
- `src/sync_ui.rs` — Terminal sync progress UI (pretty + plain modes)
- `src/upgrade.rs` — Self-upgrader from GitHub releases
- `src/mcp_setup.rs` — Interactive MCP harness setup (5 harnesses)
- `src/global_config.rs` — Global configuration management
```

After refactor, also add:

```
- `src/sync.rs` — Shared sync pipeline (SyncStrategy, SyncPlan, SyncPipeline)
```

### MCP Tools

Change from "3 tools (kt_search, kt_read_file, kt_sync)" to "5 tools (kt_search, kt_read_file, kt_sync, kt_git_status, kt_index_pr)".

### CLI Description

Change from "`kt sync <dir>` or `kt serve`" to "`kt serve`, `kt sync <dir>`, `kt upgrade`, `kt mcp setup/list/show/remove`".

## Deliverable 3: Sync Pipeline Refactor

### New Module: `src/sync.rs`

#### Types

```rust
pub enum SyncStrategy {
    Full,
    PartialGit { prev_commit: String, current_commit: String },
    PartialMtime,
}

pub struct SyncPlan {
    pub files: Vec<DiscoveredFile>,
    pub strategy: SyncStrategy,
    pub deleted_paths: Vec<String>,
}

pub struct SyncStats {
    pub total_files: usize,
    pub total_chunks: usize,
    pub errors: usize,
}

pub trait SyncProgress: Send + Sync {
    fn start_file(&mut self, path: &str, index: usize);
    fn finish_file(&mut self, path: &str, chunks: usize);
    fn finish(self, files: usize, chunks: usize) where Self: Sized;
}
```

#### SyncPipeline Functions

Three free functions (no state to encapsulate):

**`plan(root: &Path, storage: &Storage, full: bool) -> Result<SyncPlan>`**

Determines which files to sync and how:
1. If `full`: `discover_files(root)`, strategy = `Full`
2. Else if git repo detected: get git info → compare commits → diff files → filter to supported extensions. Collects deleted paths into `SyncPlan.deleted_paths`. Strategy = `PartialGit { prev_commit, current_commit }`. Falls back to full if no previous sync or diff fails.
3. Else: get known mtimes from Redis → `discover_modified_files(root, &mtimes)`. Strategy = `PartialMtime`.

`plan()` is a pure function — it does not mutate Redis state. Deleted-file cleanup is deferred to `execute()`.

**`execute(plan: &SyncPlan, storage: &Storage, engine: &EmbeddingEngine, progress: &mut dyn SyncProgress) -> Result<SyncStats>`**

First removes chunks for `plan.deleted_paths`, then for each file in `plan.files`:
1. Parse with `indexing::parse_file`
2. Remove old chunks with `storage.remove_file_chunks`
3. Embed with `engine.embed_batch`
4. Store with `storage.store_chunks_batch_with_mtimes`
5. Report progress via `progress.start_file()` / `progress.finish_file()`

Returns `SyncStats` with totals.

**`finalize(root: &Path, strategy: &SyncStrategy, storage: &Storage) -> Result<()>`**

Updates sync state:
- `Full`: `storage.clear_sync_state()`
- `PartialGit`: `storage.set_last_synced_commit()`
- `PartialMtime`: no-op

#### Progress Implementations

- `NoopProgress` — does nothing (used by MCP tool)
- `CliProgress` — wraps `sync_ui::SyncUI` directly (used by CLI). `finish(self)` consumes the progress tracker.

The existing `SyncUI` enum stays in `sync_ui.rs`. `UiProgress` is a thin adapter in `main.rs`.

#### Caller Changes

**`main.rs::run_sync()`** becomes:

```rust
async fn run_sync(config: &Config, directory: &Path, full: bool) -> anyhow::Result<()> {
    // tracing setup (stays)
    // validation (stays)
    // init storage + engine (stays)

    let plan = kt::sync::plan(directory, &storage, full).await?;

    let mut progress = CliProgress {
        ui: kt::sync_ui::SyncUI::new(plan.files.len()),
    };

    let stats = kt::sync::execute(&plan, &storage, &engine, &mut progress).await?;
    kt::sync::finalize(directory, &plan.strategy, &storage).await?;

    progress.finish(stats.total_files, stats.total_chunks);
    Ok(())
}
```

**`mcp.rs::kt_sync()`** becomes:

```rust
async fn kt_sync(&self, params: SyncParams) -> Result<CallToolResult, rmcp::ErrorData> {
    self.ensure_ready().await.map_err(mcp_error)?;
    // validation (stays)

    let plan = kt::sync::plan(root, &storage, full).await.map_err(mcp_error)?;
    let stats = kt::sync::execute(&plan, &storage, &engine, &mut NoopProgress).await.map_err(mcp_error)?;
    kt::sync::finalize(root, &plan.strategy, &storage).await.map_err(mcp_error)?;

    // format XML result
}
```

#### Performance Impact

None. Same operations in the same order. The refactor is purely structural — extracting existing logic into shared functions.

#### Error Handling

- `plan()` returns `anyhow::Result` — callers wrap into their error types
- `execute()` returns `anyhow::Result<SyncStats>` — errors per-file are logged and counted, not propagated
- `finalize()` returns `anyhow::Result` — logged on failure but non-fatal

This matches the current behavior where CLI uses `?` propagation and MCP logs and counts errors.

### Files Changed

| File | Change |
|------|--------|
| `spec.md` | Full rewrite |
| `AGENTS.md` | Update architecture, tools, CLI sections |
| `src/sync.rs` | New file with `SyncStrategy`, `SyncPlan`, `SyncStats`, `SyncProgress`, `plan()`, `execute()`, `finalize()` |
| `src/lib.rs` | Add `pub mod sync;` |
| `src/main.rs` | Replace `run_sync()` body with calls to `sync::plan/execute/finalize` |
| `src/mcp.rs` | Replace `kt_sync()` body with calls to `sync::plan/execute/finalize` |

### Testing

- Existing unit tests in `lib.rs`, `mcp.rs` unchanged
- Integration tests (`tests/adversarial.rs`) unchanged
- Add unit tests in `src/sync.rs` for `SyncStrategy` determination logic
- `cargo test` + `cargo clippy --all-targets --all-features` must pass
