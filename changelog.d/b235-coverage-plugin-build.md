CI's Coverage job builds the same WASM plugins as the test shards again, and
cannot quietly fall behind them.

The Test and Coverage jobs each kept their own hand-written list of plugins to
build, and the change that added the search index writer tests (B218) added
`test_search_enqueue` and `trovato_scheduled_publishing` to the Test job's list
only. Coverage runs only on pushes to `main`, so no pull request showed it, and
from that commit on it failed on every push: three tests in
`search_index_writers_test` could not find `trovato_scheduled_publishing.wasm`.
The list now lives in one script, `scripts/ci-build-plugins.sh`, which the Test,
Coverage and Build jobs and `scripts/pre-commit-check.sh` all call. A new
Plugin Build Parity Guard job runs on pull requests and fails when the Test or
Coverage job stops calling it, or when any job names plugins of its own on a
`wasm32-wasip1` build.
