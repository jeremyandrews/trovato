# Roadmap

Trovato is at 0.99.0. This page says what stands between that and 1.0, and what
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

Stages and system configuration are configuration-import only. Each needs a form.
This is the largest block of ordinary work before 1.0 and the most approachable
for a contributor who wants somewhere to start.

**Menus are done.** `/admin/structure/menus` lists a site's menus, renders each as
an indented tree, and creates, edits, reorders and deletes links, with cycle
rejection and a stated answer for what happens to a deleted link's children. It
needed no kernel plumbing: the render layer reads `menu_link` per request, so an
edit shows on the next page load without a restart. Plugin-registered navigation
is listed read-only beside it, because it is not rows.

### Test isolation

A few tests use fixed usernames and assert exact row counts without cleaning up,
so they pass on a fresh database and fail on the second run against the same
one. Giving them unique fixtures, or teardown, is small self-contained work and
a good first contribution.

### The committed plugin binary

`plugins/ritrovo_importer/ritrovo_importer.wasm` is a compiled artifact checked
into a source repository, and it cannot currently be rebuilt from published
sources: the SDK revision it was compiled against is in no public repository. See
KNOWN-ISSUES.md, which carries the detail. The right answer is either a
build-from-source step in the tutorial or publishing the reference application so
the artifact can be fetched rather than committed. Before 1.0, one of those.

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
