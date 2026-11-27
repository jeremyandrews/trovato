#!/usr/bin/env bash
#
# Build the WASM test plugins and copy them into plugins/, exactly as the
# `test` job's "Build WASM plugins" step in .github/workflows/ci.yml does.
# Integration tests that load a plugin by name (`shared_app`, `TestApp::new`,
# and friends) read it from plugins/<name>/<name>.wasm, so this has to run
# once per checkout before any integration test target does.
#
#   ./scripts/build-test-plugins.sh
#
# The package list below is kept identical to CI's by
# scripts/ci-scripts-test.sh, which fails if the two drift apart. Bash 3.2
# safe (macOS), like scripts/ci-test-shard.sh.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

# Keep this list, in this order, identical to the `-p` flags of the `cargo
# build` in the "Build WASM plugins" step of the `test` job in
# .github/workflows/ci.yml. scripts/ci-scripts-test.sh checks that.
PACKAGES="trovato_blog trovato_search
test_e2e_caller test_e2e_callee test_e2e_bystander test_e2e_nocaps
test_ai_background
trovato_field_access_ref
trovato_recovery_ref test_recovery_bystander
test_queue_worker test_item_writer test_site_queries test_item_deny test_item_background test_search_enqueue trovato_scheduled_publishing test_members test_menu_entries test_page_builder_components
trovato_record_ref
trovato_spam
trovato_series test_plugin_api
test_assistant_scope
trovato_book trovato_contact
trovato_ai test_form_taps test_form_taps_b test_owned_vocabulary"

PKG_FLAGS=$(printf '%s\n' $PACKAGES | awk '{printf " -p %s", $1}')
# shellcheck disable=SC2086
cargo build $PKG_FLAGS --target wasm32-wasip1 --release

for pkg in $PACKAGES; do
  mkdir -p "plugins/$pkg"
  cp "target/wasm32-wasip1/release/$pkg.wasm" "plugins/$pkg/"
done

echo "Built and copied $(printf '%s\n' $PACKAGES | wc -w | tr -d ' ') test plugins into plugins/."
