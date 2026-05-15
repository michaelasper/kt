# kt Backlog

This file tracks technical debt, performance optimizations, and future enhancements identified during development and code reviews.

## Performance
- [ ] **Search Stage Pipelining**: In `execute_hybrid_lane`, semantic and lexical searches are pipelined. However, Stage 3 (Pure Semantic) fallback is currently a standalone call. We could potentially reuse semantic results from the initial pipeline if cached.
- [ ] **Batch Deletion Optimization**: `remove_file_chunks_impl` deletes keys sequentially in a single pipe. For files with 10,000+ chunks, this is still one large round-trip. Monitor performance for high-latency remote Redis.
- [ ] **Embedding Concurrency**: Revisit removing `Mutex` around ONNX `Session` if future `ort` versions implement `Clone` or if we move to a session pool. Current version (rc.12) requires `&mut self` for `run`.

## Configuration
- [ ] **Externalize Search Thresholds**: The search score threshold (`0.6`) is currently a magic number in `src/storage/search.rs`. Move this to `Config` for user-tunable precision/recall.

## Roadmap
- [ ] **Extensible Language Support**: Refactor language configs to be loaded from TOML/JSON instead of being hardcoded in `src/lib.rs` and `indexing/languages.rs`.
