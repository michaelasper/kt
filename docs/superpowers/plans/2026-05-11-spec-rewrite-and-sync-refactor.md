# Spec Rewrite + AGENTS.md Update + Sync Pipeline Refactor Implementation Plan

> **Status: COMPLETED** — All 8 tasks implemented and verified. Additional review-driven refinements applied: `SyncPlan.deleted_paths` field added, `execute()` signature changed to accept `&SyncPlan`, `SyncProgress::finish` changed to take `self` by value.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix issue #3 by rewriting spec.md to match the Rust implementation, updating AGENTS.md with all 15+ modules and 5 MCP tools, and extracting the duplicated sync pipeline into a shared `src/sync.rs` module.

**Architecture:** Three independent deliverables — documentation fixes (spec.md, AGENTS.md) and a code refactor (src/sync.rs). The sync refactor extracts the file-discovery strategy, per-file embed→store loop, and sync-state finalization from both `main.rs::run_sync()` and `mcp.rs::kt_sync()` into free functions in a new `src/sync.rs` module, with progress reporting via a `SyncProgress` trait.

**Tech Stack:** Rust, tokio async, Redis, ONNX Runtime, Tree-sitter, git2, clap, rmcp

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `spec.md` | Rewrite | Accurate Rust implementation specification |
| `AGENTS.md` | Modify | Complete module/tool documentation |
| `src/sync.rs` | Create | Shared sync pipeline: `SyncStrategy`, `SyncPlan`, `SyncStats`, `SyncProgress` trait, `plan()`, `execute()`, `finalize()` |
| `src/lib.rs` | Modify | Add `pub mod sync;` |
| `src/main.rs` | Modify | Replace `run_sync()` body with `sync::plan/execute/finalize` calls |
| `src/mcp.rs` | Modify | Replace `kt_sync()` body with `sync::plan/execute/finalize` calls |

---

### Task 1: Create `src/sync.rs` with types and `plan()`

**Files:**
- Create: `src/sync.rs`
- Modify: `src/lib.rs` (add `pub mod sync;`)

- [x] **Step 1: Add `pub mod sync;` to `src/lib.rs`**

In `src/lib.rs`, add `pub mod sync;` after the existing module declarations (after line 12, before `pub use config::Config;`):

```rust
pub mod sync;
```

- [x] **Step 2: Create `src/sync.rs` with types and `plan()` function**

Create `src/sync.rs` with the `SyncStrategy` enum, `SyncPlan` struct, `SyncStats` struct, `SyncProgress` trait, and the `plan()` function extracted from the current `main.rs:121-209` / `mcp.rs:215-295`:

