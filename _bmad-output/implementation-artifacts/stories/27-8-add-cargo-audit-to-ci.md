# Story 27.8: Add cargo-audit to CI

Status: ready-for-dev

## Story

As a **maintainer**,
I want automated dependency vulnerability scanning in CI,
So that known vulnerabilities in dependencies are caught before they reach production.

## Acceptance Criteria

1. `cargo audit` runs in CI on every PR and push to main
2. Build fails on any known vulnerability in the RustSec advisory database
3. Process documented for handling advisories (update within 1 week for security-critical crates)

## Tasks / Subtasks

- [ ] Add `cargo-audit` job to `.github/workflows/ci.yml` (AC: #1)
  - [ ] Add new job that installs cargo-audit and runs `cargo audit`
  - [ ] Configure to run on PRs and pushes to main
  - [ ] Ensure job fails the build on advisory findings (AC: #2)
- [ ] Verify cargo-audit works locally (AC: #2)
  - [ ] Run `cargo audit` locally to confirm no current advisories
  - [ ] Verify exit code behavior (non-zero on findings)
- [ ] Document dependency update cadence (AC: #3)
  - [ ] Add security dependency policy to `docs/security-audit.md`
  - [ ] Document 1-week SLA for security-critical crate updates
  - [ ] Document process for advisory suppression when no fix is available

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

### References

- [RustSec Advisory Database](https://rustsec.org/)
- [cargo-audit documentation](https://docs.rs/cargo-audit/)
