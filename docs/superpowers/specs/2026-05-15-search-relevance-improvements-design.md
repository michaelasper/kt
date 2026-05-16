# Search Relevance Improvements

Addresses GitHub issue #58: AI feedback requesting better implementation-over-test ranking, flow/sequence discovery, and less boilerplate in search results.

## Problem Statement

When searching a large codebase with `kt_search`, three issues degrade result quality:

1. **Test code dominates results** — Every search hit returns `*Test.java` files first. The chunker treats all files identically; there is no `file_role` classification or ranking signal.
2. **No flow/sequence understanding** — Finding a multi-step flow (e.g., `create → boot → check`) requires 4+ searches with different phrasings. Search returns isolated symbols with no awareness of call relationships.
3. **Boilerplate pollutes embeddings** — Import statements, blank lines, and package declarations consume embedding tokens that should represent actual logic, reducing semantic match quality.

## Design

Three improvements, delivered together as schema v3.

### 1. File Role Classification (Test vs. Production)

**Goal**: Implementation code ranks higher than test code by default.

#### Data model

Add a `FileRole` enum to `src/lib.rs`:

```rust
enum FileRole {
    Implementation,
    Test,
    Fixture,
    Generated,
    Config,
}
```

Add `file_role: FileRole` to the `Chunk` struct (default: `Implementation`).

#### Detection rules

Classify during file discovery in `src/discovery.rs` using path and naming conventions per language:

| Role | Patterns |
|------|----------|
| **Test** | Rust: `tests/`, `*_test.rs`, `mod tests`; Go: `*_test.go`; Java: `*Test.java`, `*Tests.java`, `*IT.java`, `src/test/`; Python: `test_*.py`, `*_test.py`, `tests/`; Swift: `*Tests.swift`; ObjC: `*Test*.m`, `*Spec*.m`; TypeScript: `*.test.ts`, `*.spec.ts`, `__tests__/` |
| **Generated** | `*.generated.go`, `*.pb.go`, `*_pb2.py`, `*.generated.java` |
| **Config** | `*.config.rs`, `config/` directory |
| **Fixture** | `fixtures/`, `__fixtures__/`, `testdata/` |
| **Implementation** | Everything else |

Return `FileRole` from the discovery layer alongside the filepath and language.

#### Storage

Add `file_role` as a TAG field in the Redis FT.CREATE schema. Value is the lowercase enum name (e.g., `test`, `implementation`). Redis TAG fields support exact-match filtering via `@file_role:{implementation}`.

#### Embedding impact

Include `file_role` in the embedding text:

```
filepath: src/auth.rs
language: rust
node_type: function
file_role: implementation
name: verify_token
...
```

#### Search ranking

Default behavior: implementation results receive a 2x RRF score boost over test results. Specifically, when computing RRF scores in `fuse_search_lanes`, multiply each result's contribution by a factor based on its `file_role`:

- `Implementation`: factor 1.0
- `Config`: factor 1.0
- `Fixture`: factor 0.7
- `Generated`: factor 0.6
- `Test`: factor 0.5

Add a `file_role` parameter to `SearchParams` (default: no filter). When specified, results are filtered to only that role (enabling explicit `file_role=test` queries).

#### Schema migration

Bump `kt:schema_version` from 3 to 4. The existing migration path (`migrate_to_latest_schema`) drops the index and all data, then requires a full re-sync. Add `file_role` to `build_schema_args()` as a TAG field. Existing chunks re-indexed during sync will get the correct `file_role` value.

### 2. Boilerplate Reduction in Embeddings

**Goal**: Improve embedding quality by stripping boilerplate from the text sent to the embedding model. Full content remains in Redis storage for retrieval — no information loss for the user.

#### What gets stripped (embedding text only)

- Import/use/require statements (language-specific)
- Package/namespace declarations
- Blank/whitespace-only lines
- Multi-line doc comments: keep the first line, strip the rest (deferred — implementation strips imports/package declarations only)

#### What stays in both embedding and storage

