# Releasing

How a Trovato release is cut. Reconstructed from `v0.101.0` and the `Trovato
0.102.0` sweep commit, and written down so the next one is not reverse
engineered again.

Three artifacts have to agree, because three different consumers read them
independently:

| Artifact | Who reads it |
|---|---|
| the git tag | a docs importer fetching `raw.githubusercontent.com` content at the tag |
| the container image with the same version | compose files that pull it by tag, and ledgers that record its digest |
| the commit the tag points at | sites pinning `trovato-sdk` to it in `Cargo.toml` |

A release where any two of those disagree is worse than no release: each
consumer is separately convinced it has the right thing.

## 1. The version sweep

Trovato has one version number and everything carries it. Move every place
[design/version-map.md](design/version-map.md) lists, in **one commit**, titled
`Trovato X.Y.Z`. That file is the checklist; do not work from memory, and note
that item 10 fails the suite rather than merely reading wrong.

Minor or patch is decided by what shipped, not by how large the diff is. New
public context variables that a theme can come to depend on are a minor bump,
because a theme written against them will not work on the previous release.

The sweep commit lands before the changes it versions, or after them. Either
works. What matters is that the tag goes on a commit where the declared version,
the changelog section and the code all say the same thing.

## 2. The changelog section

Every entry names a root cause, not a symptom. The section heading is
`## vX.Y.Z — YYYY-MM-DD`, and the date is the day it is tagged.

Entries accumulate under `## Unreleased` as work merges. At release time they
move into that version's section. If the sweep commit already opened the
section, fold `Unreleased` into it rather than opening a second one, and restamp
the date to the day of the tag.

## 3. The gates

Run these on the exact commit that will be tagged. Nothing else is worth
gating: a green run on a commit that will not be tagged proves nothing about the
one that will.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --all -- --test-threads=1     # needs Postgres and Redis
cargo audit
cargo semver-checks check-release --manifest-path crates/plugin-sdk/Cargo.toml
```

CI runs the suite in three shards against a database each, so **CI green does
not imply `cargo test --all` green**: two tests that contend for one fixture can
land in different shards and never meet. Run the whole suite locally before
tagging.

`cargo semver-checks` is the one that matters most. The plugin contract is
frozen: a kernel change must not move the SDK's public surface, and this check is
the proof rather than the claim.

## 4. The tag

Annotated, named `vX.Y.Z`, message `Trovato X.Y.Z`:

```sh
git tag -a v0.102.0 -m "Trovato 0.102.0"
git push origin v0.102.0
```

`.github/workflows/docker-publish.yml` runs on `refs/tags/v*` and publishes
`ghcr.io/jeremyandrews/trovato` at `X.Y.Z`, `X.Y` and `latest`, built for amd64
and arm64 (arm64 is release-only; nightlies skip it and take up to four hours
when it does run).

That workflow carries `paths-ignore: ['**/*.md']`. A release commit that changes
only markdown can therefore fail to trigger it. **Confirm the run started**, and
if it did not, dispatch it against the tag rather than pushing an empty commit:

```sh
gh run list --workflow=docker-publish.yml --limit 3
gh workflow run docker-publish.yml --ref v0.102.0   # only if it did not fire
```

## 5. The GitHub Release

**Every tag gets a Release.** It is not decoration: the kernel's update check
reads `releases/latest`, so a tag without a Release is a release no site learns
about. Paste that version's changelog section into the notes.

A security release's title leads with `[security]`, per
[CONTRIBUTING.md](../CONTRIBUTING.md). The prefix has to lead: a title that
merely mentions security is not a security release, or every note that says the
word becomes an emergency. `is_security_title` in
`crates/kernel/src/update_status.rs` is what reads it, and the admin dashboard
styles that banner as an alarm rather than a notice.

## 6. Verify from the consumer's side

The publishing side almost always looks fine. Check the side that breaks:

```sh
curl -fsSL https://raw.githubusercontent.com/jeremyandrews/trovato/vX.Y.Z/README.md | head -3
docker pull ghcr.io/jeremyandrews/trovato:X.Y.Z
docker buildx imagetools inspect ghcr.io/jeremyandrews/trovato:X.Y.Z   # note the digest
docker pull ghcr.io/jeremyandrews/trovato@sha256:<digest>
```

Then compile something against the tagged commit, which is what a site pinning
`trovato-sdk` actually does:

```sh
cargo new /tmp/sdk-pin-check && cd /tmp/sdk-pin-check
cargo add trovato-sdk --git https://github.com/jeremyandrews/trovato --rev <tagged-commit>
cargo check
```

Release notes end with the three values a consuming site needs verbatim: the
tag, the image's `sha256` digest, and the tagged commit hash.
