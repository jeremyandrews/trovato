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
# --workspace so the plugin crates are linted too; they are workspace members
# that were not in default-members, so a plain `cargo clippy` skipped them.
run_check "Clippy" cargo clippy --workspace --all-features --all-targets -- -D warnings

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
# The expand-then-contract rule for migrations (docs/design/upgrades-and-probes.md
# §3), named on its own so a failure says what it is. Mirrors CI.
run_check "Migration compatibility rule" cargo test --all --lib migration_rule
# The CI timing scripts and the test budget (mirrors CI: the Test job's
# "Test the timing scripts" step). No database, no build.
run_check "CI timing scripts" ./scripts/ci-scripts-test.sh
# The version and changelog gates (mirrors CI: the Version Step job).
run_check "Release gate scripts" ./scripts/release-scripts-test.sh

# SQL custom-expr guard (PF-5 durable convention, mirrors CI: SQL Custom-Expr
# Guard job). Ratchets bare `Expr::cust(` sites; a new one must be confirmed
# value-free (else use Expr::cust_with_values) and bump the baseline. Keep
# CUST_BASELINE identical to .github/workflows/ci.yml.
echo "=== SQL custom-expr guard ==="
CUST_BASELINE=21
CUST_COUNT=$(grep -rn --include='*.rs' 'Expr::cust(' crates/ | wc -l | tr -d ' ')
if [[ "$CUST_COUNT" -gt "$CUST_BASELINE" ]]; then
    echo -e "${RED}FAIL${NC}: $CUST_COUNT bare Expr::cust( sites > baseline $CUST_BASELINE."
    echo "Value-bearing custom SQL MUST use Expr::cust_with_values (bound params)."
    echo "If the new site is provably value-free, bump CUST_BASELINE here and in CI."
    FAILED=1
else
    echo -e "${GREEN}OK${NC} ($CUST_COUNT bare Expr::cust( sites, baseline $CUST_BASELINE)"
fi
echo ""

# --- Full: add release build + WASM plugin build ---

if [[ "$MODE" == "full" ]]; then
    run_check "Release build" cargo build -p trovato-kernel --release
    echo "=== WASM plugin build ==="
    # The same list CI builds, from the one script that holds it (B235).
    if ./scripts/ci-build-plugins.sh 2>&1; then
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