```rust
use crate::discovery::{self, DiscoveredFile};
use crate::git;
use crate::storage::Storage;
use std::collections::HashSet;
use std::path::Path;

pub enum SyncStrategy {
    Full,
    PartialGit {
        prev_commit: String,
        current_commit: String,
    },
    PartialMtime,
}

pub struct SyncPlan {
    pub files: Vec<DiscoveredFile>,
    pub strategy: SyncStrategy,
}

pub struct SyncStats {
    pub total_files: usize,
    pub total_chunks: usize,
    pub errors: usize,
}

pub trait SyncProgress: Send + Sync {
    fn start_file(&mut self, path: &str, index: usize);
    fn finish_file(&mut self, path: &str, chunks: usize);
    fn finish(&mut self, files: usize, chunks: usize);
}

pub struct NoopProgress;

impl SyncProgress for NoopProgress {
    fn start_file(&mut self, _path: &str, _index: usize) {}
    fn finish_file(&mut self, _path: &str, _chunks: usize) {}
    fn finish(&mut self, _files: usize, _chunks: usize) {}
}

pub async fn plan(root: &Path, storage: &Storage, full: bool) -> anyhow::Result<SyncPlan> {
    if full {
        tracing::info!("Full sync requested (--full flag)");
        return Ok(SyncPlan {
            files: discovery::discover_files(root),
            strategy: SyncStrategy::Full,
        });
    }

    if git2::Repository::discover(root).is_ok() {
        tracing::info!("Git repository detected, using git-aware partial sync");

        let git_info = git::get_git_info(root)?;
        let current_commit = match git_info.commit_sha {
            Some(sha) => sha,
            None => {
                tracing::warn!("No commit SHA found (detached HEAD?), falling back to full sync");
                return Ok(SyncPlan {
                    files: discovery::discover_files(root),
                    strategy: SyncStrategy::Full,
                });
            }
        };

        let dir_str = root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in directory path"))?;
        let last_commit = storage.get_last_synced_commit(dir_str).await?;

        match last_commit {
            None => {
                tracing::info!("No previous sync found, performing full sync");
                Ok(SyncPlan {
                    files: discovery::discover_files(root),
                    strategy: SyncStrategy::Full,
                })
            }
            Some(last) if last == current_commit => {
                tracing::info!("Already up to date (commit: {})", current_commit);
                Ok(SyncPlan {
                    files: vec![],
                    strategy: SyncStrategy::PartialGit {
                        prev_commit: last,
                        current_commit,
                    },
                })
            }
            Some(last) => {
                tracing::info!(
                    "Changes detected ({} -> {}), performing partial sync",
                    &last[..8],
                    &current_commit[..8]
                );

                match git::get_diff_files(root, &last) {
                    Ok(changed_paths) => {
                        for path in &changed_paths {
                            if !root.join(path).exists() {
                                tracing::info!("Removing deleted file from index: {}", path);
                                if let Err(e) = storage.remove_file_chunks(path).await {
                                    tracing::warn!(
                                        "Failed to remove chunks for deleted file {}: {e}",
                                        path
                                    );
                                }
                            }
                        }

                        let changed_set: HashSet<_> = changed_paths.into_iter().collect();

                        let all_files = discovery::discover_files(root);
                        let changed_files: Vec<_> = all_files
                            .into_iter()
                            .filter(|f| changed_set.contains(&f.relative_path))
                            .collect();

                        if changed_files.is_empty() {
                            tracing::info!("No supported files in changed set");
                        } else {
                            tracing::info!(
                                "Found {} changed files to index",
                                changed_files.len()
                            );
                        }

                        Ok(SyncPlan {
                            files: changed_files,
                            strategy: SyncStrategy::PartialGit {
                                prev_commit: last,
                                current_commit,
                            },
                        })
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to compute diff ({e}), falling back to full sync"
                        );
                        Ok(SyncPlan {
                            files: discovery::discover_files(root),
                            strategy: SyncStrategy::Full,
                        })
                    }
                }
            }
        }
    } else {
        tracing::info!("Not a git repository, using mtime-based partial sync");

        let known_mtimes = storage.get_file_mtimes().await?;
        Ok(SyncPlan {
            files: discovery::discover_modified_files(root, &known_mtimes),
            strategy: SyncStrategy::PartialMtime,
        })
    }
}
```

- [x] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors (warnings about unused `SyncStats`/`SyncProgress` are fine for now)

- [x] **Step 4: Commit**

```bash
git add src/sync.rs src/lib.rs
git commit -m "feat(sync): add sync module with SyncStrategy, SyncPlan, and plan()"
```

---

### Task 2: Add `execute()` and `finalize()` to `src/sync.rs`

**Files:**
- Modify: `src/sync.rs`

- [x] **Step 1: Add `execute()` and `finalize()` functions**

Append to `src/sync.rs` after the `plan()` function:

