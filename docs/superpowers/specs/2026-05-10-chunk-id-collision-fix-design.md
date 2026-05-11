# Fix: Chunk ID Collisions for Same-Named Items

**Date**: 2026-05-10
**Issue**: [#2](https://github.com/michaelasper/kt/issues/2)
**Severity**: Critical (data loss)

## Problem

`Chunk::generate_id` hashes only `filepath + name`, so two AST nodes in the same file with the same name (e.g. `impl A::new()` and `impl B::new()`) produce identical chunk IDs. The second write to Redis silently overwrites the first, causing invisible data loss.

A secondary issue: no field separators in the hash input means `"fo"+"obar"` hashes identically to `"foob"+"ar"`, creating low-probability boundary collisions.

## Solution

Change the hash input to `filepath \0 name \0 start_line`:

- `start_line` guarantees uniqueness within a file (no two AST nodes start on the same line)
- `\0` separators prevent boundary collisions
- All data is already available at the call sites

## Changes

### 1. `src/lib.rs` — `Chunk::generate_id`

New signature:

```rust
pub fn generate_id(filepath: &str, name: &str, start_line: usize) -> String
```

Hash input: `filepath.as_bytes() + b"\x00" + name.as_bytes() + b"\x00" + start_line.to_be_bytes()`

### 2. `src/indexing.rs` — Two call sites

- `build_chunk` (~line 140): pass `start_line` (already computed as a local variable)
- `fallback_line_chunks` (~line 279): pass `start_line` (already the loop variable)

### 3. `src/lib.rs` — Unit tests

| Test | Asserts |
|------|---------|
| `generate_id_uniqueness` | Same filepath+name, different start_line produces different IDs |
| `generate_id_stability` | Same inputs always produce the same ID |
| `generate_id_separator_safety` | Boundary-crossing field values produce different IDs |

## Migration

No migration code needed. The existing sync flow provides automatic cleanup:

1. Every sync (full or partial) calls `remove_file_chunks(filepath)` before writing — this searches by `@filepath` and deletes all matching keys regardless of chunk ID format
2. New chunks are written with new-format IDs
3. Old-format chunks for unsynced files continue working (search finds them by filepath/content fields)
4. They get replaced naturally on their next sync

Users should run `kt sync --full <dir>` after upgrading to migrate everything in one pass.

## Impact

- **Breaking**: Chunk IDs change format. Old and new IDs never collide (different hash inputs), so mixed states are safe.
- **Performance**: No change — same single SHA256 hash per chunk.
- **Storage**: No schema changes to Redis.
