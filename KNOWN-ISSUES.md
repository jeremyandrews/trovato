# Known issues

What is outstanding in 0.103.0. This is the list that would otherwise be a
surprise, so it is written down rather than discovered.

Trovato was developed privately and is published as a pre-1.0 release for
exactly the reasons on this page. Nothing here is a secret being managed; it is
a backlog being worked in the open. [ROADMAP.md](ROADMAP.md) says what happens
to each item, and [docs/BACKLOG.md](docs/BACKLOG.md) lists every finding, including
the smaller ones this page does not describe, with its verified status and whether a
Ritrovo row waits on it.

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
and the admin screens carry inline `style=` attributes, 305 of them across 44
templates. Extracting the styles allows the directive to be tightened, which is
also what makes the page-builder allowlist above worth having.

`script-src` does not carry `'unsafe-inline'`, but not because nothing needs it:
four templates still depend on inline `<script>` blocks (account recovery,
sessions, passkeys and the admin recovery settings) and five more on inline `on*=`
handlers, and the enforcing policy blocks every one of them. Those have to move to
`static/js/` as well. See BL-08 in [docs/BACKLOG.md](docs/BACKLOG.md).

### Dependency advisories are suppressed with justifications

`cargo audit` runs in CI and `.cargo/audit.toml` lists what is suppressed and
why. Each entry has reasoning; none is suppressed silently. The open ones:

Every wasmtime and cranelift advisory is **fixed rather than suppressed**:
RUSTSEC-2026-0085 through -0096, -0114 and -0222 all cleared by upgrading the
runtime to wasmtime 47.0.3, and RUSTSEC-2026-0268 and -0269 by 47.0.4, which is
what the runtime is on. Nothing about the plugin sandbox is being carried on a
justification.

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

`plugins/ritrovo_importer/ritrovo_importer.wasm` is a compiled binary in a source
repository, which is something to resolve rather than keep. It is now at least
**reproducible from public sources**, which it was not: the header of
`crates/kernel/tests/ritrovo_paired_consumer_test.rs` records the sha256 (asserted by
a test in that file), the Ritrovo commit it was built from, and the revision of *this*
repository that commit pins its SDK at. The recipe there was run from a fresh clone
and produced the artifact byte for byte.

Two corrections to what this entry used to say. It claimed the artifact "is checked in
so the tutorial works without a second repository", and that is not true in either
half: the directory holds the `.wasm` and **no `.info.toml` and no migrations**, so the
plugin loader skips it —

```
WARN trovato::plugin::runtime: no .info.toml file found, skipping
     dir=…/trovato/plugins/ritrovo_importer
```

— and `docs/tutorial/part-02-ritrovo-importer.md` has been stale since Ritrovo moved to
its own repository. It tells the reader to read the plugin's source (not here), to run
`cargo build -p ritrovo_importer` (not a workspace member), and that the manifest
declares `default_enabled = true` (there is no manifest here, and the real one says
false). Rewriting that tutorial part is its own piece of work.

What the artifact is actually for is the paired-consumer test: a real external plugin,
built from public sources against the published SDK, loading on this kernel and running
through the plugin-queue drain. That is worth having. Whether it is worth having as a
committed binary, rather than fetched from a Ritrovo release, is the open question.

### Languages are configuration import only, on purpose

A site's language set is part of its definition rather than something an operator
changes while running it: it is decided once, it belongs in the config set that a
deployment applies, and `language.{code}.yml` is that. So there is no admin screen
for it, and this is a decision rather than a gap.

The other half of the reasoning is that a language screen on its own would not help
much. Adding a language row is the small part of adding a language; the work is the
interface strings and the content translations. This page used to say that
`trovato_locale` handles the first at `/admin/config/locale` by importing `.po`
files. It does not, in either half. The plugin is 47 lines: it implements `tap_menu`
and `tap_perm` only, declares no host interfaces, and registers its two menu entries
with a `callback` and no `tap_api` behind it, so both paths are a startup warning and
a 404. And `LocaleService::import_translations`
(`crates/kernel/src/services/locale.rs:74`) has no caller anywhere in the tree: no
route, no CLI command, no config entity, so no `.po` file can be imported by any
means. `trovato_locale` preloads the default language and no other `load_language`
call exists (`crates/kernel/src/state.rs:645`), so a site's second language is served
entirely in the first language's strings.

