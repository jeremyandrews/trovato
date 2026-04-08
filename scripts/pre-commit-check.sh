#!/usr/bin/env bash
# Pre-commit quality checks — mirrors all CI jobs that don't need infrastructure.
#
# Runs: format, clippy, doc check, unit tests, doc tests
# Skips: integration tests (need Postgres+Redis), coverage, security audit
#
# Usage:
#   ./scripts/pre-commit-check.sh          # all checks
#   ./scripts/pre-commit-check.sh --quick  # fmt + clippy only (fastest, ~30s)
#   ./scripts/pre-commit-check.sh --full   # all checks + release build + WASM plugins

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

MODE="standard"
if [[ "${1:-}" == "--quick" ]]; then
    MODE="quick"
elif [[ "${1:-}" == "--full" ]]; then
    MODE="full"
fi

FAILED=0

run_check() {
    local label="$1"
    shift
    echo "=== $label ==="
    if "$@" 2>&1; then
        echo -e "${GREEN}OK${NC}"
    else
        echo -e "${RED}FAIL${NC}"
        FAILED=1
    fi
    echo ""
}

# --- Always run: format + clippy (matches CI: Format + Clippy jobs) ---
run_check "Format check" cargo fmt --all -- --check
run_check "Clippy" cargo clippy --all-features --all-targets -- -D warnings

if [[ "$MODE" == "quick" ]]; then
    if [[ "$FAILED" -eq 0 ]]; then
        echo -e "${GREEN}All checks passed!${NC}"
    else
        echo -e "${RED}Some checks failed — fix before committing.${NC}"
        exit 1
    fi
    exit 0
fi

# --- Standard: add doc check + unit tests + doc tests ---

# Doc check (matches CI: Doc Check job with RUSTDOCFLAGS="-D warnings")
echo "=== Doc check ==="
if RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items 2>&1; then
    echo -e "${GREEN}OK${NC}"
else
    echo -e "${RED}FAIL${NC}"
    FAILED=1
fi
echo ""

run_check "Unit tests" cargo test --all --lib
run_check "Doc tests" cargo test --all --doc

# --- Full: add release build + WASM plugin build ---

if [[ "$MODE" == "full" ]]; then
    run_check "Release build" cargo build -p trovato-kernel --release
    echo "=== WASM plugin build ==="
    if cargo build -p trovato_blog -p trovato_search --target wasm32-wasip1 --release 2>&1; then
        echo -e "${GREEN}OK${NC}"
    else
        echo -e "${RED}FAIL${NC}"
        FAILED=1
    fi
    echo ""
fi

if [[ "$FAILED" -eq 0 ]]; then
    echo -e "${GREEN}All checks passed!${NC}"
else
    echo -e "${RED}Some checks failed — fix before committing.${NC}"
    exit 1
fi
