# Version map

Every place the project version appears, and what it has to say. Trovato has one
version number (see [Versioning.md](Versioning.md)); this is the list of things
that have to move when it changes.

Current version: **0.99.0**, plugin API **(0, 99)**.

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

| # | Location | Field | At 0.99.0 |
|---|---|---|---|
| 1 | `Cargo.toml` | `[workspace.package] version` | `"0.99.0"` |
| 2 | `crates/kernel/src/plugin/mod.rs` | `KERNEL_API_VERSION` | `(0, 99)` |
| 3 | `crates/kernel/src/plugin/info_parser.rs` | `default_api_version()` | `"0.99"` |
| 4 | `plugins/**/*.info.toml` (36 files) | `version` | `"0.99.0"` |
| 5 | `plugins/**/*.info.toml` (36 files) | `api_version` | `"0.99"` |
| 6 | `.github/workflows/docker-publish.yml` | `BASE_VERSION` | `"0.99"` |
| 7 | `CHANGELOG.md` | new release section | `## v0.99.0` |
| 8 | `docs/design/Versioning.md` | worked examples | `0.99.0` / `(0, 99)` |
| 9 | this file | the "current version" line and the table | `0.99.0` |

Items 2 and 3 must agree with item 1: the API tuple is the project version with
the patch component dropped. Items 4 and 5 are mechanical across every manifest.

## Deliberately not the project version

| Location | Version | Why |
|---|---|---|
| `benchmarks/phase0/guest/Cargo.toml` | `0.1.0` | A benchmark fixture, kept out of the root workspace so it can set its own release profile, so it cannot inherit `version.workspace`. Never released. |

## Checking the work

After a bump, this should return only third-party dependency versions and
unrelated numeric tuples, never one of ours:

```sh
grep -rn "0\.99\|(0, 99)" --include='*.rs' --include='*.toml' --include='*.md' . \
  | grep -v Cargo.lock
```

The direct check is to build and ask:

```sh
cargo build --release
./target/release/trovato --version          # 0.99.0
grep '^version' Cargo.toml                  # 0.99.0
grep -rh '^api_version' plugins --include='*.info.toml' | sort -u   # one line
grep -rh '^version' plugins --include='*.info.toml' | sort -u       # one line
```

The last two are the useful ones: if either prints more than one line, a manifest
was missed.
