# Story 39.6: Comprehensive Test Suite

Status: done

## Story

As a **developer**,
I want a comprehensive test suite with unit tests, integration tests, load testing, and CI automation,
so that regressions are caught early and the codebase maintains high quality as it evolves.

## Acceptance Criteria

1. 1,089+ unit tests across the codebase covering all major subsystems
2. Integration test suites covering content, categories, gathers, plugins, stages, URL aliases, search, forms, themes, cron, cache
3. Plugin WASM functions testable via `__inner_*` pattern that exposes logic without WASM boundary
4. Load test tool in `benchmarks/load-test/` for HTTP endpoint benchmarking
5. CI pipeline with 8 jobs: fmt, clippy, test, coverage, build, doc, audit, terminology
6. CI runs tests with PostgreSQL 16 and Redis 7 service containers
7. WASM plugins (blog, trovato_search) built as part of CI before test execution
8. Terminology check enforces Trovato naming conventions (no Drupal terms in source)

## Tasks / Subtasks

- [x] Write unit tests for all kernel subsystems: content, gather, cache, config_storage, cron, file, forms, metrics, middleware, models, plugins, routes, search, services, stage, tap, theme (AC: #1)
- [x] Write integration test suites: integration_test.rs (135 tests), item_test.rs (47 tests), gather_test.rs (24 tests), tutorial_test.rs (22 tests), plugin_test.rs (21 tests), category_test.rs (13 tests), stage_test.rs (14 tests), url_alias_test.rs (12 tests), form_test.rs (10 tests), theme_test.rs (9 tests), search_test.rs (5 tests), stage_aware_config_test.rs (5 tests), cron_test.rs (4 tests), cache_test.rs (4 tests) (AC: #2)
- [x] Implement `__inner_*` function pattern for testing WASM plugin logic natively (AC: #3)
- [x] Create load test tool with HTTP client benchmarking (AC: #4)
- [x] Configure GitHub Actions CI workflow with 8 jobs (AC: #5)
- [x] Configure PostgreSQL 16 and Redis 7 service containers in CI (AC: #6)
- [x] Add WASM plugin build step (blog + trovato_search) before test execution (AC: #7)
- [x] Implement terminology check job scanning for Drupal terms with context-aware exclusions (AC: #8)

## Dev Notes

### Architecture

The test infrastructure uses a shared runtime pattern to avoid connection pool exhaustion. All integration tests use `#[test]` + `run_test(async { ... })` on a shared `SHARED_RT` Tokio runtime (not `#[tokio::test]`). A single `TestApp` is lazily initialized via `shared_app()` returning `&'static TestApp`, preventing PgPool connection staling across runtimes.

Tests that mutate shared state use serialization locks (`RwLock` / `Mutex`) to prevent interference in parallel execution. Login helpers derive per-username fake IPs to avoid rate limit collisions. Test Argon2 params use minimal settings (4 MiB, 1 iter, 1 lane) for speed.

The `__inner_*` pattern allows WASM plugin functions to be tested as native Rust by extracting the core logic into a non-WASM function that the exported WASM function delegates to.

The CI pipeline is structured for fast feedback: `fmt` and `clippy` run first (no services needed), then `test`/`coverage`/`build` run with database services, and `doc`/`audit`/`terminology` run independently.

### Testing

This story IS the test infrastructure. Key test files:

| File | Tests |
|------|-------|
| `crates/kernel/tests/integration_test.rs` | 135 |
| `crates/kernel/tests/item_test.rs` | 47 |
| `crates/kernel/tests/gather_test.rs` | 24 |
| `crates/kernel/tests/tutorial_test.rs` | 22 |
| `crates/kernel/tests/plugin_test.rs` | 21 |
| `crates/kernel/tests/category_test.rs` | 13 |
| `crates/kernel/tests/stage_test.rs` | 14 |
| `crates/kernel/tests/url_alias_test.rs` | 12 |
| `crates/kernel/tests/form_test.rs` | 10 |
| `crates/kernel/tests/theme_test.rs` | 9 |
| `crates/kernel/tests/stage_aware_config_test.rs` | 5 |
| `crates/kernel/tests/search_test.rs` | 5 |
| `crates/kernel/tests/cron_test.rs` | 4 |
| `crates/kernel/tests/cache_test.rs` | 4 |
| `crates/mcp-server/tests/mcp_test.rs` | 16 |

### References

- `crates/kernel/tests/common/mod.rs` -- shared test infrastructure (TestApp, SHARED_RT, locks)
- `benchmarks/load-test/src/main.rs` (327 lines) -- HTTP load test tool
- `.github/workflows/ci.yml` -- CI pipeline configuration (8 jobs)
