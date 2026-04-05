#!/usr/bin/env bash
# Compares recipe sync hashes against current tutorial file hashes.
# Run before starting any tutorial part to catch drift.

set -euo pipefail

TUTORIAL_DIR="docs/tutorial"
RECIPE_DIR="docs/tutorial/recipes"

check_sync() {
    local part="$1"
    local tutorial_file="$2"
    local recipe_file="$3"

    if [[ ! -f "$tutorial_file" ]]; then
        echo "ERROR: Tutorial file missing: $tutorial_file"
        return 1
    fi
    if [[ ! -f "$recipe_file" ]]; then
        echo "ERROR: Recipe file missing: $recipe_file"
        return 1
    fi

    local current_hash
    current_hash=$(shasum -a 256 "$tutorial_file" | cut -c1-8)
    local recorded_hash
    recorded_hash=$(grep -o 'Sync hash:\*\* [a-f0-9]\{8\}' "$recipe_file" | grep -o '[a-f0-9]\{8\}$' || echo "MISSING")

    if [[ "$current_hash" == "$recorded_hash" ]]; then
        echo "OK: $part — recipe matches tutorial ($current_hash)"
    else
        echo "DRIFT: $part — tutorial is $current_hash, recipe says $recorded_hash"
        echo "  -> Diff the tutorial against the recipe and update the recipe before proceeding."
        return 1
    fi
}

exit_code=0
check_sync "Part 1" "$TUTORIAL_DIR/part-01-hello-trovato.md" "$RECIPE_DIR/recipe-part-01.md" || exit_code=1
check_sync "Part 2" "$TUTORIAL_DIR/part-02-ritrovo-importer.md" "$RECIPE_DIR/recipe-part-02.md" || exit_code=1
check_sync "Part 3" "$TUTORIAL_DIR/part-03-look-and-feel.md" "$RECIPE_DIR/recipe-part-03.md" || exit_code=1
check_sync "Part 4" "$TUTORIAL_DIR/part-04-editorial-engine.md" "$RECIPE_DIR/recipe-part-04.md" || exit_code=1
check_sync "Part 5" "$TUTORIAL_DIR/part-05-forms-and-input.md" "$RECIPE_DIR/recipe-part-05.md" || exit_code=1
check_sync "Part 6" "$TUTORIAL_DIR/part-06-community.md" "$RECIPE_DIR/recipe-part-06.md" || exit_code=1
check_sync "Part 7" "$TUTORIAL_DIR/part-07-going-global.md" "$RECIPE_DIR/recipe-part-07.md" || exit_code=1
check_sync "Part 8" "$TUTORIAL_DIR/part-08-production-ready.md" "$RECIPE_DIR/recipe-part-08.md" || exit_code=1
check_sync "Part 9" "$TUTORIAL_DIR/part-09-ai-and-search.md" "$RECIPE_DIR/recipe-part-09.md" || exit_code=1

if [[ $exit_code -eq 0 ]]; then
    echo ""
    echo "All recipes in sync."
else
    echo ""
    echo "Some recipes are out of sync. Fix before proceeding."
fi
exit $exit_code