```rust
pub async fn execute(
    files: &[DiscoveredFile],
    storage: &Storage,
    engine: &crate::embedding::EmbeddingEngine,
    progress: &mut dyn SyncProgress,
) -> anyhow::Result<SyncStats> {
    let mut total_chunks = 0usize;
    let mut total_files = 0usize;
    let mut errors = 0usize;

    for (i, file) in files.iter().enumerate() {
        let chunks = crate::indexing::parse_file(&file.path, &file.relative_path, file.language);
        if chunks.is_empty() {
            continue;
        }

        progress.start_file(&file.relative_path, i);

        if let Err(e) = storage.remove_file_chunks(&file.relative_path).await {
            tracing::warn!("Failed to clean old chunks for {}: {e}", file.relative_path);
        }

        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        match engine.embed_batch(&texts) {
            Ok(embeddings) => {
                let mtime = discovery::get_file_mtime(&file.path).unwrap_or_default();
                let mtimes = vec![mtime; chunks.len()];

                if let Err(e) = storage
                    .store_chunks_batch_with_mtimes(&chunks, &embeddings, &mtimes)
                    .await
                {
                    tracing::warn!("Failed to store chunks for {}: {e}", file.relative_path);
                    errors += 1;
                    continue;
                }
                total_chunks += chunks.len();
                total_files += 1;
            }
            Err(e) => {
                tracing::warn!("Failed to embed chunks for {}: {e}", file.relative_path);
                errors += 1;
            }
        }

        progress.finish_file(&file.relative_path, chunks.len());
    }

    Ok(SyncStats {
        total_files,
        total_chunks,
        errors,
    })
}

pub async fn finalize(
    root: &Path,
    strategy: &SyncStrategy,
    storage: &Storage,
) -> anyhow::Result<()> {
    let dir_str = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in directory path"))?;

    match strategy {
        SyncStrategy::Full => {
            storage.clear_sync_state(dir_str).await?;
            tracing::debug!("Cleared sync state after full sync");
        }
        SyncStrategy::PartialGit { .. } => {
            if let Ok(git_info) = git::get_git_info(root) {
                if let Some(commit) = git_info.commit_sha {
                    storage.set_last_synced_commit(dir_str, &commit).await?;
                    tracing::debug!("Updated last synced commit to {}", &commit[..8]);
                }
            }
        }
        SyncStrategy::PartialMtime => {}
    }

    Ok(())
}
```

- [x] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [x] **Step 3: Commit**

```bash
git add src/sync.rs
git commit -m "feat(sync): add execute() and finalize() to sync pipeline"
```

---

### Task 3: Refactor `main.rs::run_sync()` to use `sync` module

**Files:**
- Modify: `src/main.rs`

- [x] **Step 1: Replace `run_sync()` body**

Replace the entire `run_sync` function (lines 96-267 of `src/main.rs`) with:

```rust
struct CliProgress {
    ui: kt::sync_ui::SyncUI,
}

impl kt::sync::SyncProgress for CliProgress {
    fn start_file(&mut self, path: &str, index: usize) {
        self.ui.start_file(path, index);
    }

    fn finish_file(&mut self, path: &str, chunks: usize) {
        self.ui.finish_file(path, chunks);
    }

    fn finish(&mut self, files: usize, chunks: usize) {
        self.ui.finish(files, chunks);
    }
}

async fn run_sync(
    config: &kt::config::Config,
    directory: &std::path::Path,
    full: bool,
) -> anyhow::Result<()> {
    let is_tty = std::io::stdout().is_terminal();

    let default_level = if is_tty { "kt=warn" } else { "kt=info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::default()
            .add_directive(default_level.parse().unwrap())
            .add_directive("ort=warn".parse().unwrap())
    });

    tracing_subscriber::fmt().with_env_filter(filter).init();

    if !directory.exists() {
        anyhow::bail!("Directory not found: {}", directory.display());
    }

    let storage = kt::storage::Storage::new(config)?;
    storage.ensure_index().await?;

    let engine = kt::embedding::EmbeddingEngine::new(config).await?;

    let plan = kt::sync::plan(directory, &storage, full).await?;

    if plan.files.is_empty() {
        tracing::info!("No supported files found to sync");
        return Ok(());
    }

    let mut progress = CliProgress {
        ui: kt::sync_ui::SyncUI::new(plan.files.len()),
    };

    let stats = kt::sync::execute(&plan.files, &storage, &engine, &mut progress).await?;
    kt::sync::finalize(directory, &plan.strategy, &storage).await?;

    progress.finish(stats.total_files, stats.total_chunks);

    Ok(())
}
```

- [x] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [x] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "refactor(cli): use shared sync pipeline in run_sync()"
```

---

### Task 4: Refactor `mcp.rs::kt_sync()` to use `sync` module

**Files:**
- Modify: `src/mcp.rs`

- [x] **Step 1: Replace `kt_sync()` body**

Replace the `kt_sync` method (lines 195-373 of `src/mcp.rs`) with:

```rust
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
        let stats =
            crate::sync::execute(&plan.files, &storage, engine, &mut progress)
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
```

- [x] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors. The `use std::collections::HashSet;` import in `mcp.rs` is no longer needed by `kt_sync` but is still used by `merge_and_deduplicate` and `deduplicate_results`, so no import changes needed.

- [x] **Step 3: Commit**

```bash
git add src/mcp.rs
git commit -m "refactor(mcp): use shared sync pipeline in kt_sync()"
```

---

### Task 5: Run full test suite and clippy

**Files:** None

- [x] **Step 1: Run unit tests**

Run: `cargo test`
Expected: all existing tests pass. No tests should have changed behavior.

- [x] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features`
Expected: no errors or warnings

