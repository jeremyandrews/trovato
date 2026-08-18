# Known issues

What is outstanding in 0.99.0. This is the list that would otherwise be a
surprise, so it is written down rather than discovered.

Trovato was developed privately and is published as a pre-1.0 release for
exactly the reasons on this page. Nothing here is a secret being managed; it is
a backlog being worked in the open. [ROADMAP.md](ROADMAP.md) says what happens
to each item.

## Security

### Security findings from private development are not independently verified

Several security audits were run during private development and their findings
were addressed, but the fixes have not been re-verified by anyone other than the
person who made them, and the audits themselves were not independent. Treat the
security posture as "reviewed once, by the author" until that changes. Reviewing
those findings on a public codebase is a 1.0 blocker.

### Page-builder components accept arbitrary inline CSS

`crates/kernel/src/content/page_builder.rs:91` carries a TODO to restrict
allowed CSS properties to an allowlist. The Ammonia sanitizer currently permits
the `style` attribute wholesale, so a component's markup can carry any
declaration it likes. HTML tags and non-style attributes are constrained; CSS
properties are not.

This is related to the item below and the two should be fixed together.

### Content-Security-Policy still allows inline styles

`style-src` keeps `'unsafe-inline'`
(`crates/kernel/src/middleware/security_headers.rs`) because the base template
and the admin screens carry inline `style=` attributes. `script-src` no longer
needs it, so this is the last inline exception. Extracting the styles allows the
directive to be tightened, which is also what makes the page-builder allowlist
above worth having.

### Dependency advisories are suppressed with justifications

`cargo audit` runs in CI and `.cargo/audit.toml` lists what is suppressed and
why. Each entry has reasoning; none is suppressed silently. The open ones:

Every wasmtime and cranelift advisory is **fixed rather than suppressed**:
RUSTSEC-2026-0085 through -0096, -0114 and -0222 all cleared by upgrading the
runtime to wasmtime 47.0.3. Nothing about the plugin sandbox is being carried on
a justification.

Five suppressions remain, none of them in the WASM runtime:

- **RUSTSEC-2026-0194 / RUSTSEC-2026-0195** (quick-xml denial of service): the
  live one. Reachable only through `plist` and `syntect`, which parse the
  bundled syntax-highlighting theme files at startup rather than anything an
  attacker supplies. The fix is quick-xml 0.41, and `plist` pins `^0.38`, so it
  needs an upstream release before Trovato can take it.
- **RUSTSEC-2026-0189** (rmcp DNS rebinding, CVSS 8.8): not compiled. The
  vulnerable code is rmcp's Streamable HTTP server transport, behind the
  `transport-streamable-http-server` feature. `trovato-mcp` enables only
  `transport-io` and serves over STDIO, so the MCP server has no HTTP listener
  to rebind against. The fix is rmcp 1.4, a major bump with breaking API
  changes; worth taking on its own merits, but not a live exposure.
- **RUSTSEC-2026-0141** (lettre TLS hostname verification with the Boring
  backend): not applicable. Trovato builds lettre with `default-features =
  false` and the rustls backend, so the vulnerable code is never compiled.
- **RUSTSEC-2023-0071** (rsa timing sidechannel): transitive through
  `sqlx-mysql`; Trovato uses PostgreSQL only.

## Completeness

### There is no plugin registry, and no package format

A plugin is a directory containing a compiled `.wasm`, an `.info.toml` manifest
and any migrations. `trovato plugin install <name>` takes a machine name and
reads it from the plugin search path. There is no archive format, no install
from a URL, and no index to discover plugins from. Distribution today means
telling someone where the directory is.

`PLUGINS_DIR`, `TEMPLATES_DIR` and `STATIC_DIR` accept several directories, so
an application can keep its plugins, templates and assets in its own repository
rather than inside a Trovato checkout. That is the mechanism a package format
would eventually build on. Each directory still has to be named in the search
path by hand; a plugin's own `static/` and `templates/` subdirectories are not
discovered automatically.

### The committed reference plugin is a binary artifact

