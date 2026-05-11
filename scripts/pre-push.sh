#!/bin/bash
# Pre-push hook: runs all CI checks before allowing push
# To install: cp scripts/pre-push.sh .git/hooks/pre-push && chmod +x .git/hooks/pre-push

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}Running pre-push checks (same as CI)...${NC}"

# Read stdin for push info (standard pre-push hook input)
if ! [ -t 0 ]; then
    read -r local_ref local_sha remote_ref remote_sha
    echo "Pushing $local_ref ($local_sha) to $remote_ref ($remote_sha)"
fi

# Track failures
FAILURES=0

# Function to run a check
run_check() {
    local name="$1"
    shift
    echo -e "\n${YELLOW}Running: $name${NC}"
    if "$@"; then
        echo -e "${GREEN}✓ $name passed${NC}"
    else
        echo -e "${RED}✗ $name failed${NC}"
        FAILURES=$((FAILURES + 1))
    fi
}

# 1. cargo fmt check
run_check "cargo fmt" cargo fmt --all -- --check

# 2. cargo check
run_check "cargo check" cargo check --all-targets --all-features

# 3. cargo clippy
run_check "cargo clippy" cargo clippy --all-targets --all-features -- -D warnings

# 4. cargo test
run_check "cargo test" cargo test --all-features --verbose

# 5. cargo machete (optional - only if installed)
if command -v cargo-machete &> /dev/null; then
    run_check "cargo machete" cargo machete --with-metadata
else
    echo -e "\n${YELLOW}⚠ cargo-machete not installed, skipping unused deps check${NC}"
    echo "To install: cargo install cargo-machete@0.7.0"
fi

# Summary
echo -e "\n----------------------------------------"
if [ $FAILURES -eq 0 ]; then
    echo -e "${GREEN}All checks passed!${NC}"
    exit 0
else
    echo -e "${RED}$FAILURES check(s) failed!${NC}"
    echo -e "${RED}Push aborted. Fix the issues and try again.${NC}"
    exit 1
fi