That is the same shape as the content translation paragraph below, and it has the
same consequence: a form that adds a language row would look like the feature without
being it, because neither of the two things that make a language work can be done at
all. See BL-98 in [docs/BACKLOG.md](docs/BACKLOG.md).

Content translations used to be the weaker half of that sentence. The kernel read
`item_translation` everywhere a page is rendered and **nothing wrote to it**, so
outside the tests the only way to add a translation was SQL.

Two of the three write paths now exist. `/admin/content/{id}/translate/{lang}`
takes a POST that saves one, with the same `_token` protection as the rest of the
admin, and a delete route withdraws it; the form is built from the content type's
own text fields, since a boolean or a file reference is the same value in every
language. And `item_translation.<uuid>.<lang>.yml` is a config entity, placed
after `item` in the import order, so a config set ships an item and its
translations in one pass and a translated site exports and re-imports like any
other.

The third is a finding rather than a fix. There is no SDK-visible way for a
plugin to write a translation, because no WIT item write can carry a language:
`save-item` takes opaque item JSON and the host behind it builds `UpdateItem`,
which has no `language` field, and `CreateItem` with `language: None` hardcoded
— so a plugin cannot set even an item's own language, let alone a translation of
it. Adding one means changing a WIT signature. See BL-02 in
[docs/BACKLOG.md](docs/BACKLOG.md).

Interface strings are still the open half: see BL-98 above.

`crates/kernel/tests/config_admin_coverage_test.rs` holds this decision as a table:
every config entity type there either names a screen that must serve or names the
sentence above, and a new config entity type fails that test until somebody decides
which it is.

### What is configuration import only, in full

**Thirteen of the fourteen config entity types have an admin screen**, and the
fourteenth (`language`) is import-only by the decision above. That leaves the
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

### A plugin enabled while the server runs registers its permissions on restart

`tap_perm` is dispatched at boot and its result stored, so every plugin enabled
at startup has its permissions in the grid and nameable in a `role.*.yml` file.
Enabling a plugin from `/admin/plugins` also refreshes its declarations
immediately, but only if that plugin's module was already compiled: the runtime
loads the plugins that were enabled at boot and builds the tap registry from
that set once, so a plugin enabled afterwards has nothing to dispatch to until
the next restart. Its permissions then appear on the next start. This is the
same restart boundary `plugin enable` on the CLI already documents and the one
`tap_install` already waits for.

### A plugin's permissions cannot be granted by config import (fixed)

`tap_perm` was declared in the WIT and not dispatched, so the kernel had no list
of a plugin's permissions. Three things followed at once: the permission grid
could not show or grant one, `config import` refused to name one, and saving the
grid **revoked** any that SQL or a migration had inserted, because the save
rebuilt each role's whole set from the kernel's list and anything absent from
that list looked deliberately unchecked.

All three are fixed. The kernel dispatches `tap_perm` at boot and stores what it
gets in `plugin_permission`, which is a cache of declarations and never a grant.
The grid renders those beside the kernel's own with a column naming the plugin
that declared each. `config import` accepts a declared permission, and still
accepts one some role already holds, which covers a site whose plugin is
currently disabled. The save now replaces only the permissions the form actually
rendered, so a permission the grid did not show keeps whatever it had.

### Semantic search has no approximate index

Vector similarity is computed exactly, comparing against every candidate row.
This is correct and it is fine at small scale; it does not stay fine as the
corpus grows. There is no ivfflat or hnsw index on the embeddings table yet.

### Migrations only move forward

There is no down migration and no rollback. Recovering from a bad migration
means restoring the database. Plan accordingly before upgrading a production
site.

### Templates are read once, at startup

There is no filesystem watch. `ThemeEngine::reload` exists and nothing calls it, so
a template change needs a restart in development and production alike.