- Function signatures, type definitions, struct fields
- Parent context headers
- All logic code, assertions, actual behavior

#### Implementation

Add `fn strip_boilerplate(content: &str, language: Language) -> String` in `src/embedding.rs`. Called inside `chunk_embedding_text()` to produce the embedding text. The `content` field stored in Redis remains unchanged.

The function uses line-based prefix matching (not a second Tree-sitter parse) to identify boilerplate per language:

| Language | Stripped patterns |
|----------|-------------------|
| Rust | Lines starting with `use ` or `use{` or `extern crate `; blank lines |
| Go | Lines starting with `import ` or `import(` or `package `; blank lines |
| Java | Lines starting with `import ` or `package `; blank lines |
| Python | Lines starting with `import ` or `from `; blank lines |
| Swift | Lines starting with `import `; blank lines |
| ObjC | Lines starting with `#import ` or `@import `; blank lines |
| TypeScript/TSX/JS | Lines starting with `import ` or `import{` or `} from`; blank lines |
| Markdown | (no stripping) |
| HTML | (no stripping) |

Stripping stops after the first non-blank, non-boilerplate line — content body is preserved unchanged.

### 3. Call-Graph Indexing

**Goal**: Help search return related steps together. A query like "host provisioning flow" should surface `create → boot → check` as connected results.

#### Extraction: call reference collection

During the existing `collect_chunks` traversal in `src/indexing.rs`, also identify call expressions in the AST:

| Language | Call node types |
|----------|----------------|
| Rust | `call_expression`, `method_call_expression` |
| Go | `call_expression`, `selector_expression` |
| Java | `method_invocation`, `object_method_invocation`, `class_instance_creation_expression` |
| Python | `call` |
| Swift | `call_expression`, `function_call_expression` |
| ObjC | `message_expression` |
| TypeScript/TSX/JS | `call_expression` |

For each call, extract:
- **callee name**: the function/method being called
- **receiver**: the type or object the method is called on (from `self.`, `Foo.`, etc.), if available

Store as a new `calls` field on `Chunk`:

```rust
struct CallRef {
    name: String,
    receiver: Option<String>,
}
```

#### Storage

Add `calls` as a **TEXT** field on the chunk document (not TAG — we need BM25 matching on call references, not exact-match filtering). The value is a space-separated string of `Receiver.Name` or `Name` patterns (e.g., `Host::create Host::boot assert_ready`). BM25 matching means a query for "host create" matches chunks that call `Host::create`, even if the chunk's own name is different.

Call refs are also included in embedding text (after boilerplate stripping):

```
filepath: src/provision.rs
language: rust
node_type: function
file_role: implementation
name: provision_host
calls: Host::create, Host::boot, Host::check
content:
...
```

#### Retrieval: call-context expansion

After primary search results are returned:

1. **Same-result boosting**: For called symbols that reference chunks found in the same search result set, boost those chunks' RRF scores (they're part of the same flow).
2. **Related-chunk expansion**: For called symbols not in the results, look up chunks by name using the existing `lookup_chunks_by_name_scoped()` function. Append up to N (default: 2) related chunks as `<related>` XML elements — similar to how `<parent_struct>` works today.

#### Scope for first iteration

- Extract **local call references only** — calls where the callee name can be resolved from the AST node (no cross-module resolution).
- Don't build a full call graph database — use the `calls` field as lightweight metadata.
- No new Redis structures beyond the `calls` TEXT field.

## Migration Path

1. Bump schema version to 4
2. `migrate_to_latest_schema` drops existing index and data (as already implemented for v2→v3)
3. `build_schema_args` updated with new `file_role` (TAG) and `calls` (TEXT) fields
4. Sync pipeline updated to populate `file_role` and `calls` on each chunk
5. Full re-sync required (natural consequence of schema migration)

## Success Criteria

- Search for "how does X work" returns implementation code before test code
- Searching for a step in a flow (e.g., "host boot") also surfaces related steps (create, check)
- Embedding quality improves: stripped embedding text produces better semantic matches for the same queries (measured by manual spot-checking on the reporter's codebase)