# MCP Improvement Notes

Notes captured while using kt's MCP tools during language-support work.

## Parser Development

- Add an optional `kt_parse_preview` or `kt_debug_parse` tool that returns a compact Tree-sitter S-expression and extracted chunks for one file. This would make grammar integration faster without adding temporary debug tests.
- Include parser warnings in `kt_index_pr` output when a changed file parses to zero chunks. That would catch unsupported node names or malformed grammar config early.
- Expose language configuration metadata through a read-only MCP tool, including aliases, extensions, target nodes, and container nodes.

## Search Ergonomics

- Allow `kt_search` filters to accept language aliases such as `objc`, `md`, and `htm`, then echo the canonical language in results.
- Add a search result field for `signature` and `parent_context` match reasons, so users can tell whether a hit came from code content, surrounding type context, or metadata.
- Add a quick "changed files only" search mode after `kt_index_pr`, useful for validating branch-local parser changes.

## Sync Feedback

- Report per-language file counts at the end of `kt_sync` and `kt_index_pr`.
- Report per-language chunk counts and zero-chunk files so parser coverage gaps are visible.
- Consider a `--sample-chunks <N>` sync option for newly supported languages that prints representative extracted chunks.