- [x] **Step 3: Fix any issues found**

If clippy or tests report issues, fix them and re-run.

---

### Task 6: Rewrite `spec.md`

**Files:**
- Modify: `spec.md`

- [x] **Step 1: Replace the entire contents of `spec.md`**

The new spec must accurately describe the Rust implementation. Replace the full file with content covering:

1. **Meta Information**: Rust project, key crates (`ort`, `redis`, `rmcp`, `tree-sitter`, `git2`, `clap`, `tokio`)
2. **Executive Summary**: Local RAG system using Tree-sitter AST chunking, ONNX Runtime embeddings (all-MiniLM-L6-v2, 384-dim), Redis Stack hybrid vector+BM25 search, MCP server via `rmcp` on stdio
3. **Architecture & Data Flow**: Three layers
   - **Indexing Engine**: File discovery via `walkdir`, AST parsing via Tree-sitter with parent context injection, ONNX Runtime embedding, Redis ingestion with batch pipeline
   - **Storage Engine**: Redis Stack with main index (`idx:kt_codebase`) and shadow index (`idx:kt_shadow`), hybrid vector+BM25 search
   - **Interface Layer**: Rust MCP server via `rmcp` on stdio, clap CLI with `serve`, `sync`, `upgrade`, `mcp` commands
4. **Redis Schema Design**: 11-field table:

   | Field | Type | Description |
   |-------|------|-------------|
   | `chunk_id` | `TAG` | SHA-256 hash of `filepath \0 name \0 start_line` |
   | `filepath` | `TEXT` | Repository-relative path |
   | `language` | `TAG` | `rust`, `go`, or `java` |
   | `node_type` | `TAG` | Canonical AST type: `function`, `struct`, `enum`, `impl`, `trait`, `class`, `interface`, `constructor`, `type_alias`, `const`, `text_block` |
   | `name` | `TEXT` | Extracted identifier name |
   | `signature` | `TEXT` | First-line signature for the node |
   | `content` | `TEXT` | Source code + injected parent context |
   | `start_line` | `NUMERIC` | Start line in file |
   | `end_line` | `NUMERIC` | End line in file |
   | `parent_context` | `TEXT` | Container node header (first 3 lines) |
   | `embedding` | `VECTOR` | 384-dim FLOAT32, FLAT index, COSINE distance |

   Plus unindexed `mtime` field for sync state.

5. **Shadow Index**: Identical schema, `idx:kt_shadow` / `kt:shadow:` prefix, TTL-based expiration (default 2h)
6. **MCP Tools**: All 5 tools with full parameter descriptions:
   - `kt_search` — `query`, `language?`, `top_k?`, `headers_only?`
   - `kt_read_file` — `filepath`
   - `kt_sync` — `directory_path`, `full?`
   - `kt_git_status` — `directory_path`
   - `kt_index_pr` — `directory_path`, `base_branch?`, `ttl_seconds?`
7. **CLI Commands**: `kt serve`, `kt sync <dir> [--full]`, `kt upgrade [--force] [--version]`, `kt mcp setup/list/show/remove`
8. **Sync Strategies**: Full re-index, git-aware partial (diff via `git2`), mtime-based partial (for non-git repos)
9. **Implementation Milestones**: Accurate history (Phase 1: Rust core + Redis, Phase 2: Tree-sitter + ONNX, Phase 3: MCP server + CLI, Phase 4: Git integration + partial sync, Phase 5: Shadow index + PR workflow, Phase 6: Self-upgrade + MCP setup)
10. **Known Risks**: AST parsing on incomplete code (fallback to 30-line text blocks), context window saturation (8000 chars/chunk, 32000 total), ONNX Runtime compatibility across platforms

- [x] **Step 2: Verify no Python references remain**

Search `spec.md` for: `python`, `sentence-transformers`, `redis-py`, `@modelcontextprotocol/sdk`, `redis-py`, `pip`
Expected: zero matches

- [x] **Step 3: Commit**

