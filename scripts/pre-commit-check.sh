#!/usr/bin/env bash
# Pre-commit quality checks — mirrors CI jobs that don't need infrastructure.
#
# Runs: format check, clippy, unit tests (no DB/Redis required).
# Skips: integration tests (need Postgres+Redis), coverage, security audit.
#
# Usage:
#   ./scripts/pre-commit-check.sh          # run all checks
#   ./scripts/pre-commit-check.sh --quick  # fmt + clippy only (no tests)

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

QUICK=0
if [[ "${1:-}" == "--quick" ]]; then
    QUICK=1
fi

FAILED=0

echo "=== Format check ==="
if cargo fmt --all -- --check 2>&1; then
    echo -e "${GREEN}OK${NC}"
else
    echo -e "${RED}FAIL: run 'cargo fmt --all' to fix${NC}"
    FAILED=1
fi

echo ""
echo "=== Clippy ==="
if cargo clippy --all-targets -- -D warnings 2>&1; then
    echo -e "${GREEN}OK${NC}"
else
    echo -e "${RED}FAIL: fix clippy warnings above${NC}"
    FAILED=1
fi

if [[ "$QUICK" -eq 0 ]]; then
    echo ""
    echo "=== Unit tests (no infrastructure required) ==="
    if cargo test --all --lib 2>&1; then
        echo -e "${GREEN}OK${NC}"
    else
        echo -e "${RED}FAIL: unit tests failed${NC}"
        FAILED=1
    fi

    echo ""
    echo "=== Doc tests ==="
    if cargo test --all --doc 2>&1; then
        echo -e "${GREEN}OK${NC}"
    else
        echo -e "${RED}FAIL: doc tests failed${NC}"
        FAILED=1
    fi
fi

echo ""
if [[ "$FAILED" -eq 0 ]]; then
    echo -e "${GREEN}All checks passed!${NC}"
else
    echo -e "${RED}Some checks failed — fix before committing.${NC}"
    exit 1
fi
