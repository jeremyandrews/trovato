# Security Dependency Audit Policy

## Automated Scanning

`cargo audit` runs automatically in CI on every pull request and push to `main`.
The job uses the [RustSec Advisory Database](https://rustsec.org/) to check all
transitive dependencies for known vulnerabilities.

## Response SLA

| Severity | Response Time | Action |
|----------|--------------|--------|
| Critical/High | 1 week | Update dependency or apply mitigation |
| Medium | 2 weeks | Update dependency or suppress with justification |
| Low | Next release cycle | Update or suppress |
| Unmaintained warning | Quarterly review | Evaluate alternatives |

## Advisory Suppression

When an advisory cannot be immediately resolved (no fix available, or the
vulnerability does not affect our usage), suppress it in `.cargo/audit.toml`
with:

1. The advisory ID (e.g., `RUSTSEC-2023-0071`)
2. A comment explaining why suppression is acceptable
3. A review date for re-evaluation

Example:

```toml
[advisories]
ignore = [
    # rsa timing sidechannel — transitive via sqlx-mysql, we only use postgres.
    # Review date: 2026-06-01
    "RUSTSEC-2023-0071",
]
```

## Current Suppressions

See `.cargo/audit.toml` for the current list of suppressed advisories with
justifications.

## Quarterly Review Process

Every quarter, review `.cargo/audit.toml` suppressions:

1. Check if fixes are now available for suppressed advisories
2. Update dependencies where possible
3. Remove suppressions for resolved advisories
4. Update review dates for advisories that remain suppressed
