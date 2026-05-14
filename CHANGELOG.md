# Changelog

All notable changes to this project will be documented in this file.

## <small>0.8.2 (2026-05-14)</small>

* fix: remove unused &self from find_checksum_asset, move checksum fetch before progress bar ([4565531](https://github.com/michaelasper/kt/commit/4565531))
* fix: verify SHA256 of downloaded upgrade binary against release checksum asset ([12b6875](https://github.com/michaelasper/kt/commit/12b6875))

## <small>0.8.1 (2026-05-14)</small>

* fix: gate dead agent module behind feature flag and skip diagnostics overhead when disabled ([736bb05](https://github.com/michaelasper/kt/commit/736bb05))

## 0.8.0 (2026-05-13)

* style(sync): format sync UI changes ([0ccb0b4](https://github.com/michaelasper/kt/commit/0ccb0b4))
* feat(sync): redesign async sync progress UI ([4d4e1fb](https://github.com/michaelasper/kt/commit/4d4e1fb))

## <small>0.7.4 (2026-05-13)</small>

* fix: standardize MCP error signaling to use JSON-RPC errors ([45ff6cc](https://github.com/michaelasper/kt/commit/45ff6cc)), closes [#15](https://github.com/michaelasper/kt/issues/15)

## <small>0.7.3 (2026-05-13)</small>

* fix: propagate discovery errors and finalize async migration ([615ab92](https://github.com/michaelasper/kt/commit/615ab92)), closes [#10](https://github.com/michaelasper/kt/issues/10) [#31](https://github.com/michaelasper/kt/issues/31) [#10](https://github.com/michaelasper/kt/issues/10) [#31](https://github.com/michaelasper/kt/issues/31)

## <small>0.7.2 (2026-05-13)</small>

* perf: offload ONNX inference and tokenization to blocking tokio task ([7a128bd](https://github.com/michaelasper/kt/commit/7a128bd)), closes [#9](https://github.com/michaelasper/kt/issues/9)

## <small>0.7.1 (2026-05-13)</small>

* fix: standardize MCP error signaling ([3cb0091](https://github.com/michaelasper/kt/commit/3cb0091))

## 0.7.0 (2026-05-13)

* feat: add privacy-conscious diagnostics and metrics system ([8714745](https://github.com/michaelasper/kt/commit/8714745))
* feat: define public contract for agentic RAG queries and flatten sync logic ([cdbe3c7](https://github.com/michaelasper/kt/commit/cdbe3c7))
* perf: offload file discovery and git operations to blocking threads ([ff529dc](https://github.com/michaelasper/kt/commit/ff529dc))
* perf: offload parsing and embedding to blocking threads and parallelize sync ([546a5cf](https://github.com/michaelasper/kt/commit/546a5cf))

## <small>0.6.2 (2026-05-12)</small>

* fix(mcp): surface shadow index failures ([a09bded](https://github.com/michaelasper/kt/commit/a09bded)), closes [#12](https://github.com/michaelasper/kt/issues/12)

## <small>0.6.1 (2026-05-12)</small>

* fix(config): apply global settings at runtime ([f1d5f0f](https://github.com/michaelasper/kt/commit/f1d5f0f))

## 0.6.0 (2026-05-12)

* feat(search): improve abstract retrieval recall ([5f0697d](https://github.com/michaelasper/kt/commit/5f0697d))

## <small>0.5.6 (2026-05-12)</small>

* fix(storage): reject invalid language metadata ([dd6a0be](https://github.com/michaelasper/kt/commit/dd6a0be))

## <small>0.5.5 (2026-05-12)</small>

* fix: force a release release ([91eca03](https://github.com/michaelasper/kt/commit/91eca03))
* Add multi-codebase indexing support ([f14586c](https://github.com/michaelasper/kt/commit/f14586c))
* Add shadow read ordering regression ([2ced9be](https://github.com/michaelasper/kt/commit/2ced9be))
* Fix git base ref resolution ([995cf71](https://github.com/michaelasper/kt/commit/995cf71))
* Fix kt_read_file source ordering ([bf89611](https://github.com/michaelasper/kt/commit/bf89611))
* Merge pull request #38 from michaelasper/multi-codebase-indexing ([09d74c1](https://github.com/michaelasper/kt/commit/09d74c1)), closes [#38](https://github.com/michaelasper/kt/issues/38)
* security: add SHA256 verification for model downloads, size check for binary upgrades ([d964104](https://github.com/michaelasper/kt/commit/d964104)), closes [#30](https://github.com/michaelasper/kt/issues/30) [#4](https://github.com/michaelasper/kt/issues/4)
* style: apply cargo fmt formatting ([b26dcbc](https://github.com/michaelasper/kt/commit/b26dcbc))
* refactor: split storage.rs into focused modules, fix N+1 query pattern ([115c267](https://github.com/michaelasper/kt/commit/115c267)), closes [#6](https://github.com/michaelasper/kt/issues/6)

## <small>0.5.4 (2026-05-11)</small>

* perf(embedding): implement true batched ONNX inference in embed_batch ([a8a0d7e](https://github.com/michaelasper/kt/commit/a8a0d7e)), closes [#5](https://github.com/michaelasper/kt/issues/5)

## <small>0.5.3 (2026-05-11)</small>

* fix(sync): save last synced commit after full sync so next sync is incremental ([db7648f](https://github.com/michaelasper/kt/commit/db7648f))

## <small>0.5.2 (2026-05-11)</small>

* perf: batch lookup_chunks_by_name FT.SEARCH calls via Redis pipeline ([aad386d](https://github.com/michaelasper/kt/commit/aad386d)), closes [#24](https://github.com/michaelasper/kt/issues/24)

## <small>0.5.1 (2026-05-11)</small>

* fix: remove panic when tree-sitter language fails to load ([986b9bf](https://github.com/michaelasper/kt/commit/986b9bf)), closes [#33](https://github.com/michaelasper/kt/issues/33)

## 0.5.0 (2026-05-11)

* style: apply cargo fmt ([5a37e65](https://github.com/michaelasper/kt/commit/5a37e65))
* docs: rewrite spec.md to accurately reflect Rust implementation (#3) ([7243843](https://github.com/michaelasper/kt/commit/7243843)), closes [#3](https://github.com/michaelasper/kt/issues/3)
* docs: sync plan and design docs with actual implementation ([a23ce68](https://github.com/michaelasper/kt/commit/a23ce68))
* docs: update AGENTS.md with all 16 modules and 5 MCP tools (#3) ([43edf13](https://github.com/michaelasper/kt/commit/43edf13)), closes [#3](https://github.com/michaelasper/kt/issues/3)
* refactor: use shared sync pipeline in main.rs and mcp.rs ([4c2ba38](https://github.com/michaelasper/kt/commit/4c2ba38))
* refactor(sync): move deleted-file cleanup into execute(), take finish(self) by value ([5ca389d](https://github.com/michaelasper/kt/commit/5ca389d))
* feat(sync): add sync module with SyncStrategy, SyncPlan, execute, and finalize ([48b05a1](https://github.com/michaelasper/kt/commit/48b05a1))

## <small>0.4.1 (2026-05-11)</small>

* Merge remote-tracking branch 'origin/main' ([c050b4e](https://github.com/michaelasper/kt/commit/c050b4e))
* fix: apply cargo fmt formatting ([16ac625](https://github.com/michaelasper/kt/commit/16ac625))
* fix: replace unwrap with expect for static tracing strings (issue #36) ([c3b30c9](https://github.com/michaelasper/kt/commit/c3b30c9)), closes [#36](https://github.com/michaelasper/kt/issues/36)

## 0.4.0 (2026-05-11)

* feat: add cargo bin cache to test-macos job ([97aea99](https://github.com/michaelasper/kt/commit/97aea99))
* feat: add pre-push hook with all CI checks ([1de81ed](https://github.com/michaelasper/kt/commit/1de81ed))
* fix: add ndarray to cargo-machete ignored list ([83f78b8](https://github.com/michaelasper/kt/commit/83f78b8))
* fix: apply cargo fmt formatting ([e6dff20](https://github.com/michaelasper/kt/commit/e6dff20))
* Address code review feedback for issue #34 ([6e171e4](https://github.com/michaelasper/kt/commit/6e171e4)), closes [#34](https://github.com/michaelasper/kt/issues/34)
* Cache cargo bin and pin cargo-machete version in CI ([cddf84b](https://github.com/michaelasper/kt/commit/cddf84b))
* Fix silent failures in FT.SEARCH result count parsing (Issue #34) ([7b397ba](https://github.com/michaelasper/kt/commit/7b397ba)), closes [#34](https://github.com/michaelasper/kt/issues/34)
* Remove unused dependencies (issues #22, #35) and add cargo-machete to CI ([ece7a74](https://github.com/michaelasper/kt/commit/ece7a74)), closes [#22](https://github.com/michaelasper/kt/issues/22) [#35](https://github.com/michaelasper/kt/issues/35)

## <small>0.3.3 (2026-05-11)</small>

* fix: format test assertions per rustfmt ([03427f6](https://github.com/michaelasper/kt/commit/03427f6))
* fix: include start_line and separators in chunk ID generation (#2) ([324e39f](https://github.com/michaelasper/kt/commit/324e39f)), closes [#2](https://github.com/michaelasper/kt/issues/2)
* fix: remove needless borrow flagged by clippy ([7b9b596](https://github.com/michaelasper/kt/commit/7b9b596))
* refactor: zero-alloc start_line encoding and add boundary test (#2) ([3f83388](https://github.com/michaelasper/kt/commit/3f83388)), closes [#2](https://github.com/michaelasper/kt/issues/2)
* docs: add spec for chunk ID collision fix (#2) ([a61863d](https://github.com/michaelasper/kt/commit/a61863d)), closes [#2](https://github.com/michaelasper/kt/issues/2)

## <small>0.3.2 (2026-05-10)</small>

* fix(ci): defer release publication until build artifacts are ready ([d84522f](https://github.com/michaelasper/kt/commit/d84522f))

## <small>0.3.1 (2026-05-10)</small>

* fix(ui): ensure sync animation is rendered by ticking progress bars ([9aedea8](https://github.com/michaelasper/kt/commit/9aedea8))

## 0.3.0 (2026-05-10)

* feat(ui): overhaul sync UI with in-place updates and matrix rain ([48bfc9e](https://github.com/michaelasper/kt/commit/48bfc9e))

## <small>0.2.2 (2026-05-10)</small>

* fix: build all binaries before publishing GitHub release ([95bb5fe](https://github.com/michaelasper/kt/commit/95bb5fe))

## <small>0.2.1 (2026-05-10)</small>

* fix: eliminate release race condition and correct binary version ([3be481d](https://github.com/michaelasper/kt/commit/3be481d))

## 0.2.0 (2026-05-10)

* feat: cyberpunk data shredder sync UI with hex rain animation ([890ab56](https://github.com/michaelasper/kt/commit/890ab56))

## <small>0.1.9 (2026-05-10)</small>

* fix: extract binary from tar.gz before self-replace in upgrade ([ed6e6f1](https://github.com/michaelasper/kt/commit/ed6e6f1))

## <small>0.1.8 (2026-05-10)</small>

* fix: capture semantic-release output reliably for GITHUB_OUTPUT ([c1a9302](https://github.com/michaelasper/kt/commit/c1a9302))

## <small>0.1.7 (2026-05-10)</small>

* fix: set release_published output so build-release job runs ([310e17c](https://github.com/michaelasper/kt/commit/310e17c))

## <small>0.1.6 (2026-05-10)</small>

* fix: handle 'no such index' error from newer Redis/RediSearch ([cdd1841](https://github.com/michaelasper/kt/commit/cdd1841))
* style: run cargo fmt ([9693efa](https://github.com/michaelasper/kt/commit/9693efa))

## <small>0.1.5 (2026-05-10)</small>

* fix: handle RediSearch 'Index: already exists' error variant ([dab13fe](https://github.com/michaelasper/kt/commit/dab13fe))
* ci: add Redis service for integration tests ([0dc25ed](https://github.com/michaelasper/kt/commit/0dc25ed))
* ci: remove x86_64 macos build target ([1c9831d](https://github.com/michaelasper/kt/commit/1c9831d))
* ci: set OPENSSL_DIR for macOS builds, fix clippy warning ([9c9e9d4](https://github.com/michaelasper/kt/commit/9c9e9d4))
* ci: start redis-server directly instead of brew services ([49a1f9c](https://github.com/michaelasper/kt/commit/49a1f9c))
* ci: use redis-stack-server for RediSearch module support ([2a7f095](https://github.com/michaelasper/kt/commit/2a7f095))

## <small>0.1.4 (2026-05-10)</small>

* fix: graceful fallback to full sync when stored commit missing, fix upgrade asset pattern ([107b37a](https://github.com/michaelasper/kt/commit/107b37a))

## <small>0.1.3 (2026-05-10)</small>

* fix: strip v prefix from GitHub tag before semver parse ([58f41a4](https://github.com/michaelasper/kt/commit/58f41a4))

## <small>0.1.2 (2026-05-10)</small>

* fix: accept commit SHA in get_diff_files for partial sync ([913e452](https://github.com/michaelasper/kt/commit/913e452))
* ci: fix macOS build issues and add Node.js 24 support ([96bee0d](https://github.com/michaelasper/kt/commit/96bee0d))

## <small>0.1.1 (2026-05-10)</small>

* fix: add back conventional-changelog-conventionalcommits package ([f19169e](https://github.com/michaelasper/kt/commit/f19169e))
* ci: update GitHub workflows with modern actions and fix semantic-release ([ef14094](https://github.com/michaelasper/kt/commit/ef14094))
