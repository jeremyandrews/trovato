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
# CI's own step is `./scripts/ci-build-plugins.sh`, which also holds the one
# list of plugins (see its header: B235 moved the list there after Test and
# Coverage each kept a hand-written copy and drifted apart). This delegates to
# it instead of keeping a second list that could drift the same way;
# scripts/ci-scripts-test.sh checks that it still delegates.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
exec "$ROOT/scripts/ci-build-plugins.sh"
