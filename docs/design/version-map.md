# Version map

Trovato has one version number (see [Versioning.md](Versioning.md)). This page
explains how that one number reaches everything that repeats it.

<!-- version:begin -->
Current version: **0.104.0**, plugin API **(0, 104)**.
<!-- version:end -->

That line is generated. So is nearly everything below it: the only thing a
release changes by hand is `[workspace.package] version` in the root
`Cargo.toml`, plus a `CHANGELOG.md` entry, which is writing rather than
bookkeeping.

## A bump, start to finish

```sh
# 1. Edit the one authored copy.
$EDITOR Cargo.toml            # [workspace.package] version

# 2. Write it everywhere else.
./scripts/sync-version.sh

# 3. Describe the release in your own words.
$EDITOR CHANGELOG.md
```

`cargo test` then fails by name if anything disagrees. There is no list to work
through and nothing to remember, which is the point: the list used to be
fifteen rows long and two of those rows were "every one of the plugin
manifests".

## What derives the version, and how

### The compiler

Nothing to do, and nothing that can go stale. These are listed so that nobody
"fixes" one by hardcoding a number.

| Location | How |
|---|---|
| every in-tree crate | `version.workspace = true` |
| `crates/kernel/src/plugin/mod.rs` | `KERNEL_API_VERSION` is parsed from `CARGO_PKG_VERSION_MAJOR` and `CARGO_PKG_VERSION_MINOR` by a `const fn` |
| `crates/kernel/src/plugin/info_parser.rs` | `default_api_version()` formats `KERNEL_API_VERSION` |
| `crates/kernel/src/plugin/info_parser.rs` | the API compatibility tests build their accepted and rejected versions from `KERNEL_API_VERSION` and the minor above it |
| `crates/kernel/src/main.rs` | `#[command(version)]`, so `trovato --version` |
| `crates/kernel/src/cron/mod.rs` | outbound HTTP user-agent, `Trovato/<version>` |
| `crates/kernel/src/routes/route_metadata.rs` | the OpenAPI document's `info.version` |
| `crates/mcp-server/src/server.rs` | MCP server identification |

The API tuple is the project version with the patch component dropped. It is
derived rather than declared, so it cannot disagree with the version it is
supposed to follow.

### The publish workflow

`.github/workflows/docker-publish.yml` reads the major and minor out of
`Cargo.toml` in its first step and exports `BASE_VERSION`, which the nightly tag
arithmetic then uses. The workflow carries no copy of the version.

### `scripts/sync-version.sh`

Run after a bump; takes no arguments, because there is nothing to tell it that
`Cargo.toml` does not already say.

| Location | Field |
|---|---|
| `plugins/**/*.info.toml` | `version` and `api_version`, in every manifest |
| `docs/design/Versioning.md` | the five worked examples |
| this file | the current-version line |
| `README.md`, `ROADMAP.md`, `CONTRIBUTING.md`, `KNOWN-ISSUES.md` | the sentence naming the current release |
| `docs/RELEASING.md` | the worked `git tag` example |
| `SECURITY.md` | the supported-versions table |
| `.github/ISSUE_TEMPLATE/config.yml` | the line naming the current release |

In the Markdown files the generated text sits between a `version:begin` and a
`version:end` HTML comment, and only what is between the markers is rewritten.
Prose outside them is written to be durable: where a sentence used to name the
current release in passing, it now describes the rule instead, and a comment
that records what was true when it was written needs no bump and is not on this
list.

The plugin manifests are the reason the script exists. The loader reads them at
run time, so they hold literal strings rather than anything derived, and there
are dozens of them.

### The test

`crates/kernel/tests/version_sync.rs` walks every manifest and every marker
block and asserts each one names the current version. One manifest out of step
fails the suite and names the file. That is what makes the literals on disk safe
to leave as literals.

## Still a human act

`CHANGELOG.md` gets a new section per release, and `UPGRADING.md` closes its
`## Unreleased` heading when the release has operator notes. Both are writing,
not bookkeeping, and neither is generated.

## Deliberately not the project version

| Location | Version | Why |
|---|---|---|
| `benchmarks/phase0/guest/Cargo.toml` | `0.1.0` | A benchmark fixture, kept out of the root workspace so it can set its own release profile, so it cannot inherit `version.workspace`. Never released. |

## Checking the work

The test is the check. The grep below stays useful as an independent one,
because it answers a question the test cannot: whether some prose nobody
thought to mark up is still talking about the previous release. Substitute the
version that was just left behind:

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
./target/release/trovato --version          # the workspace version
grep -rh '^api_version' plugins --include='*.info.toml' | sort -u   # one line
grep -rh '^version' plugins --include='*.info.toml' | sort -u       # one line
```

The last two are the useful ones: if either prints more than one line, a
manifest is out of step, and the test will already have said which.
