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
- Stores a codebase registry so multiple repositories can share one Redis corpus while still supporting scoped search by alias or path.
- Supports branch-aware working sets through a temporary **shadow index** (`kt_git_status`, `kt_index_pr`) so changed files are searchable during PR or draft work.
- Exposes MCP tools (`kt_search`, `kt_read_file`, `kt_sync`, `kt_list_codebases`) for agent workflows and scriptable code navigation.
- Embeds code locally with ONNX `all-MiniLM-L6-v2` to keep indexing and inference offline.
- Adds MCP-facing XML envelopes so results can be parsed safely by agent and automation tooling.

## When to Use

Use `kt` when you are doing AI-assisted development on local Rust/Go/Java code and want retrieval quality higher than plain grep or raw text search.

Avoid `kt` if your stack is not language-aware, or if you cannot run a local Redis Stack service.

## Security & Stability

`kt` uses the `ort` (ONNX Runtime) crate for local embeddings. As of version 0.1.0, this dependency is pinned to a **release candidate** (`2.0.0-rc.12`) to support the latest ONNX Runtime features and performance improvements. 

While this version is recommended by the `ort` maintainers for new projects, it carries a risk of regressions or unpatched security vulnerabilities. 

- This risk is acknowledged via the `unstable-ort-rc` Cargo feature (enabled by default).
- For more details, see [Issue #47](https://github.com/michaelasper/kt/issues/47).

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
kt sync --name my-repo /path/to/repo
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

### Index a repository

```bash
kt sync --name my-repo /path/to/repo
```

`--name` is optional, but aliases make scoped MCP calls easier. A repository's `codebase_id` is derived from the canonical absolute sync root, so the same alias cannot be assigned to a different path.

Upgrading to the multi-codebase schema migrates Redis to `kt:schema_version = 2`. The migration intentionally drops old `kt:doc:*`, `kt:shadow:*`, and legacy sync-state keys, so repositories must be re-synced after upgrading.

### Upgrade kt

```bash
kt upgrade
```

Upgrade kt to the latest version from GitHub releases. Use `--force` to re-install even if already up-to-date, or `--version <version>` to install a specific version.

### Configure MCP harnesses

```bash
kt mcp setup
```

Interactively configure kt for detected MCP harnesses (OpenCode, Claude Desktop, Cline, Continue, Oh My Pi).

```bash
kt mcp setup --global
```

Configure kt with global settings that apply across all repositories.

```bash
kt mcp setup --create-agents
```

Create an `AGENTS.md` file in the current directory with kt usage instructions for AI assistants.

```bash
kt mcp list
```

List detected MCP harnesses and their configuration status.

```bash
kt mcp show
```

Display the current global kt configuration.

## CLI Commands

| Command | Description |
|---------|-------------|
| `kt serve` | Start the MCP server (stdio transport) |
| `kt sync [--name <alias>] <dir>` | Index a directory into the knowledge base |
| `kt upgrade` | Upgrade kt to the latest version |
| `kt mcp setup` | Configure kt for MCP harnesses (interactive) |
| `kt mcp setup --global` | Configure with global settings |
| `kt mcp setup --create-agents` | Create AGENTS.md in current directory |
| `kt mcp list` | List detected MCP harnesses |
| `kt mcp show` | Show global configuration |
| `kt --help` | Show help information |

### MCP Setup

**Recommended:** Use the built-in setup command to automatically configure kt for your AI harness:

```bash
kt mcp setup
```

This will:
- Detect installed MCP harnesses (OpenCode, Claude Desktop, Cline, Continue, Oh My Pi)
- Auto-detect Redis instance (with 5-second timeout)
- Configure each harness with the appropriate settings
- Create global configuration for consistent settings across repos

**Manual Configuration:** If you prefer manual configuration, add this to your MCP client config:

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

**Supported Harnesses:**
- **OpenCode**: `~/.config/opencode/mcp.json`
- **Claude Desktop**: `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS)
- **Cline**: `~/.vscode/settings.json`
- **Continue**: `~/.vscode/settings.json`
- **Oh My Pi**: `~/.omp/agent/mcp.json`

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

### Multi-codebase search

By default, `kt_search` searches across every indexed codebase:

```xml
<kt_search query="redis schema migration" />
```

Scope searches or file reads with either `codebase_alias` or `directory_path`:

```xml
<kt_search query="hybrid_search" codebase_alias="kt" />
<kt_read_file filepath="src/lib.rs" directory_path="/Users/me/source/kt" />
```

Use `kt_list_codebases` to discover registered codebases, aliases, root paths, last synced commits, and indexed status. Unscoped `kt_read_file` returns grouped matches for the requested repo-relative filepath across all codebases.

### Retrieval behavior

`kt_search` keeps the same MCP parameters and XML output, but internally retrieves from two lanes: a vector-first semantic lane over only hard filters such as codebase and language, and a BM25 lexical lane for the query text. This improves recall for abstract questions because natural-language query words no longer pre-filter vector candidates.

Existing indexes benefit from vector-first retrieval immediately after upgrading. Newly synced chunks also embed filepath, language, symbol metadata, parent context, and code content; run `kt sync --full <repo>` to re-embed an existing repository with that enriched input.

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

### Upgrading

Keep kt up-to-date with the latest features and bug fixes:

```bash
kt upgrade
```

The upgrade command:
- Checks GitHub releases for newer versions
- Downloads the appropriate binary for your platform
- Safely replaces the current binary
- Supports force re-install with `--force`
- Supports installing specific versions with `--version <version>`

### Nightly Builds

For early access to new features, try the nightly builds:

```bash
curl -L https://github.com/michaelasper/kt/releases/download/nightly/kt-darwin-amd64-nightly-<date>.tar.gz | tar xz
sudo mv kt /usr/local/bin/
```

⚠️ **Warning:** Nightly builds may contain unstable features and bugs. Use at your own risk!

## Development

### Building from Source

```bash
git clone https://github.com/michaelasper/kt.git
cd kt
cargo build --release
```

### Testing

```bash
cargo test              # unit tests
cargo test --test adversarial  # integration tests (requires Redis)
cargo clippy --all-targets --all-features
cargo fmt --all -- --check
```

### CI/CD

This project uses GitHub Actions for continuous integration and automated releases:

- **CI**: Runs tests, linting, and builds on every push and pull request
- **Semantic Releases**: Automatic versioning and release creation based on conventional commits
- **Nightly Builds**: Daily automated builds from the main branch

**Conventional Commits:**

Use conventional commit messages to trigger automatic releases:

- `feat:` - New features (triggers minor version bump)
- `fix:` - Bug fixes (triggers patch version bump)
- `chore:`, `docs:`, `style:`, `refactor:`, `perf:`, `test:`, `build:`, `ci:` - Other changes (no version bump)

Example:
```bash
git commit -m "feat: add automatic Redis detection"
git commit -m "fix: resolve issue with chunk deduplication"
```

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
