# Security Dependency Audit Policy

## Automated Scanning

`cargo audit` runs in two places, against the same lockfile and the same
suppression list. What differs is which copy of the
[RustSec Advisory Database](https://rustsec.org/) each one reads.

| Job | Runs on | Advisory database | What a failure means |
|---|---|---|---|
| `Security Audit`, in `.github/workflows/ci.yml` | every pull request and push to `main` | the commit pinned in `.cargo/advisory-db-pin`, fetching disabled | this branch has a vulnerable dependency |
| `Live Security Audit`, in `.github/workflows/security-audit.yml` | `main`, daily at 06:40 UTC, and on demand | fetched live | `main` has a vulnerable dependency, possibly published this morning |

Both are `scripts/security-audit.sh`, which is also how either one is reproduced
locally:

```sh
./scripts/security-audit.sh          # what a pull request runs
./scripts/security-audit.sh --live   # what the daily job runs
```

A failure of the daily job opens an issue naming the advisory and the crate, or
comments on the one already open rather than filing a duplicate. A clean run
closes it, so no open issue means the last run was clean rather than that nobody
looked.

## The pinned advisory database

`cargo audit` fetches the advisory database before it scans, so a bare run
answers a question that changes under it: not "does this branch have a vulnerable
dependency" but "does it have one as of whenever the job happened to start". Every
open branch went red the morning an advisory landed against a crate none of them
had touched, and the only cure was to rebase and burn another full CI cycle. A red
audit had stopped saying anything about the branch it was attached to.

So a pull request audits against one commit of the database, recorded in
`.cargo/advisory-db-pin`, with fetching disabled. A branch then fails only for an
advisory that already existed when the pin was set, which is a real finding about
its own dependencies, and the same commit audited twice gives the same answer.

**The pin is a floor, and a floor that never moves stops being one.** Every
advisory published after the pin is invisible to every pull request. That is the
one real risk this arrangement introduces, so it is gated rather than trusted:
`scripts/security-audit.sh` fails, in CI and locally, on a pin more than 90 days
old, with instructions. The window matches the quarterly suppression review below,
because the two are the same visit to the same question.

### Moving the pin

In its own pull request, so that raising the floor is a reviewed act with a date
on it:

1. Read the current commit: `git ls-remote https://github.com/rustsec/advisory-db.git HEAD`
2. Put it in `.cargo/advisory-db-pin`, and record in the comment above it the date
   you set it.
3. Run `./scripts/security-audit.sh` and read what the newer floor reports.
4. If it reports something real, that is a separate pull request: upgrade the
   dependency, and land that before or alongside the bump.

**Never move the pin past a finding.** An advisory is answered by upgrading the
dependency, or, when it genuinely does not apply to how Trovato uses the crate, by
suppressing it in `.cargo/audit.toml` with a justification and a review date (see
below). Moving the pin forward past one does neither: it hides the finding from
every pull request while leaving it live on `main`, where the daily job goes on
reporting it and no branch ever shows it again. That is the single way to turn
this mechanism into something weaker than the bare `cargo audit` it replaced.

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