`plugins/ritrovo_importer/ritrovo_importer.wasm` is checked in so the tutorial
works without a second repository. It is reproducible (the header of
`crates/kernel/tests/ritrovo_paired_consumer_test.rs` records the source commit,
the pinned SDK commit and the sha256, and two clean checkouts produced it byte
for byte), but a compiled binary in a source repository is still something to
resolve rather than keep.

### Some admin screens are configuration import only

Roles and permissions, stages, menus, and system configuration are managed by
editing YAML and running `trovato config import`. There is no form for them.
Content types, fields, users, categories, content, gather queries, tiles,
aliases, plugins and AI providers all do have admin screens.

Menus were listed here as having a screen, and do not: no route under `/admin`
matches `menu`, and `templates/admin/` holds no menu template. Menu links are
rows in `menu_link`, read by the render layer
(`crates/kernel/src/routes/helpers.rs`) and written only by config import. Since
1.0 means a site can be configured through the interface, the form belongs before
it — see [ROADMAP.md](ROADMAP.md).

Because import is the only path for those types, it now refuses to apply a set
containing a file it cannot parse: the run names every offending file, exits
non-zero, and writes nothing. It used to skip such a file with a warning and
report success, which for a role or a stage meant an entity that never arrived
with nothing that said why.

### Role permissions are not carried by config import

The `role` config entity carries a role's UUID and name. It does not carry the
role's permissions, so `config import` creates roles but cannot grant them
anything; assign permissions at `/admin/people/permissions` afterwards. The role
files in `docs/tutorial/config/` list their intended permissions in comments for
exactly this reason.

### Tiles and menu links ignore the stage a config file declares

`Tile` and `MenuLink` both carry a `stage_id`, and both config files must declare
one to parse, but the storage layer's insert does not bind it — the row takes the
column default, which is the Live stage. A tile or menu link cannot currently be
imported onto a non-Live stage. The same insert also drops a menu link's
`parent_id`, `hidden` and `plugin`, so menu hierarchy does not survive an import
round trip.

### Semantic search has no approximate index

Vector similarity is computed exactly, comparing against every candidate row.
This is correct and it is fine at small scale; it does not stay fine as the
corpus grows. There is no ivfflat or hnsw index on the embeddings table yet.

### Migrations only move forward

There is no down migration and no rollback. Recovering from a bad migration
means restoring the database. Plan accordingly before upgrading a production
site.

### Template reloading on file change is for development only

The filesystem watch that reloads templates is a development convenience. In
production, templates are read at startup and a change needs a restart.

## Contract and versioning

### The frozen plugin contract is enforced by policy, not by tooling

The plugin boundary is frozen and does not change through the 0.99 series. The
`SDK Semver Gate` CI job runs `cargo-semver-checks` against it, but under
SemVer's 0.x rules a breaking change is permitted by a minor bump, so the gate
cannot fail one. Until 1.0.0 the freeze is held by review. See
[docs/design/Versioning.md](docs/design/Versioning.md).

### An old pre-freeze manifest passes the version check

The compatibility rule is `major ==` and `minor <=`, so a manifest declaring an
early `api_version` such as `"0.2"` is accepted by a kernel at `(0, 99)`.
Nothing was ever released against the pre-freeze API, so no such plugin exists
outside this repository's own history, but the check is a compatibility gate and
not a provenance check, and it is worth knowing which of the two it is.

## Testing

### One notification test is timing-sensitive under coverage

`the_pipeline_turns_a_summarized_story_into_a_dispatched_notification` in
`crates/kernel/tests/argus_notify_test.rs` drives the real Argus WASM plugin and
asserts that the notification captured the story as it stood when it was
founded, with one member rather than two. It depends on a fixed 1200ms sleep
winning a race against a second report joining the story.

Under `cargo llvm-cov`, instrumentation slows execution enough that the race can
go the other way, and the CI Coverage job fails with `article_count` 2. It
passes on re-run and passes in the ordinary test job. Observed once on
2026-08-16. If Coverage fails on that assertion, re-run it; the fix is to make
the test wait on the state it needs instead of on a duration.

### The local test gate is stronger than CI

CI splits the integration tests across three shards with three separate
databases. A local `cargo test --all` runs every target against one database, so
it catches cross-file interference through shared fixtures that CI can miss. The
local run is the stronger gate; see CONTRIBUTING.md.
