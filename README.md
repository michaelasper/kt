# Knowledge Transfer (kt)

<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset=".github/logo-light.svg">
    <img alt="kt logo" src=".github/logo-light.svg" width="120">
  </picture>

  <h1>kt</h1>
  <p>Search and retrieve local Rust, Go, and Java code with semantic awareness for AI-assisted development.</p>
</div>

<div align="center">

[![Rust][rust-shield]][rust-url]

</div>

<div align="center">
  <a href="#quick-start">Quick Start</a> ·
  <a href="#install">Install</a> ·
  <a href="#usage">Usage</a> ·
  <a href="#mcp-setup">MCP Setup</a> ·
  <a href="#features">Features</a> ·
  <a href="#when-to-use">When to Use</a> ·
  <a href="#license">License</a>
</div>

<br>

---

## Why kt?

AI agents struggle with stale retrieval when repos are edited on branches because standard file-level indexing misses semantic context for uncommitted or draft changes.  
`kt` indexes local code into semantic chunks, so search can return function-level snippets with context that match how the code actually behaves in practice.

If you want a local, private, MCP-native knowledge layer for code reasoning, this is the tool.

## Features

- Indexes Rust, Go, and Java with Tree-sitter AST chunking and parent-context injection for stronger semantic recall.
- Runs hybrid retrieval in Redis with vector similarity plus BM25 keyword ranking.
- Supports branch-aware working sets through a temporary **shadow index** (`kt_git_status`, `kt_index_pr`) so changed files are searchable during PR or draft work.
- Exposes MCP tools (`kt_search`, `kt_read_file`, `kt_sync`) for agent workflows and scriptable code navigation.
- Embeds code locally with ONNX `all-MiniLM-L6-v2` to keep indexing and inference offline.
- Adds MCP-facing XML envelopes so results can be parsed safely by agent and automation tooling.

## When to Use

Use `kt` when you are doing AI-assisted development on local Rust/Go/Java code and want retrieval quality higher than plain grep or raw text search.

Avoid `kt` if your stack is not language-aware, or if you cannot run a local Redis Stack service.

## Quick Start

From a clean checkout:

1) Start Redis Stack:

```bash
docker compose up -d
```

2) Install `kt`:

```bash
KT_REPO_URL=$(git remote get-url origin) ./scripts/install.sh
```

3) Index and run the MCP server:

```bash
kt sync /path/to/repo
kt serve
```

## Install

Recommended install:

```bash
curl -fsSL https://raw.githubusercontent.com/michaelasper/kt/main/scripts/install.sh \
  | bash -s -- --repo https://github.com/michaelasper/kt.git
```

To install into a custom directory, set `KT_INSTALL_DIR`:

```bash
KT_INSTALL_DIR=/usr/local/bin \
  curl -fsSL https://raw.githubusercontent.com/michaelasper/kt/main/scripts/install.sh \
  | bash -s -- --repo https://github.com/michaelasper/kt.git
```

You can also pass options directly:

```bash
curl -fsSL https://raw.githubusercontent.com/michaelasper/kt/main/scripts/install.sh | bash -s -- \
  --repo https://github.com/michaelasper/kt.git \
  --prefix ~/.local/bin \
  --branch main
```

If you are already in a repository checkout:

```bash
KT_REPO_URL=$(git remote get-url origin) ./scripts/install.sh
```

For local development from source:

```bash
git clone https://github.com/michaelasper/kt.git kt
cd kt
cargo build
```

## Usage

### Start the server

```bash
kt serve
```

The server reads `KT_REDIS_URL` (default `redis://localhost:6379`) and exposes MCP tools for search and retrieval.

### Rebuild a repository index

```bash
kt sync /path/to/repo
```

### MCP Setup

Configure your MCP client to run `kt serve` and pass the Redis environment.

```json
{
  "mcpServers": {
    "kt": {
      "command": "kt",
      "args": ["serve"],
      "env": {
        "KT_REDIS_URL": "redis://localhost:6379"
      }
    }
  }
}
```

### Branch-safe indexing workflow

`kt` exposes two branch-aware tools for PR workflows:

#### `kt_git_status`

```bash
kt_git_status
```

Used as an MCP tool, this returns current branch, commit, working-tree dirty state, and changed files.

#### `kt_index_pr`

```bash
kt_index_pr
```

Used as an MCP tool, this indexes changed files into an ephemeral shadow index (TTL-based). Afterwards, call `kt_search` and `kt_read_file` normally; shadow chunks are merged and preferred automatically.

## Architecture Overview

`kt` uses a local flow:

1. Discover supported source files and parse them into typed semantic chunks.
2. Generate embeddings for chunks and persist metadata.
3. Query Redis hybrid indexes for ranked results and return scored contexts.

## Prerequisites

| Requirement | Details |
|-------------|---------|
| Redis Stack | Running locally for Redis Search/Hybrid vector indexing |
| Rust       | Stable toolchain for local builds |
| Docker     | Optional for local Redis startup |
| Env vars   | `KT_REDIS_URL` (default `redis://localhost:6379`) |

## Configuration

### Branch-aware workflow notes

- Run `kt_git_status` before and during editing to inspect branch/dirty state.
- Run `kt_index_pr` after staging or editing local files so the shadow index stays in sync with your current draft.
- Continue using `kt_search`/`kt_read_file` with no change to query semantics.

### Environment variables

- `KT_REDIS_URL` — Redis endpoint (default `redis://localhost:6379`)
- `KT_MODEL_CACHE_DIR` — Cache directory for local ONNX model files

## Contributing

Contributions are welcome. The project is organized around `src/` and keeps indexing, storage, and MCP tool boundaries explicit.

## License

No license file is included in this repository snapshot.

## Acknowledgments

- Built with Rust, Redis Stack, and MCP for local agent-native retrieval workflows.
- Inspired by practical developer workflows that need semantic context during branch-level editing.

Crafted with [Readme Craft](https://github.com/motiful/readme-craft)

[rust-shield]: https://img.shields.io/badge/rust-2021-orange?logo=rust
[rust-url]: https://www.rust-lang.org/
