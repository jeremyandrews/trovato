#!/usr/bin/env bash
#
# Builds the WASM plugins the tests load and copies each one into
# plugins/<name>/<name>.wasm, where the integration tests look for it
# (`common::project_root().join("plugins")`). This list is the only place they
# are named: CI's Test, Coverage and Build jobs and scripts/pre-commit-check.sh
# all call this script. Before B235 each job kept its own copy of the list, a
# change that added two plugins to the Test job's copy left Coverage's behind,
# and Coverage failed on every push to main without a pull request ever showing
# it. scripts/ci-check-plugin-build-parity.sh fails a pull request whose
# workflow builds plugins any other way.
#
# A test that needs a new plugin: add it to PLUGINS below, and nowhere else.
#
#   ./scripts/ci-build-plugins.sh          build and copy into plugins/
#   ./scripts/ci-build-plugins.sh --list   print the list, one per line
#
# The build is a plain `cargo build`, never under cargo-llvm-cov, so the
# artifacts land in the default target/wasm32-wasip1/release whatever target
# directory the coverage tool uses for the kernel. CARGO_TARGET_DIR is honoured
# if it is set.
set -euo pipefail

PLUGINS=(
  trovato_blog
  trovato_search
  test_e2e_caller
  test_e2e_callee
  test_e2e_bystander
  test_e2e_nocaps
  test_ai_background
  trovato_field_access_ref
  trovato_recovery_ref
  test_recovery_bystander
  test_queue_worker
  test_item_writer
  test_site_queries
  test_item_deny
  test_item_background
  test_search_enqueue
  trovato_scheduled_publishing
  test_members
  test_menu_entries
  test_page_builder_components
  trovato_record_ref
  trovato_spam
  trovato_series
  test_plugin_api
  test_assistant_scope
  trovato_book
  trovato_contact
  trovato_ai
  test_form_taps
  test_form_taps_b
  test_owned_vocabulary
  test_timer
)

if [ "${1:-}" = "--list" ]; then
  printf '%s\n' "${PLUGINS[@]}"
  exit 0
fi
if [ $# -ne 0 ]; then
  echo "usage: $0 [--list]" >&2
  exit 2
fi

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

args=()
for name in "${PLUGINS[@]}"; do
  args+=(-p "$name")
done
cargo build "${args[@]}" --target wasm32-wasip1 --release

out="${CARGO_TARGET_DIR:-target}/wasm32-wasip1/release"
for name in "${PLUGINS[@]}"; do
  mkdir -p "plugins/$name"
  cp "$out/$name.wasm" "plugins/$name/"
done
echo "built and copied ${#PLUGINS[@]} plugins into plugins/"
