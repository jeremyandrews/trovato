# Contributing to Trovato

Trovato is at 0.101.0 and the work between here and 1.0 is happening in public.
[ROADMAP.md](ROADMAP.md) says what that work is, and the admin screens listed
there are the most approachable place to start.

## Before you write much

Open an issue first for anything beyond a bug fix. That is not process for its
own sake: the kernel has a deliberate boundary (see "Kernel minimality" below)
and it is better to find out that something belongs in a plugin before it is
written rather than after.

## AI-assisted contributions

Trovato is written with substantial AI assistance. Contributions made the same
way are welcome, on the same terms the rest of this file describes. There is no
disclosure requirement and no separate review track.

What is asked of you is the same thing that is asked of any contributor, which
AI assistance makes easier to get wrong:

- **Understand the change you are submitting.** You should be able to explain
  why it works, what it touches, and what happens when it fails. If you cannot,
  it is not ready, however green the tests are.
- **Verify it yourself before submitting.** Run the checks below on your own
  machine. A patch that has only been reasoned about is not a patch that has
  been tested.
- **Check the claims in the description against the code.** Descriptions that
  confidently assert things the diff does not do are the characteristic failure
  here, and they cost a reviewer more time than the change saves.
- **Keep it scoped.** Unrequested refactoring of surrounding code, speculative
  abstraction and drive-by reformatting all make a change harder to review.

Please do not name the tool you used in commit messages, code comments, the
changelog or the pull request. This is a house style about noise, not a position
on AI: attribution trailers and tool mentions accumulate in the history and tell
a future reader nothing they need. The same rule applies to `Co-Authored-By`
trailers of any kind.

## Before submitting

Run the local check script, which mirrors CI:

```bash
./scripts/pre-commit-check.sh          # fmt + clippy + unit tests
./scripts/pre-commit-check.sh --quick  # fmt + clippy only
```

Or the steps by hand:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --all --lib` (unit tests; no database or Redis needed)

### The local gate is stronger than CI

CI splits the integration tests across three shards with three separate
databases, to keep any one runner from exhausting its disk. A local
`cargo test --all` runs every target against **one** database, so interference
between test files through shared fixtures is visible locally and can be
invisible in CI when two files land in different shards.

**CI passing does not imply `cargo test --all` passes.** If you touch anything in
`crates/kernel/tests/common/mod.rs`, the shared `TestApp`, or any other shared
fixture, run the full suite locally against a single database before opening the
pull request.

Shared seeders must stay concurrency-safe. Tests within a binary run in
parallel and every binary shares one database, so a plain `EXISTS` check before
an insert is a race that silently duplicates fixtures. Use a Postgres advisory
lock, as the existing seeders do.

Integration tests need PostgreSQL and Redis. `docker compose up` starts both.

Run them against a **fresh** database, and drop and recreate it between full
runs. A few tests use fixed fixture names and assert exact row counts without
cleaning up, so they pass the first time and fail the second. See
KNOWN-ISSUES.md.

## Coding standards

The full reference is [docs/coding-standards.md](docs/coding-standards.md). The
rules that come up most:

- All new public items need `///` doc comments; all new `.rs` files need `//!`
  module documentation.
- `.unwrap()` is not allowed in production code. Use `.expect("reason")` with a
  `# Panics` section on the enclosing function, or propagate the error.
- A new `#[allow(clippy::...)]` needs a comment explaining why.
- Use Trovato's vocabulary, not Drupal's: category (not taxonomy or vocabulary),
  item (not node), tap (not hook), plugin (not module), gather (not views), tile
  (not block). [docs/design/Terminology.md](docs/design/Terminology.md) has the
  full map.
- Never build SQL with `format!()`. Use SeaQuery's parameterized queries.
- All state-changing endpoints use `require_csrf`.
- Every `| safe` in a Tera template needs a `{# SAFE: reason #}` comment saying
  what already sanitized the value.

## Kernel minimality

The kernel enables; plugins implement. If it is a feature, it is a plugin. If it
is infrastructure that plugins depend on, it is kernel.

A new service in `crates/kernel/src/services/` has to answer one question: does
another kernel subsystem depend on this, or only feature routes? If only feature
routes, it belongs in a plugin. The pull request template asks this, and
[docs/kernel-minimality-audit.md](docs/kernel-minimality-audit.md) has the full
reasoning and the current inventory.

## The plugin contract is frozen

The plugin boundary (the WIT surface, the `trovato-sdk` crate, the manifest
semantics, the error vocabularies) does not change before 1.0. Additive changes
are possible; breaking ones are not.

Note that the tooling cannot enforce this on its own right now. Under SemVer's
0.x rules a breaking change is permitted by a minor bump, so the `SDK Semver
Gate` job will not fail one until the project reaches 1.0.0. Until then it is
held by review. If your change touches the SDK's public surface, say so in the
pull request.

## Versioning

Trovato has one version number, and everything carries it: the kernel, the SDK,
every plugin, every manifest, and the plugin API tuple. Do not bump one of them
on its own. [docs/design/version-map.md](docs/design/version-map.md) lists every
place that moves together.

## Releases

Every tag gets a GitHub Release, with that version's `CHANGELOG.md` section pasted
into the notes. This is not decoration: it is the update channel. GitHub serves
`https://api.github.com/repos/jeremyandrews/trovato/releases/latest` and
`https://github.com/jeremyandrews/trovato/releases.atom` for free, and the kernel's
update check reads the first of them. A tag with no Release is a release no site
learns about.

**A security release's title starts with `[security]`.** For example:

```
[security] 0.99.2 — session fixation in the recovery flow
```

That single convention is the whole difference between "a newer version exists" and
"act now": the latest-release JSON says what the newest version is and has no field
for urgency, so the signal lives in the one field a human writes deliberately. The
kernel reads it (`crates/kernel/src/update_status.rs`, `is_security_title`) and the
admin dashboard styles the banner as an alarm rather than a notice.

The prefix has to lead. A title merely *mentioning* security is not a security
release, or every release note that says the word becomes an emergency.

## Licensing your contribution

Trovato is dual licensed under MIT and Apache-2.0.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
