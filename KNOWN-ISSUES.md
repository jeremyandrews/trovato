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
works without a second repository. A compiled binary in a source repository is
something to resolve rather than keep, and this one cannot currently be rebuilt
from published sources at all.

Its provenance is recorded in the header of
`crates/kernel/tests/ritrovo_paired_consumer_test.rs`, which since the version
sweep claims only what a reader can check: the sha256 (asserted by a test in
that file, so the artifact cannot be swapped without the header going stale) and
the Ritrovo commit it was compiled from, in the public
[ritrovo](https://github.com/jeremyandrews/ritrovo) repository. What is missing
is the SDK revision: at that Ritrovo commit the `trovato-sdk` dependency was
pinned at a commit of the unpublished development repository, so there is no
public revision to name and no rebuild recipe that would work. The header used
to assert a byte-for-byte reproducible build and cite that commit as if a reader
could resolve it; they cannot. Re-pointing Ritrovo's SDK dependency at this
repository and refreshing the artifact from that build is what makes the
reproducibility claim true, and is the next step here.

### Languages are configuration import only, on purpose

A site's language set is part of its definition rather than something an operator
changes while running it: it is decided once, it belongs in the config set that a
deployment applies, and `language.{code}.yml` is that. So there is no admin screen
for it, and this is a decision rather than a gap.

The other half of the reasoning is that a language screen on its own would not help
much. Adding a language row is the small part of adding a language; the work is the
interface strings, which `trovato_locale` handles at `/admin/config/locale` by
importing `.po` files, and the content translations, which
`trovato_content_translation` handles per item. A form that adds a row and leaves an
operator to do both of those anyway would look like the feature without being it.

`crates/kernel/tests/config_admin_coverage_test.rs` holds this decision as a table:
every config entity type there either names a screen that must serve or names the
sentence above, and a new config entity type fails that test until somebody decides
which it is.

### What is configuration import only, in full

**Twelve of the thirteen config entity types have an admin screen**, and the
thirteenth (`language`) is import-only by the decision above. That leaves the
`variable` type, which is a key/value store rather than one thing, and so is
partly covered:

| Setting | Screen |
|---|---|
| `site_name`, `site_slogan`, `site_mail`, `front_page`, `items_per_page`, registration mode, the SMTP settings, `notify_admin_on_register`, `update_check` | `/admin/config/site` |
| `pathauto_patterns` | `/admin/config/pathauto` |
| `robots_txt_custom` | **none** |
| Anything a plugin defines | **none** |

There is deliberately **no generic variable editor**, and there should not be one.
A form that writes arbitrary JSON into arbitrary `site_config` keys is a form that
can break a site in ways the kernel parses at startup, with no validation possible
because the schema is per key. What a specific variable needs is a specific field on
a specific screen, which is how the covered ones got there.

This list is no longer only prose. `crates/kernel/tests/config_admin_coverage_test.rs`
holds the audit as a table: every config entity type either names an admin path that
must serve for an administrator, and must not serve for an anonymous visitor, or
names the sentence in `KNOWN-ISSUES.md` that records it as a deliberate decision. A
new config entity type fails that test until somebody chooses.

The prose version of this list drifted before, which is why: menus were listed among
the types *with* screens for a while and did not have one.

Because import is the only path for what remains, it refuses to apply a set
containing a file it cannot parse: the run names every offending file, exits
non-zero, and writes nothing. It used to skip such a file with a warning and report
success, which meant an entity that never arrived with nothing that said why.

### A plugin's permissions cannot be granted by config import

A role config file declares a `permissions` list and `config import` grants
exactly that set, so a role now arrives able to do something. What it cannot
declare is a permission belonging to a plugin.

A plugin declares its permissions through `tap_perm`, which is declared in the WIT
and **not dispatched** by the kernel (`crates/wit/kernel.wit` says so). So the
kernel has no list of a plugin's permissions to validate against, and it refuses a
permission string it has no evidence exists rather than granting one that matches
nothing a permission check will ever ask for. The evidence it does accept is a
permission some role in the database already holds, which is what lets an export
of a site that uses plugin permissions re-import.

The practical consequence: grant a plugin's permissions once at
`/admin/people/permissions` (or by SQL), after which they can go in the config file
like any other. The permission grid has the same limitation from the other side —
it renders the kernel's list, so a plugin's permissions do not appear there either.
Dispatching `tap_perm` is what fixes both, and it is additive to the plugin
contract rather than a break of it.

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

### A revision's authorship can change, and nothing else about it can

`item_revision` rows are immutable by database trigger: a revision is a snapshot,
and a snapshot that can be edited is not a history. That invariant is now narrowed
by one case. When an account is deleted, its revisions' `author_id` is set to the
anonymous author; every other column must be byte-identical or the trigger still
refuses.

The narrowing is not a convenience. `item_revision.author_id` is
`NOT NULL REFERENCES users(id)` with no `ON DELETE` action, so the trigger and the
foreign key together made an account that had ever saved an item **undeletable** —
which is what self-service account deletion ran into, and which the admin delete
screen had been quietly failing on all along. The alternatives were deleting other
people's content history or refusing erasure to anyone who ever wrote anything.

The enforcement compares whole rows (`to_jsonb(NEW) - 'author_id'` against the same
of `OLD`) rather than listing columns, so a column added to that table in future is
covered rather than silently exempted. See
`crates/kernel/migrations/20260819000001_allow_revision_author_anonymization.sql`,
which carries the reasoning.

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
