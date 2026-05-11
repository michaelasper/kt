# Storage Layer Refactor — N+1 Fix & Maintainability

Addresses: https://github.com/michaelasper/kt/issues/6

## Problem

`storage.rs` has three categories of issues:

1. **N+1 query pattern**: `get_file_mtimes()` issues 2 individual HGET calls per chunk key, resulting in 20k+ Redis round trips for a 10k-chunk codebase.
2. **Code duplication**: Four near-identical store functions (with/without mtime), duplicated main/shadow index creation, duplicated search logic.
3. **Dead code**: `get_indexed_files()` has zero callers.
4. **Monolithic file**: 1100+ lines in a single `storage.rs` with no internal organization.

## Design

### 1. Module Split

Replace `src/storage.rs` with `src/storage/` directory:

```
src/storage/
├── mod.rs          (~150 lines) Storage struct, connection, index lifecycle,
│                                 sync state, get_file_mtimes, re-exports
├── commands.rs     (~200 lines) Unified store/remove/lookup chunk operations
├── search.rs       (~250 lines) Hybrid search, file reads, result parsing, query helpers
└── index.rs        (~80 lines)  Schema definitions, index creation, FT.ALTER migration
```

All public methods remain on `impl Storage` blocks spread across files via standard Rust module convention. `mod.rs` re-exports everything needed by external callers.

**Module responsibilities:**
- `mod.rs` — `Storage` struct, `connection()`, `ensure_index()`, `ensure_shadow_index()`, sync state (`get/set/clear_last_synced_commit`), `get_file_mtimes()` (fixed)
- `commands.rs` — `store_chunk()`, `store_chunks_batch()`, `remove_file_chunks()`, `lookup_chunks_by_name()`, shadow store
- `search.rs` — `hybrid_search()`, `read_file_chunks()`, `search_shadow()`, `read_shadow_file_chunks()`, `parse_search_results()`, query escape helpers
- `index.rs` — Schema constants, `create_index()` helper, `create_shadow_index()` helper, `alter_add_mtime_field()`

### 2. N+1 Fix — Two Phases

#### Phase 1: Pipeline Batching (Immediate)

Replace the per-key HGET loop in `get_file_mtimes()`:

**Before:**
```
SCAN batch → for each key: HGET filepath + HGET mtime  (2N round trips)
```

**After:**
```
SCAN batch → pipeline { HMGET key filepath mtime, ... }  (1 round trip per batch)
```

For 10k chunks with SCAN COUNT 100: ~100 SCAN iterations + ~100 pipeline calls = ~200 round trips (down from 20k+). A ~100x improvement.

#### Phase 2: FT.ALTER + FT.AGGREGATE (Primary Path)

1. Add `mtime` as `TEXT` field to the schema definition in `index.rs`
2. On `ensure_index()`, run `FT.ALTER idx:kt_codebase SCHEMA ADD mtime TEXT` after index creation/verification. This is idempotent — silently handles "field already exists" errors.
3. Replace `get_file_mtimes()` primary implementation with:
   ```
   FT.AGGREGATE idx:kt_codebase *
     LOAD 2 @filepath @mtime
     GROUPBY 1 @filepath
     REDUCE FIRST_VALUE 1 @mtime AS mtime
   ```
   Single round trip returning deduplicated filepath-to-mtime mappings.
4. Keep pipeline batching as a fallback method, used if FT.AGGREGATE fails (e.g., index not yet migrated).

### 3. Store Function Consolidation

**Current (4 functions):**
- `store_chunk(chunk, embedding)`
- `store_chunk_with_mtime(chunk, embedding, mtime)`
- `store_chunks_batch(chunks, embeddings)`
- `store_chunks_batch_with_mtimes(chunks, embeddings, mtimes)`

**Proposed (2 functions):**
- `store_chunk(chunk, embedding, mtime: Option<&str>)` — when `None`, mtime field omitted from HSET
- `store_chunks_batch(chunks, embeddings, mtimes: Option<&[String]>)` — when `None`, mtime omitted for all

Shadow store (`store_shadow_chunks_batch`) stays separate since it has distinct TTL/EXPIRE logic.

### 4. Search Logic DRY

Extract shared query-building into private helpers in `search.rs`:

- `build_hybrid_query(query_text, language, top_k) -> String` — shared by `hybrid_search` and `search_shadow`
- `build_file_query(filepath) -> String` — shared by `read_file_chunks` and `read_shadow_file_chunks`

Main and shadow variants call these helpers, differing only in the index name parameter.

### 5. Index Creation DRY

Extract a `create_index(conn, index_name, key_prefix, include_mtime)` helper in `index.rs` that both `ensure_index()` and `ensure_shadow_index()` call. Schema fields are defined once as a builder function.

The `mtime` field is added to the main index schema only. Shadow index does not need mtime (shadow data is ephemeral).

### 6. Dead Code Removal

- Delete `get_indexed_files()` entirely (zero callers in codebase)

### 7. Caller Updates

`sync.rs` callers updated to use new unified signatures:
- `storage.store_chunks_batch_with_mtimes(...)` → `storage.store_chunks_batch(..., Some(&mtimes))`

## File Impact

| File | Change |
|------|--------|
| `src/storage.rs` | Deleted, replaced by `src/storage/` module directory |
| `src/storage/mod.rs` | New — Storage struct, connection, sync state, get_file_mtimes |
| `src/storage/commands.rs` | New — Unified store operations |
| `src/storage/search.rs` | New — Search, read, parse, query helpers |
| `src/storage/index.rs` | New — Schema, index creation, FT.ALTER migration |
| `src/sync.rs` | Updated — new store function signatures |

## Risks

- **Index migration**: FT.ALTER is available in RediSearch 2.0+. If running against an older Redis without RediSearch module, the pipeline fallback covers this.
- **Module split**: Mechanical change but requires careful re-export to avoid breaking external imports. All public API surface stays identical.
- **Store consolidation**: Callers pass `None` where they previously called the non-mtime variant. Trivial but needs to be verified across all call sites.
