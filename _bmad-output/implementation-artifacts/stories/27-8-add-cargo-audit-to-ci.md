# Story 27.8: Add cargo-audit to CI

Status: done

## Story

As a **maintainer**,
I want automated dependency vulnerability scanning in CI,
So that known vulnerabilities in dependencies are caught before they reach production.

## Acceptance Criteria

1. `cargo audit` runs in CI on every PR and push to main
2. Build fails on any known vulnerability in the RustSec advisory database
3. Process documented for handling advisories (update within 1 week for security-critical crates)

## Tasks / Subtasks

- [x] Add `cargo-audit` job to `.github/workflows/ci.yml` (AC: #1)
  - [x] Add new job that installs cargo-audit and runs `cargo audit`
  - [x] Configure to run on PRs and pushes to main
  - [x] Ensure job fails the build on advisory findings (AC: #2)
- [x] Verify cargo-audit works locally (AC: #2)
  - [x] Run `cargo audit` locally to confirm no current advisories
  - [x] Verify exit code behavior (non-zero on findings)
- [x] Document dependency update cadence (AC: #3)
  - [x] Add security dependency policy to `docs/security-audit.md`
  - [x] Document 1-week SLA for security-critical crate updates
  - [x] Document process for advisory suppression when no fix is available

## Dev Notes

### CI Configuration

The existing CI workflow is at `.github/workflows/ci.yml`. The cargo-audit job should:
- Use `actions-rs/audit-check` or install `cargo-audit` directly via `cargo install cargo-audit`
- Run as a separate job that can fail independently of build/test jobs
- Be fast (cargo-audit only checks the advisory database, no compilation needed)

### Advisory Handling

When `cargo audit` finds an advisory:
1. **Fix available**: Update the dependency within 1 week
2. **No fix available**: Add to `audit.toml` with justification and review date
3. **False positive**: Suppress with comment explaining why

### Implementation Details

#### CI Job

Added `audit` job to `.github/workflows/ci.yml`:
- Installs `cargo-audit` via `cargo install`
- Runs `cargo audit` which checks `.cargo/audit.toml` for suppressions
- Fails the build on any unacknowledged advisory

#### Advisory Suppressions

Created `.cargo/audit.toml` with 3 suppressed advisories:

| Advisory | Crate | Justification |
|----------|-------|---------------|
| RUSTSEC-2023-0071 | rsa (via sqlx-mysql) | Timing sidechannel; we only use postgres, not mysql |
| RUSTSEC-2025-0046 | wasmtime | fd_renumber host panic; we return ENOSYS for all fd operations |
| RUSTSEC-2025-0118 | wasmtime | Shared memory unsoundness; we don't use wasm shared memory |

Each suppression includes a review date of 2026-06-01.

#### Documentation

Created `docs/security-audit.md` with:
- Response SLA table (Critical/High: 1 week, Medium: 2 weeks, Low: next cycle)
- Advisory suppression process with required fields
- Quarterly review process for re-evaluating suppressions

### Files Changed

- `.github/workflows/ci.yml` — Added `audit` job
- `.cargo/audit.toml` — New file with 3 suppressed advisories
- `docs/security-audit.md` — New dependency security policy document

### References

- [RustSec Advisory Database](https://rustsec.org/)
- [cargo-audit documentation](https://docs.rs/cargo-audit/)
