#!/usr/bin/env bash
# Kernel line-count tracking script
# Counts code lines (excluding blanks and comments) in kernel and plugin SDK.
# Used to detect kernel bloat trends over time.
#
# Usage: ./scripts/kernel-loc.sh
#
# Baseline (2026-02-20): Kernel 34,785 | SDK 481 | Ratio 72.3:1

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL_DIR="$REPO_ROOT/crates/kernel/src"
SDK_DIR="$REPO_ROOT/crates/plugin-sdk/src"

count_code_lines() {
    local dir="$1"
    find "$dir" -name '*.rs' -exec grep -c -v -E '^\s*$|^\s*//' {} + \
        | awk -F: '{sum += $NF} END {print sum}'
}

count_total_lines() {
    local dir="$1"
    wc -l $(find "$dir" -name '*.rs') | tail -1 | awk '{print $1}'
}

count_files() {
    local dir="$1"
    find "$dir" -name '*.rs' | wc -l | tr -d ' '
}

KERNEL_CODE=$(count_code_lines "$KERNEL_DIR")
KERNEL_TOTAL=$(count_total_lines "$KERNEL_DIR")
KERNEL_FILES=$(count_files "$KERNEL_DIR")

SDK_CODE=$(count_code_lines "$SDK_DIR")
SDK_TOTAL=$(count_total_lines "$SDK_DIR")
SDK_FILES=$(count_files "$SDK_DIR")

RATIO=$(echo "scale=1; ${KERNEL_CODE} / ${SDK_CODE}" | bc)

echo "Trovato Kernel Line-Count Report"
echo "================================"
echo ""
echo "Kernel (crates/kernel/src/):"
echo "  Code lines:  ${KERNEL_CODE}"
echo "  Total lines: ${KERNEL_TOTAL}"
echo "  Files:       ${KERNEL_FILES}"
echo ""
echo "Plugin SDK (crates/plugin-sdk/src/):"
echo "  Code lines:  ${SDK_CODE}"
echo "  Total lines: ${SDK_TOTAL}"
echo "  Files:       ${SDK_FILES}"
echo ""
echo "Ratio: ${RATIO}:1 (kernel:SDK)"
echo ""
echo "Baseline (2026-02-20): Kernel 34,785 | SDK 481 | Ratio 72.3:1"
