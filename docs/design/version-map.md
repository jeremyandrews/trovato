# Version map

Every place the project version appears, and what it has to say. Trovato has one
version number (see [Versioning.md](Versioning.md)); this is the list of things
that have to move when it changes.

Current version: **0.100.0**, plugin API **(0, 100)**.

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

| # | Location | Field | At 0.100.0 |
|---|---|---|---|
| 1 | `Cargo.toml` | `[workspace.package] version` | `"0.100.0"` |
| 2 | `crates/kernel/src/plugin/mod.rs` | `KERNEL_API_VERSION` | `(0, 100)` |
| 3 | `crates/kernel/src/plugin/info_parser.rs` | `default_api_version()` | `"0.100"` |
| 4 | `plugins/**/*.info.toml` (35 files) | `version` | `"0.100.0"` |
| 5 | `plugins/**/*.info.toml` (35 files) | `api_version` | `"0.100"` |
| 6 | `.github/workflows/docker-publish.yml` | `BASE_VERSION` | `"0.100"` |
| 7 | `CHANGELOG.md` | new release section | `## v0.100.0` |
| 8 | `docs/design/Versioning.md` | worked examples | `0.100.0` / `(0, 100)` |
| 9 | this file | the "current version" line and the table | `0.100.0` |
| 10 | `crates/kernel/src/plugin/info_parser.rs` | the two API-compat tests | `"0.100"` accepted, `"0.101"` rejected |
| 11 | `README.md`, `ROADMAP.md`, `CONTRIBUTING.md`, `KNOWN-ISSUES.md`, `.github/ISSUE_TEMPLATE/config.yml` | prose naming the current release | `0.100.0` |
| 12 | `crates/kernel/src/plugin/mod.rs`, `crates/kernel/src/plugin/info_parser.rs`, `.github/workflows/ci.yml`, `plugins/trovato_book/src/lib.rs` | comments naming the current API or contract | `0.100` / `(0, 100)` |

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

## Deliberately not the project version

| Location | Version | Why |
|---|---|---|
| `benchmarks/phase0/guest/Cargo.toml` | `0.1.0` | A benchmark fixture, kept out of the root workspace so it can set its own release profile, so it cannot inherit `version.workspace`. Never released. |

## Checking the work

The useful grep after a bump looks for the version that was left behind, not the
new one. Substitute the previous version; at 0.100.0 that was 0.99:

```sh
grep -rn '0\.99\|(0, 99)' --include='*.rs' --include='*.toml' --include='*.md' \
  --include='*.yml' --include='*.wit' . | grep -v Cargo.lock | grep -v './target/'
```

Every surviving hit has to be history: a `CHANGELOG.md` entry, an "added in" or
"shipped in", a test fixture deliberately holding an older manifest, or a number
that was never ours, such as a 0.99 percentile. Anything written in the present
tense was missed.

The direct check is to build and ask:

```sh
cargo build --release
./target/release/trovato --version          # 0.100.0
grep '^version' Cargo.toml                  # 0.100.0
grep -rh '^api_version' plugins --include='*.info.toml' | sort -u   # one line
grep -rh '^version' plugins --include='*.info.toml' | sort -u       # one line
```

The last two are the useful ones: if either prints more than one line, a manifest
was missed.
