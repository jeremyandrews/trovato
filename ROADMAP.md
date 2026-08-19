# Roadmap

Trovato is at 0.100.0. This page says what stands between that and 1.0, and what
comes after. [KNOWN-ISSUES.md](KNOWN-ISSUES.md) describes each item in more
detail; this one is about order and intent.

## The road to 1.0

1.0 means the CMS is finished to the standard the plugin contract already meets:
a site can be built, configured and operated through the interface, and the
security work has been reviewed by someone other than the person who wrote it.

### Security review, in public

The security findings from private development were addressed but not
independently verified. Re-reviewing them against a public codebase is the first
piece of work, and it is the one that most benefits from being public. Two
concrete items are already scoped: restricting the page-builder CSS allowlist
(`crates/kernel/src/content/page_builder.rs:91`) and extracting inline styles so
`style-src 'unsafe-inline'` can come out of the CSP. Those two are related and
land together.

### Dependency advisories

The wasmtime and cranelift batch is **done**: the runtime is on 47.0.3 and no
advisory against it remains. What is left is smaller. The quick-xml pair is
blocked on an upstream `plist` release and is tracked rather than worked. The
rmcp DNS-rebinding advisory does not apply to the STDIO transport Trovato uses,
but taking rmcp 1.4 is worthwhile independently, and it is a breaking API change
to `trovato-mcp`.

### The remaining admin screens

**This block is done, and the sentence above is now checkable line by line.**

Twelve of the thirteen config entity types have an admin screen. The thirteenth,
`language`, is import-only by a decision recorded in KNOWN-ISSUES.md rather than by
omission: a site's language set belongs in its config set, and a form that adds a
language row would not do the interface strings or the content translations that
actually make a language work.

The `variable` type is a key/value store rather than one thing, and the settings that
warrant a screen have one. There is deliberately no generic variable editor.

`crates/kernel/tests/config_admin_coverage_test.rs` is the audit: every type either
names a path that must serve for an administrator and must not serve for a visitor,
or names the sentence that records the decision. Adding a config entity type fails
that test until somebody chooses which it is, so this cannot drift back — which it
did before, with menus listed as having a screen they did not have.

**Stages are done.** `/admin/structure/stages` creates and edits stages: machine
name, label, description, visibility, default and weight, which is what the schema
models. There is deliberately no workflow-membership field, because there is
nothing to edit: the tutorial ships a `workflow.editorial` variable describing
transitions and no kernel code reads it.

**Menus are done.** `/admin/structure/menus` lists a site's menus, renders each as
an indented tree, and creates, edits, reorders and deletes links, with cycle
rejection and a stated answer for what happens to a deleted link's children. It
needed no kernel plumbing: the render layer reads `menu_link` per request, so an
edit shows on the next page load without a restart. Plugin-registered navigation
is listed read-only beside it, because it is not rows.

### Self-service account management

**Done.** A person can delete their own account (`/user/delete`) and download their
own data (`/user/data-export`). Both were absent, which for a site operated from the
EU with open registration made GDPR articles 15 and 17 unanswerable, and which Drupal
6 had in core. Deleting an account reattributes its content to the anonymous author
rather than destroying it; KNOWN-ISSUES.md records the one invariant that had to be
narrowed to make it possible at all.

### A contact form, and the three plugin surfaces it needed

**Done.** A visitor can reach the site owner: `plugins/trovato_contact` serves
`/contact`, and it is a plugin because kernel minimality puts a feature in one.

It could not be written before, and the three reasons were all missing kernel
seams rather than plugin problems. A plugin could not send email, so the `mail`
host interface was added — narrow by construction, sending only to the site's own
configured address, because a host function that takes a recipient from its caller
is a spam relay. A plugin could not serve a form that worked without JavaScript, so
plugin-served posts now accept a `_token` field alongside the `X-CSRF-Token` header
and the kernel hands the plugin a token to embed; a form also posts back to its own
URL, which the menu registry could not represent until it stopped being keyed by
path alone. And a plugin could not render into the site theme, so a response can
now ask to be wrapped in the site's page template.

Two narrower things stayed undone and are in KNOWN-ISSUES.md rather than pretended
away: `tap_theme` and `tap_preprocess_item` are still declared and not dispatched,
each for a stated reason, and a plugin's outgoing mail is rate-limited only on the
web-facing path.

### Test isolation

A few tests use fixed usernames and assert exact row counts without cleaning up,
so they pass on a fresh database and fail on the second run against the same
one. Giving them unique fixtures, or teardown, is small self-contained work and
a good first contribution.

### The committed plugin binary

`plugins/ritrovo_importer/ritrovo_importer.wasm` is a compiled artifact checked into a
source repository. It is now reproducible from public sources — Ritrovo builds against
this repository's SDK, and the test header records the commit, the SDK revision and the
sha256, verified from a fresh clone — so what remains is the binary itself, not its
provenance. The right answer is either a build-from-source step in the tutorial or
publishing the reference application so the artifact can be fetched rather than
committed. Before 1.0, one of those.

Two smaller things KNOWN-ISSUES.md now records alongside it: the committed directory has
no manifest, so the loader skips it, and `docs/tutorial/part-02-ritrovo-importer.md` has
been stale since Ritrovo moved out — it tells the reader to build a package that is not
a workspace member and to read source that is not here. That tutorial part needs a pass.

## After 1.0

These are not 1.0 blockers. A site can be built and run without them.

### Plugin distribution

A package format and somewhere to publish to. Today a plugin is a directory and
distribution is out of band. The search-path support added in 0.99.0 is the
groundwork: an application already lives in its own repository and contributes
plugins and templates from there. A package format formalises what that
directory looks like; a registry makes it findable.

### An approximate vector index

Exact cosine comparison is correct and it is fine on a small corpus. An ivfflat
or hnsw index on the embeddings table is what makes semantic search hold up as
content grows, and the point at which it matters is the point to do it.

### Migration rollback

Migrations only move forward. Down migrations, or a supported way to recover
without restoring a backup.

### Production template reloading

Template reloading on filesystem change is a development convenience. Making it
safe in production is a small piece of work that nobody has needed yet.

## How the version number moves

Everything in Trovato carries one version and moves in lock-step, so 1.0.0 is
also plugin API `(1, 0)` and an SDK 1.0.0. At that point the freeze stops being
policy and becomes something `cargo-semver-checks` can enforce, because SemVer's
rules for 1.x require a major bump for a breaking change. See
[docs/design/Versioning.md](docs/design/Versioning.md).