```bash
git add spec.md
git commit -m "docs: rewrite spec.md to accurately reflect Rust implementation (#3)"
```

---

### Task 7: Update `AGENTS.md`

**Files:**
- Modify: `AGENTS.md`

- [x] **Step 1: Update the Architecture section**

Replace the current Architecture section (lines 5-15) with:

```markdown
## Architecture

- `src/lib.rs` — Core types: `Language`, `Chunk`, `SearchResult`
- `src/config.rs` — Config from env vars (`KT_REDIS_URL`, `KT_MODEL_CACHE_DIR`)
- `src/error.rs` — Centralized `KtError` enum (Redis, Ort, Io, ParseFailed, etc.)
- `src/discovery.rs` — File walker with ignored directory filtering
- `src/indexing.rs` — Tree-sitter AST chunker with parent context injection
- `src/indexing/languages.rs` — Per-language Tree-sitter configs (Rust, Go, Java)
- `src/embedding.rs` — ONNX Runtime embedding engine (all-MiniLM-L6-v2, 384-dim)
- `src/storage.rs` — Redis CRUD, FT.CREATE index, hybrid vector+BM25 search, shadow index
- `src/sync.rs` — Shared sync pipeline: `SyncStrategy`, `SyncPlan`, `SyncStats`, `SyncProgress`
- `src/git.rs` — Git integration via git2 (branch, commit SHA, diff, status)
- `src/mcp.rs` — MCP server with 5 tools (kt_search, kt_read_file, kt_sync, kt_git_status, kt_index_pr)
- `src/mcp_setup.rs` — Interactive MCP harness setup (OpenCode, Claude Desktop, Cline, Continue, Pi)
- `src/global_config.rs` — Global configuration management (`~/.config/kt/config.json`)
- `src/sync_ui.rs` — Terminal sync progress UI (pretty + plain modes)
- `src/upgrade.rs` — Self-upgrader from GitHub releases
- `src/main.rs` — clap CLI: `kt serve`, `kt sync`, `kt upgrade`, `kt mcp setup/list/show/remove`
```

- [x] **Step 2: Update the MCP tools reference**

In the "Using kt" section, update the MCP tools description from:

```
Use `kt_search` to search the codebase semantically, `kt_read_file` to read file chunks, and `kt_sync` to index a directory.
```

to:

```
Use `kt_search` to search the codebase semantically, `kt_read_file` to read file chunks, `kt_sync` to index a directory, `kt_git_status` for branch/commit context, and `kt_index_pr` to shadow-index working tree changes.
```

- [x] **Step 3: Commit**

```bash
git add AGENTS.md
git commit -m "docs: update AGENTS.md with all 16 modules and 5 MCP tools (#3)"
```

---

### Task 8: Final verification

**Files:** None

- [x] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass

- [x] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features`
Expected: no errors or warnings

- [x] **Step 3: Verify the codebase compiles cleanly**

Run: `cargo check`
Expected: compiles successfully

- [x] **Step 4: Verify no sync logic duplication remains**

Search `src/main.rs` for `discover_files` and `discover_modified_files` — they should NOT appear (only `sync::plan` should be called).
Search `src/mcp.rs` for `discover_files` and `discover_modified_files` — they should NOT appear in `kt_sync` (only `sync::plan` should be called).

Expected: no direct discovery calls in the sync functions of either file.

---

## Post-Implementation Review Changes

After all 8 tasks were completed, a code review identified three improvements that were applied in commit `bfb3896`:

1. **`SyncPlan.deleted_paths`** — `plan()` previously called `storage.remove_file_chunks()` as a side effect during planning. This was moved into `execute()`: `plan()` now collects deleted file paths into `SyncPlan.deleted_paths`, and `execute()` removes their chunks before indexing. This makes `plan()` a pure function with no storage mutations.

2. **`SyncProgress::finish(self)`** — Changed from `finish(&mut self)` to `finish(self) where Self: Sized`. This lets implementors consume resources on completion (e.g., `SyncUI::finish(self)` takes ownership). `CliProgress` in `main.rs` now wraps `SyncUI` directly instead of `Option<SyncUI>`.

3. **`execute(plan: &SyncPlan, ...)`** — Changed from `execute(files: &[DiscoveredFile], ...)` to accept the full plan. This gives `execute()` access to `deleted_paths` and keeps the plan as a single unit of work.
