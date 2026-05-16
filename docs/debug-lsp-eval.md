# Debug LSP MCP Evaluation

Date: 2026-05-16

## Setup

Manual MCP client:

```bash
HOME=/private/tmp/kt-debug-home-seq \
KT_REDIS_URL=redis://localhost:6379 \
target/release/kt serve --debug
```

The client sent sequential `initialize`, `notifications/initialized`, and `tools/call`
requests against the debug server. Sequential calls matter because a batch of MCP
requests can race status reads and feedback reads against tool work.

## Findings

`_debug_lsp_definition` is useful when the cursor is on a call or type usage. In
the evaluation it resolved `mcp_error(...)` in `src/mcp.rs` to its local helper
definition, and resolved the `DebugLspManager` field type in `src/mcp.rs` to
`src/debug_lsp.rs`.

`_debug_lsp_references` is useful for local refactor planning. It returned seven
locations for `DebugLspManager`, spanning the MCP wrapper and the implementation
module.

`_debug_lsp_status` is useful as a cheap sanity check. Before navigation calls it
reported zero cached sessions; after definition/references it reported one running
`rust-analyzer` session for `/Users/michaelasper/source/kt`.

`_debug_lsp_document_symbols` is useful for file orientation, but it can be noisy.
`src/debug_lsp.rs` returned 76 symbol nodes. Large files need bounded output, so
the tool now defaults to returning at most 80 symbol nodes and supports
`max_symbols = 0` for unlimited output.

The LSP position tools use zero-based coordinates. Use `line = 0` for the first
line and `character = 0` for the first character in that line; one-based editor
line numbers need to be adjusted before calling `_debug_lsp_definition`,
`_debug_lsp_references`, or `_debug_chunk_at`.

`_debug_feedback` and `_debug_feedback_read` work for recording agent impressions.
The evaluation used a temporary `HOME` so feedback was written under
`/private/tmp` instead of the real user config directory.

`_debug_chunk_at` could not be evaluated in this sandbox because local Redis
access failed with `Operation not permitted (os error 1)`. That is an environment
limit, not enough evidence about the tool itself.

## Changes Driven By Evaluation

Rust-analyzer can return empty navigation results immediately after startup even
when the same request succeeds moments later. The LSP manager now waits briefly
for non-empty definition/reference locations before returning.

Document-symbol output is now bounded by default and omits raw JSON when
truncated, preventing a large file from flooding the agent context.
