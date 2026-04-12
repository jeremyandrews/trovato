#!/bin/bash
# Render all fixtures through Tera and verify output.
set -euo pipefail

FIXTURE_DIR="fixtures"
RUST_OUTPUT="output/tera"
mkdir -p "$RUST_OUTPUT"

echo "=== Building Rust renderer ==="
cd renderer && cargo build --release && cd ..

echo ""
echo "=== Rendering fixtures through Tera ==="
for f in "$FIXTURE_DIR"/*.json; do
    name=$(basename "$f" .json)
    ./renderer/target/release/pb-renderer "$f" > "$RUST_OUTPUT/$name.html"
    echo "  Rendered: $name ($(wc -c < "$RUST_OUTPUT/$name.html" | tr -d ' ') bytes)"
done

echo ""
echo "=== XSS test (edge-cases.json) ==="
if grep -q '<script>' "$RUST_OUTPUT/edge-cases.html"; then
    echo "  FAIL: <script> tag found in sanitized output!"
    exit 1
else
    echo "  PASS: No <script> tags in sanitized output"
fi

echo ""
echo "=== Zone recursion test (with-columns.json) ==="
if grep -q 'pb-columns__zone' "$RUST_OUTPUT/with-columns.html"; then
    echo "  PASS: Column zones rendered"
else
    echo "  FAIL: No column zones found in output"
    exit 1
fi

if grep -q 'pb-text-block' "$RUST_OUTPUT/with-columns.html"; then
    echo "  PASS: Child components rendered inside zones"
else
    echo "  FAIL: No child components in column zones"
    exit 1
fi

echo ""
echo "=== Markdown rendering test (simple-page.json) ==="
if grep -q '<h2>' "$RUST_OUTPUT/simple-page.html"; then
    echo "  PASS: Markdown headings rendered"
else
    echo "  FAIL: No <h2> found (Markdown not rendered)"
    exit 1
fi

if grep -q '<strong>' "$RUST_OUTPUT/simple-page.html"; then
    echo "  PASS: Markdown bold rendered"
else
    echo "  FAIL: No <strong> found (Markdown not rendered)"
    exit 1
fi

echo ""
echo "=== All tests passed ==="
echo ""
echo "Tera output files:"
for f in "$RUST_OUTPUT"/*.html; do
    echo "  $f"
done
echo ""
echo "Open in a browser to visually compare with the Puck editor canvas."
echo "Both use the same CSS classes (pb-hero, pb-text-block, pb-columns, etc.)"
