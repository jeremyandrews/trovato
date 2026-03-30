# Story 42.8: SecretConfigProvider for Sensitive Configuration

Status: ready-for-dev

## Story

As a site operator deploying to production,
I want sensitive configuration storable outside the database,
so that secrets like API keys are not exposed in database dumps, config exports, or admin UI displays.

## Acceptance Criteria

1. `ConfigStorage` trait extended with a `SecretConfigProvider` variant
2. First implementation reads secrets from environment variables
3. Config values prefixed with `"env:"` are resolved at read time: `"api_key": "env:OPENAI_API_KEY"` reads the `OPENAI_API_KEY` env var
4. Resolution happens at read time, not storage time (env var changes take effect without restart)
5. Admin UI shows the `"env:VARIABLE_NAME"` reference string, not the resolved secret value
6. Config export writes the `"env:"` prefix (safe to commit to version control)
7. Config import accepts `"env:"` values and stores them as-is (does not resolve during import)
8. Escape mechanism: values prefixed with `"literal:"` bypass prefix resolution — `"literal:env:NOT_A_SECRET"` stores and returns the literal string `"env:NOT_A_SECRET"`. The `"literal:"` prefix is stripped at read time.
9. Audit: existing config values in the database scanned for accidental `"env:"` prefixes during the first deployment. If any exist, they must be wrapped with `"literal:"` or the operator warned.
10. At least 2 integration tests: env var resolution, and `"literal:"` escape mechanism
11. "Managing Secrets" operational documentation created

## Tasks / Subtasks

- [ ] Extend `ConfigStorage` trait with `SecretConfigProvider` variant in `crates/kernel/src/config/` (AC: #1)
- [ ] Implement env var resolution: detect `"env:"` prefix, read env var, return value (AC: #2, #3)
- [ ] Ensure resolution happens at read time, not at storage/cache time (AC: #4)
- [ ] Update admin config UI to display `"env:VAR_NAME"` without resolving (AC: #5)
- [ ] Update config export to preserve `"env:"` prefix in output (AC: #6)
- [ ] Update config import to store `"env:"` values verbatim (AC: #7)
- [ ] Handle missing env vars gracefully: return error or configurable fallback (AC: #3)
- [ ] Implement `"literal:"` prefix escape: strip prefix at read time, return remainder as-is (AC: #8)
- [ ] Add startup audit: scan `site_config` for values starting with `"env:"` — log warning for any pre-existing values that may be unintentionally interpreted as secret references (AC: #9)
- [ ] Write integration test: store `"env:TEST_SECRET"`, set env var, read resolves to env value (AC: #10)
- [ ] Write integration test: store `"literal:env:NOT_A_SECRET"`, read returns `"env:NOT_A_SECRET"` (AC: #10)
- [ ] Write integration test: config export contains `"env:TEST_SECRET"` not the resolved value (AC: #6, #10)
- [ ] Create `docs/operations/managing-secrets.md` with usage guide (AC: #11)

## Dev Notes

### Architecture

The `"env:"` prefix is a convention applied at the config value level, not the storage level. When `ConfigStorage::get()` returns a value starting with `"env:"`, a resolution layer strips the prefix and reads the named env var. This resolution layer sits between the storage backend and the caller, acting as a transparent decorator.

Read-time resolution (AC #4) means the value is not cached after resolution -- each `get()` call re-reads the env var. This allows secret rotation without server restart. If performance is a concern, a short TTL cache (30s) could be added later, but env var reads are fast (~microseconds).

Future implementations could support other secret backends (Vault, AWS Secrets Manager) by extending the prefix convention: `"vault:path/to/secret"`, `"aws-sm:secret-name"`.

### Security

- The `"env:"` prefix must never be resolved in the admin UI or config export paths. These paths should call a separate method (e.g., `get_raw()`) that returns the stored value without resolution.
- Missing env vars should return a clear error (e.g., `ConfigError::SecretNotFound { var_name }`) rather than silently returning an empty string or the `"env:..."` prefix as the value.
- Config export with `"env:"` prefixes is safe to commit because it contains no actual secrets -- only references to env vars that must be set in the deployment environment.
- Consider logging a warning (not the value!) when an `"env:"` config key is accessed but the env var is not set.

### Testing

- Set `std::env::set_var("TEST_SECRET", "hunter2")` in the test, store `"env:TEST_SECRET"` as a config value, assert `get()` returns `"hunter2"`.
- Test export: assert the exported JSON/YAML contains `"env:TEST_SECRET"` literally.
- Test missing env var: assert `get()` returns an appropriate error.
- Test that non-prefixed values are returned unchanged (backward compatibility).
- Test `"literal:"` escape: store `"literal:env:FOO"`, assert `get()` returns `"env:FOO"` without attempting env var resolution.
- Test startup audit: insert a row with value `"env:ACCIDENTAL"` before the feature is deployed, verify the audit logs a warning.

### References

- `crates/kernel/src/config/` -- config storage trait
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
