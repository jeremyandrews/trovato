# Version map

Every place the project version appears, and what it has to say. Trovato has one
version number (see [Versioning.md](Versioning.md)); this is the list of things
that have to move when it changes.

Current version: **0.104.0**, plugin API **(0, 104)**.

## Derived automatically (nothing to do)

These read the version at compile time from `[workspace.package]`. They are
listed so nobody "fixes" them by hardcoding a number.

| Location | Reads |
|---|---|
| every in-tree crate | `version.workspace = true` |
| `crates/kernel/src/main.rs` | `#[command(version)]`, so `trovato --version` |
| `crates/kernel/src/cron/mod.rs` | outbound HTTP user-agent, `Trovato/<version>` |
| `crates/kernel/src/routes/route_metadata.rs` | the OpenAPI document's `info.version` |
| `crates/mcp-server/src/server.rs` | MCP server identification |

## Changed by hand on every version bump

| # | Location | Field | At 0.104.0 |
|---|---|---|---|
| 1 | `Cargo.toml` | `[workspace.package] version` | `"0.104.0"` |
| 2 | `crates/kernel/src/plugin/mod.rs` | `KERNEL_API_VERSION` | `(0, 104)` |
| 3 | `crates/kernel/src/plugin/info_parser.rs` | `default_api_version()` | `"0.104"` |
| 4 | `plugins/**/*.info.toml` (37 files) | `version` | `"0.104.0"` |
| 5 | `plugins/**/*.info.toml` (37 files) | `api_version` | `"0.104"` |
| 6 | `.github/workflows/docker-publish.yml` | `BASE_VERSION` | `"0.104"` |
| 7 | `CHANGELOG.md` | new release section | `## v0.104.0` |
| 8 | `docs/design/Versioning.md` | worked examples | `0.104.0` / `(0, 104)` |
| 9 | this file | the "current version" line and the table | `0.104.0` |
| 10 | `crates/kernel/src/plugin/info_parser.rs` | the two API-compat tests | `"0.104"` accepted, `"0.105"` rejected |
| 11 | `README.md`, `ROADMAP.md`, `CONTRIBUTING.md`, `KNOWN-ISSUES.md`, `.github/ISSUE_TEMPLATE/config.yml` | prose naming the current release | `0.104.0` |
| 12 | `crates/kernel/src/plugin/mod.rs`, `crates/kernel/src/plugin/info_parser.rs`, `.github/workflows/ci.yml`, `plugins/trovato_book/src/lib.rs` | comments naming the current API or contract | `0.104` / `(0, 104)` |
| 13 | `UPGRADING.md` | `## Unreleased` heading, when the release has operator notes | `## v0.104.0` |
| 14 | `docs/RELEASING.md` | the worked `git tag` example in section 4 | `v0.104.0` / `Trovato 0.104.0` |
| 15 | `crates/kernel/src/plugin/info_parser.rs` | the manifest fixtures in `default_api_version_is_the_current_kernel_api` and `explicit_api_version_parses` | `"0.104.0"` / `"0.104"` |

Items 2 and 3 must agree with item 1: the API tuple is the project version with
the patch component dropped. Items 4 and 5 are mechanical across every manifest.

Item 10 is the one that fails the suite rather than merely reading wrong.
`api_compat_same_version_ok` and `api_compat_newer_minor_rejected` in
`info_parser.rs` name minors relative to the kernel: the first has to be the
current minor, the second one above it. Moving `KERNEL_API_VERSION` without
moving them leaves a test asserting that the kernel's own API version requires a
newer kernel, and it fails.

Items 11 and 12 break nothing. They are how the tree speaks its own version, and
leaving them stale is how a reader ends up believing the wrong number.

Item 13 is conditional: `UPGRADING.md` accumulates entries under `## Unreleased`
the way `CHANGELOG.md` does, and a release that gained none has no heading to
close. When there is one, it closes to the same version and the same date as the
changelog section, because the two describe one release and an operator reading
"Unreleased" on a version they are running cannot tell whether the note applies
to them.

Item 14 is the `git tag` command in section 4 of `docs/RELEASING.md`, written out
with a real version rather than the `vX.Y.Z` the rest of that section uses. It is
the one stale number a reader is most likely to paste into a terminal.

Item 15 is two test fixtures that spell out a manifest at the current version.
Unlike item 10 they pass whatever version they name, because each asserts against
the number it just set, so nothing fails when they are missed. They are listed
because the leftover grep reads them as present tense and a maintainer chasing
that hit should find the row rather than decide for themselves.

## Deliberately not the project version

| Location | Version | Why |
|---|---|---|
| `benchmarks/phase0/guest/Cargo.toml` | `0.1.0` | A benchmark fixture, kept out of the root workspace so it can set its own release profile, so it cannot inherit `version.workspace`. Never released. |

## Checking the work

The useful grep after a bump looks for the version that was left behind, not the
new one. Substitute the previous version; at 0.104.0 that was 0.103:

```sh
grep -rn '0\.103\|(0, 103)' --include='*.rs' --include='*.toml' --include='*.md' \
  --include='*.yml' --include='*.wit' . | grep -v Cargo.lock | grep -v './target/'
```

Every surviving hit has to be history: a `CHANGELOG.md` entry, an "added in" or
"shipped in", a test fixture deliberately holding an older manifest, or a number
that was never ours, such as a 0.99 percentile. Anything written in the present
tense was missed.

The direct check is to build and ask:

```sh
cargo build --release
./target/release/trovato --version          # 0.104.0
grep '^version' Cargo.toml                  # 0.104.0
grep -rh '^api_version' plugins --include='*.info.toml' | sort -u   # one line
grep -rh '^version' plugins --include='*.info.toml' | sort -u       # one line
```

The last two are the useful ones: if either prints more than one line, a manifest
was missed.
