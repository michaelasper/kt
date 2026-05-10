#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: install.sh [options]

Install kt from a git repository by compiling from source.

Options:
  --repo <url>     Git repository URL (or KT_REPO_URL env var)
  --branch <name>  Git branch, tag, or commit (default: main)
  --prefix <path>  Install directory (default: $HOME/.local/bin)
  --help           Show this help message

Environment:
  KT_REPO_URL   Default repository when --repo is not provided
  KT_INSTALL_DIR  Default install directory
  KT_GIT_REF      Default branch/reference

Examples:
  KT_REPO_URL=https://github.com/example/kt.git ./scripts/install.sh
  curl -fsSL https://raw.githubusercontent.com/example/kt/main/scripts/install.sh | bash -s -- --repo https://github.com/example/kt.git
EOF
}

REPO_URL="${KT_REPO_URL:-}"
INSTALL_DIR="${KT_INSTALL_DIR:-$HOME/.local/bin}"
GIT_REF="${KT_GIT_REF:-main}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      shift
      if [[ $# -lt 1 ]]; then
        echo "error: --repo requires a URL" >&2
        exit 64
      fi
      REPO_URL="$1"
      ;;
    --branch)
      shift
      if [[ $# -lt 1 ]]; then
        echo "error: --branch requires a branch, tag, or commit" >&2
        exit 64
      fi
      GIT_REF="$1"
      ;;
    --prefix)
      shift
      if [[ $# -lt 1 ]]; then
        echo "error: --prefix requires a path" >&2
        exit 64
      fi
      INSTALL_DIR="$1"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 64
      ;;
  esac
  shift
done

if [[ -z "${REPO_URL}" ]]; then
  echo "error: repository URL is required. Pass --repo or set KT_REPO_URL." >&2
  usage
  exit 64
fi

for binary in git cargo curl; do
  if ! command -v "$binary" >/dev/null 2>&1; then
    echo "error: required command not found: $binary" >&2
    exit 1
  fi
done

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT
repo_dir="${tmp_dir}/kt"

echo "Cloning ${REPO_URL} (${GIT_REF})..."
git clone --depth 1 --branch "${GIT_REF}" "${REPO_URL}" "${repo_dir}"

if [[ ! -f "${repo_dir}/Cargo.toml" ]]; then
  echo "error: ${REPO_URL} does not contain a Rust project" >&2
  exit 1
fi

echo "Building kt (release)..."
(cd "${repo_dir}" && cargo build --release --locked)

mkdir -p "${INSTALL_DIR}"
cp "${repo_dir}/target/release/kt" "${INSTALL_DIR}/kt"
chmod +x "${INSTALL_DIR}/kt"

if command -v kt >/dev/null 2>&1; then
  installed_via_path="$(command -v kt)"
else
  installed_via_path="${INSTALL_DIR}/kt"
fi

echo "Installed kt -> ${INSTALL_DIR}/kt"
echo

# Colorful welcome message
if command -v tput >/dev/null 2>&1; then
    BOLD=$(tput bold)
    GREEN=$(tput setaf 2)
    YELLOW=$(tput setaf 3)
    BLUE=$(tput setaf 4)
    CYAN=$(tput setaf 6)
    RESET=$(tput sgr0)
else
    BOLD=""
    GREEN=""
    YELLOW=""
    BLUE=""
    CYAN=""
    RESET=""
fi

echo "${BOLD}${CYAN}╔════════════════════════════════════════════════════════════╗${RESET}"
echo "${BOLD}${CYAN}║                                                          ║${RESET}"
echo "${CYAN}║${RESET} ${BOLD}${GREEN}✨  kt Installed Successfully!  ✨${RESET} ${CYAN}║${RESET}"
echo "${CYAN}║                                                          ║${RESET}"
echo "${BOLD}${CYAN}╚════════════════════════════════════════════════════════════╝${RESET}"
echo

echo "${BOLD}${YELLOW}🚀 Quick Start:${RESET}"
echo
echo "  1. ${CYAN}Start Redis:${RESET}"
echo "     ${BLUE}docker compose up -d${RESET}"
echo
echo "  2. ${CYAN}Index your codebase:${RESET}"
echo "     ${BLUE}kt sync .${RESET}"
echo
echo "  3. ${CYAN}Configure MCP (optional but recommended):${RESET}"
echo "     ${BLUE}kt mcp setup${RESET}"
echo

echo "${BOLD}${YELLOW}💡 Pro Tips:${RESET}"
echo
echo "  • ${GREEN}Use global config for consistent settings across repos${RESET}"
echo "    ${BLUE}kt mcp setup --global${RESET}"
echo
echo "  • ${GREEN}Auto-detect Redis and harnesses for quick setup${RESET}"
echo
echo "  • ${GREEN}Create AGENTS.md in your repo for AI assistant context${RESET}"
echo "    ${BLUE}kt mcp setup --create-agents${RESET}"
echo

echo "${BOLD}${YELLOW}📚 Learn More:${RESET}"
echo "  • Documentation: ${BLUE}https://github.com/michaelasper/kt${RESET}"
echo "  • Run: ${BLUE}kt --help${RESET}"
echo

echo "${GREEN}✓${RESET} Installation complete! Note: ensure ${YELLOW}${INSTALL_DIR}${RESET} is on your PATH"
