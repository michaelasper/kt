# kt (Knowledge Transfer)

A local polyglot codebase RAG via MCP.

## Architecture

- `src/lib.rs` — Core types: `Language`, `Chunk`, `SearchResult`
- `src/config.rs` — Config from env vars
- `src/discovery.rs` — File walker
- `src/indexing.rs` — Tree-sitter AST chunker with parent context injection
- `src/indexing/languages.rs` — Per-language Tree-sitter configs (Rust, Go, Java)
- `src/embedding.rs` — ONNX Runtime embedding engine (all-MiniLM-L6-v2, 384-dim)
- `src/storage.rs` — Redis CRUD, FT.CREATE index, hybrid vector+BM25 search
- `src/mcp.rs` — MCP server with 3 tools (kt_search, kt_read_file, kt_sync)
- `src/main.rs` — clap CLI: `kt sync <dir>` or `kt serve`

## Testing

```bash
cargo test              # unit tests
cargo test --test adversarial  # integration tests (requires Redis)
cargo clippy --all-targets --all-features
```

## Using kt

The `kt` MCP server is configured in OpenCode. Use `kt_search` to search the codebase semantically, `kt_read_file` to read file chunks, and `kt_sync` to index a directory.

When exploring this codebase, prefer using `kt_search` to find relevant code before reading files directly.