### The revision history page fails once an item has a revision

`ItemRevision` carries two fields, `change_summary` and `ai_generated`, beyond the
eight columns `get_revisions` and `get_revision` select. Both carry
`#[serde(default)]`, which reads as a default at a glance and does nothing for
`sqlx::FromRow`, and neither carries `#[sqlx(default)]`, so decoding a row fails on a
missing column (`crates/kernel/src/models/item.rs:96-105`, `:396-400`, `:409-413`).

An empty result decodes, so `/item/{id}/revisions` works until the first edit and
answers 500 afterwards, and revert fails the same way. It is a 1.0 blocker and it
needs the integration test that edits an item before loading its history, which is
why it survived. See BL-91 in [docs/BACKLOG.md](docs/BACKLOG.md).

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

### The two theme taps are declared and not dispatched

`tap_theme` and `tap_preprocess_item` are in the WIT and nothing dispatches them.
Each needs a decision first, and neither is what a plugin-served page uses to reach
the theme — that is the `theme` field on the `tap_api` response, added at 0.101 and
used by `plugins/trovato_contact`.

`tap_theme` is `() -> string` and nothing consumes what it would return. In Drupal
the equivalent registers templates, which needs template discovery and override
resolution the kernel does not have.

`tap_preprocess_item` would let a plugin alter an item's render context, and the
open question is which keys it may overwrite. `csrf_token`, `user_is_admin` and
`content` are in that context, so "anything" is wrong; `breadcrumbs` is exactly
what a plugin would legitimately want to change, so a blanket deny-list is wrong
too. `crates/kernel/tests/plugin_surfaces_test.rs` pins the current state, so this
entry cannot drift out of date without a test failing.

### A plugin's outgoing mail works only while serving a request

The `mail` host interface refuses to send anywhere except the site's own contact
address, so it cannot be used to reach strangers.

How *often* a plugin may send is now bounded on every path by the `mail`
rate-limit bucket, checked inside the host function and keyed by plugin: 100
messages an hour by default, configurable like every other bucket
(`TROVATO_RATE_LIMIT_MAIL`, or the `rate_limit.mail` site config key). Keyed by
plugin rather than by client because the mailbox being protected is the site's
own, which a plugin in a loop floods regardless of who set it going. It is checked
before the SMTP-handle test, so the answer does not depend on which path the call
arrived on.

What remains is the *delivery* half: background dispatch builds its services
without the email handle (`RequestServices::for_background` in
`crates/kernel/src/tap/request_state.rs`), so plugin mail from `tap_cron` or
`tap_queue_worker` still fails with `ERR_MAIL_NOT_CONFIGURED` and logs that the
site has no SMTP host, whether it has one or not. The limit that enabling
background mail would need is therefore already in place and tested
(`crates/kernel/tests/plugin_mail_rate_limit_test.rs`); enabling the delivery is
BL-72 in [docs/BACKLOG.md](docs/BACKLOG.md).

## Contract and versioning

### The frozen plugin contract is enforced by policy, not by tooling

The plugin boundary is frozen and does not change before 1.0. The `SDK Semver
Gate` CI job runs `cargo-semver-checks` against it, but under SemVer's 0.x rules
a breaking change is permitted by a minor bump, so the gate cannot fail one.
Until 1.0.0 the freeze is held by review. See
[docs/design/Versioning.md](docs/design/Versioning.md).

### An old pre-freeze manifest passes the version check

The compatibility rule is `major ==` and `minor <=`, so a manifest declaring an
early `api_version` such as `"0.2"` is accepted by a kernel at `(0, 103)`.
Nothing was ever released against the pre-freeze API, so no such plugin exists
outside this repository's own history, but the check is a compatibility gate and
not a provenance check, and it is worth knowing which of the two it is.

## Testing

### The local test gate is stronger than CI

CI splits the integration tests across three shards with three separate
databases. A local `cargo test --all` runs every target against one database, so
it catches cross-file interference through shared fixtures that CI can miss. The
local run is the stronger gate; see CONTRIBUTING.md.
