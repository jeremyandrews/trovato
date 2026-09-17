# Kernel backlog

Every kernel finding recorded anywhere, folded into one list, checked against the
code, and classified for 1.0. [KNOWN-ISSUES.md](../KNOWN-ISSUES.md) explains the
larger items in prose and [ROADMAP.md](../ROADMAP.md) says what order they happen
in; this page is the complete ledger both of them draw from.

It exists because three projects built on Trovato each kept their own list of what
the kernel does wrong or cannot do, and none of those lists reached this
repository. Until now "what is still wrong with Trovato" could not be answered from
Trovato.

**Verified at `d3f4cd7`** (v0.102.0 plus two commits), plugin API `(0, 102)`, on
2026-09-17. Every status below cites a file and line or a commit at that revision.
A line number is where the thing is at `d3f4cd7`; the source documents' own line
numbers are often stale and are not repeated.

In short: 111 distinct findings, 94 of them open, and 30 proposed as blocking the
1.0 tag, plus 26 marked as a Ritrovo gate by the ruling of 2026-09-17. The [Tally](#tally) lists them and [Why these block 1.0](#why-these-block-10)
argues each.

## Sources

| Source | What it is | Written against |
|---|---|---|
| trovato.rs site | `docs/REPORT.md` "What was found" in [trovato-site](https://github.com/jeremyandrews/trovato-site), 22 items plus 3 site-fixable notes | v0.101.0 |
| Netgrasp | `plugins/netgrasp/FRICTION.md` in [netgrasp-trovato](https://github.com/jeremyandrews/netgrasp-trovato), 15 findings plus 9 residual bullets | text from 0.99, two findings from 0.102 |
| Argus | `plugins/argus/M1-FRICTION.md` through `M4-FRICTION.md` in this tree | 0.99 |
| Ritrovo | `FRICTION.md` and `docs/ritrovo/STATUS.md` in [ritrovo](https://github.com/jeremyandrews/ritrovo), 34 findings and 48 blocked rows | v0.102.0 |
| This tree's plans | the open items in `ROADMAP.md` and `KNOWN-ISSUES.md` | 0.102.0 |
| AI assistant work | four candidates reported during the 0.102 assistant implementation and never written down | 0.102.0 |

Where two sources describe one defect the row keeps every identifier. A source
identifier is how to find the original write-up; the `BL-` number is how the fixes
refer back here.

## How to read a row

**Status** is `open`, `fixed` with the commit, `partly fixed` (the row says which
half), or `cannot reproduce` (the row says what was looked for). Public history
starts at the 0.99.0 squash `2ff3a62`, so anything fixed during private development
shows as fixed at `2ff3a62`.

**Class** is the proposal for when it is fixed:

- **1.0**: blocks the 1.0 tag. The test: would a competent stranger deploying a
  public site on the released image hit it on day one, or does it contradict the
  1.0 definition in ROADMAP.md (a site can be built, configured and operated
  through the interface, and the security work has been reviewed by someone other
  than its author)? Each of these is argued in [Why these block 1.0](#why-these-block-10).
- **1.0.x**: should ship in a 1.0 patch or minor release. Real, but a stranger
  building a public site does not meet it on day one.
- **post**: after 1.0. A capability rather than a defect, or a defect only a
  plugin author with an unusual need meets.
- **closed**: fixed, or decided, and listed so the source documents stop being
  read as open.

**Ritrovo gate** is a fourth mark, and it stacks on top of the class rather than
replacing it. It means a row in Ritrovo's `docs/ritrovo/STATUS.md` with the status
"blocked on the kernel" waits on this finding, which by Jeremy's ruling of
2026-09-17 makes it a 1.0 blocker whatever the day-one test says. A finding can be
classed post and still be a gate; the mark wins, and the row says so, so that what
the ruling costs is visible and individual rows can be waived later by the person
who made it. Nothing is waived here. The 26 marked findings are listed in
[Ritrovo unblock order](#ritrovo-unblock-order), and the rows they unblock in
[Every blocked Ritrovo row, mapped](#every-blocked-ritrovo-row-mapped).

**Surface** says what a fix touches:

- **additive**: can land in any minor release without breaking the frozen plugin
  contract. Kernel routes, templates, middleware and config import are not the
  plugin contract; a new SDK binding for an existing WIT function is additive.
- **frozen**: the obvious fix changes the plugin contract as
  [docs/design/Versioning.md](design/Versioning.md) defines it: the WIT, the
  `trovato-sdk` crate, manifest semantics, the error vocabularies, or the
  observable behaviour of an existing host function or tap. Where an opt-in
  variant would make it additive, the row says so. See
  [Frozen-surface findings](#frozen-surface-findings).

## From the trovato.rs site

| ID | Finding | Where | Status | Class | Surface |
|---|---|---|---|---|---|
| BL-01 (SITE-1) | Every GET that is not a special category, static assets and HTML pages included, draws on one `api` bucket of 100 requests a minute per client. `/static` is not exempt and no environment variable or setting changes the limits. Behind a proxy with `TRUSTED_PROXIES` unset, every visitor shares the proxy's bucket. | `crates/kernel/src/middleware/rate_limit.rs:99-117` (defaults), `:251-273` (`categorize_path` falls through to `api`); `crates/kernel/src/state.rs:789-793` (`RateLimitConfig::default()`) | open | 1.0 | additive |
| BL-02 (SITE-2) | Nothing can write a content translation. The kernel reads and overlays `item_translation`, but no route, API, config import path, host function or shipped plugin inserts into it; outside tests the only writer is SQL. `trovato_content_translation` supplies the table and two menu entries. | `crates/kernel/src/content/item_service.rs:474,490,506,536` (reads only); `crates/kernel/src/routes/admin_translation.rs:20-27` (GET only); `plugins/trovato_content_translation/src/lib.rs` | open | 1.0, **Ritrovo gate** | additive |
| BL-03 (SITE-3, G-VARIABLES-DOUBLE-NAMESPACE) | `variables-get` and `variables-set` prefix every key with `plugin.{name}.` and a missing key returns the caller's default without a log line. Neither the WIT nor the SDK mentions the namespace, so a plugin cannot read a site-wide variable and an operator cannot see which key a plugin variable lives under. | `crates/kernel/src/host/variables.rs:46-48,58,106-107`; `crates/wit/kernel.wit:42-46`; `crates/plugin-sdk/src/host.rs:720-729` | open | 1.0.x | additive for documentation or a key helper; frozen if `get` may read outside the namespace |
| BL-04 (SITE-4) | `<html lang>` and `text_direction` were overwritten with the site default after the route set them. | `crates/kernel/src/routes/helpers.rs:211-229` | fixed `406ec63` | closed | n/a |
| BL-05 (SITE-5) | The front page never applied the translation overlay. The configured front-page item now does. The promoted-items fallback listing still takes no language (`crates/kernel/src/routes/front.rs:63,298`). | `crates/kernel/src/routes/front.rs:201-222` | fixed `406ec63`, switcher `4ff80e3` | closed | n/a |
| BL-06 (SITE-6, SITE-18 residual) | The kernel emits relative URLs where the protocols require absolute ones: sitemap `<loc>`, the `Sitemap:` line in robots.txt, and `hreflang` alternates. The sitemap lists items only. A plugin `api` route on a path a kernel route already serves makes axum panic at startup instead of being refused. | `crates/kernel/src/routes/sitemap.rs:64-73,141`; `crates/kernel/src/routes/helpers.rs:1017-1035`; `crates/kernel/src/routes/plugin_api.rs:144-180` (duplicates checked among plugins only) | open | 1.0 | additive |
| BL-07 (SITE-7) | The login page's "Forgot password?" links to `/user/password-reset`, which is registered POST only and takes JSON, so it is a 405. The link in the reset email returns JSON as well. No part of the email reset flow is an HTML page; the working reset is `trovato user reset-password` on a shell. | `templates/user/login.html:39`; `crates/kernel/src/routes/password_reset.rs:33,104-121,173` | open | 1.0 | additive |
| BL-08 (SITE-8) | The enforcing CSP has no `'unsafe-inline'`, nonce or hash for scripts, and five stock templates depend on inline `<script>` blocks: login (the passkey button never appears), account recovery, sessions, passkeys and admin recovery settings. Five more carry inline `on*=` handlers, two of them the `confirm()` guard on a delete, so those deletes happen without confirmation. The only workaround is `CSP_REPORT_ONLY=true`. | `crates/kernel/src/middleware/security_headers.rs:34-42`; `templates/user/login.html:61`, `recover.html:53`, `sessions.html:84`, `passkeys.html:109`, `templates/admin/recovery.html:53`; handlers in `templates/admin/field-list.html:43`, `admin/ai-providers.html:62`, `admin/tile-form.html:41`, `form/fieldset.html:6`, `elements/comments.html:60` | open | 1.0 | additive |
| BL-09 (SITE-9) | `mime_from_path` knows 14 extensions and serves everything else as `application/octet-stream` under `nosniff`: `.md`, `.txt`, `.xml`, and also `.webp`, `.avif`, `.pdf`, `.webmanifest` and `.otf`. | `crates/kernel/src/routes/static_files.rs:188-205` | open | 1.0 | additive |
| BL-10 (SITE-10) | The `item` config entity cannot set `promote` or `sticky` (inserted as 0), and re-import never updates `created`. The report also said `changed` is not updated; it is. | `crates/kernel/src/config_storage/mod.rs:53-89`; `crates/kernel/src/config_storage/direct.rs:726-744` | open | 1.0.x | additive |
| BL-11 (SITE-11) | A config file that omits `created` fails to parse, and because import validates the whole set first, nothing is written. The report said only roles; tags, menu links, URL aliases, tiles and stages require it too, only items default it. A role's `created` is an RFC 3339 string while every other entity's is a Unix integer. | `crates/kernel/src/models/role.rs:83-88`; `crates/kernel/src/config_storage/yaml.rs:559-570`; `crates/kernel/src/models/category.rs:57`, `menu_link.rs:45`, `url_alias.rs:31`, `tile.rs:25`, `stage.rs:114` | open | 1.0 | additive (a default); changes the documented config format if the timestamp types are unified |
| BL-12 (SITE-12) | `trovato_blog` declares `tap_item_view` in its manifest and does not export it. The blog is enabled by default, so every item view of any type instantiates the blog module, finds no export, and logs an ERROR. | `plugins/trovato_blog/trovato_blog.info.toml:10`; `crates/kernel/src/tap/dispatcher.rs:99-104,286-296` | open | 1.0 | additive |
| BL-13 (SITE-13) | A menu entry with a `callback` and the default `handler_type` logs a warning at startup. 16 in-tree plugins declare 22 such entries; a default install logs 20 from 15 plugins. The report's "seventeen" matches no count. | `crates/kernel/src/routes/plugin_api.rs:109-131`; `.callback(` in `plugins/*/src` | open | 1.0.x | additive (fix the plugins); frozen if the SDK's `MenuDefinition` default changes |
| BL-14 (SITE-14) | "`elements/comments.html` is not in the image." It is: the file exists since `2ff3a62` and the Dockerfile copies `templates/` whole (`Dockerfile:70`). The logged "not found" is what the empty fallback engine says, and the site hit that state in the same gate (see BL-85). | `templates/elements/comments.html`; `crates/kernel/src/state.rs:657-663` | cannot reproduce | closed | n/a |
| BL-15 (SITE-15) | The two content translation admin routes render `admin/content-translate-list.html` and `admin/content-translate-edit.html`, which have never existed in git history. `trovato_content_translation` is enabled by default, so both routes are mounted and return a 500 to a user allowed to translate. | `crates/kernel/src/routes/admin_translation.rs:64,103`; `crates/kernel/src/routes/mod.rs:144-148` | open | 1.0, **Ritrovo gate** | additive |
| BL-16 (SITE-16) | A themed plugin response's `title` reaches `<title>` and no heading, so every themed plugin page ships without an `<h1>`. The SDK documents the field as "the `<title>` and the page heading", so this breaks a written promise. | `templates/page.html:71-107`; `crates/plugin-sdk/src/types.rs:913-917`; `crates/kernel/src/theme/engine.rs:826-848` | open | 1.0.x | additive |
| BL-17 (SITE-17) | `track_request_timing` (a `Server-Timing` header and slow-request logging) is applied to no router, so `QUERY_SLOW_THRESHOLD_MS` does nothing, and the module advertises a `query-profiler` feature `Cargo.toml` does not define. | `crates/kernel/src/middleware/query_profiler.rs:50-74`; `crates/kernel/Cargo.toml:72-73` | open | post | additive |
| BL-18 (SITE-18) | `build_hreflang_links` was reachable only from its tests. It is now called from the item and front routes. The relative hrefs it produces are part of BL-06. | `crates/kernel/src/routes/item.rs:760-767`; `crates/kernel/src/routes/front.rs:265-272` | fixed `22b1d74`, front page `4ff80e3` | closed | n/a |
| BL-19 (SITE-19, G-SDK-NO-ITEM, G-ITEM-API-NO-DELETE-BINDING, AI-2) | `item-api` declares four functions (`get-item`, `save-item`, `delete-item`, `query-items`) and the SDK binds none of them. Argus and Netgrasp hand-roll the same FFI. The SDK's error documentation lists -1 to -3 while the host also returns -10, -12, -13 and -14. The report said two functions and no users; both are wrong. | `crates/wit/kernel.wit:7-19`; `crates/plugin-sdk/src/host.rs:27-162`; `crates/plugin-sdk/src/host_errors.rs:58-75`; `plugins/argus/src/item_host.rs` | open | 1.0.x | additive |
| BL-20 (SITE-20) | The Tera `markdown` filter cleans with ammonia's defaults, which strip `class` from `<code>`, so a fenced block loses its language hint and cannot be highlighted. | `crates/kernel/src/theme/engine.rs:430-457` | open | 1.0.x | additive |
| BL-21 (SITE-21) | Body field naming is split. The kernel's `page` type has `body` and `trovato_blog` has `field_body`; the stock blog listing template and the promoted-items renderer read `body`, while page metadata, feeds, search snippets, pagefind and `trovato_seo` read `field_body`. So blog teasers render with no text, and `page` items get no meta description, Open Graph description or feed description. | `crates/kernel/migrations/20260212000004_create_item_types.sql:37`; `plugins/trovato_blog/src/lib.rs:19`; `templates/gather/query--blog_listing.html:21-27`; `crates/kernel/src/routes/front.rs:329-340`; `crates/kernel/src/content/page_meta.rs:126-128`; `crates/kernel/src/routes/feed.rs:294` | open | 1.0 | additive (read both names); a data change if a field is renamed |
| BL-22 (SITE-22) | Gather cannot link to a friendly URL: relationships join on column equality, includes match plain fields, no Tera filter resolves an alias, and the `RelationshipHandler` extension point is never consulted. Every stock listing links to `/item/{uuid}`, and that address serves 200 rather than redirecting to the alias. | `crates/kernel/src/gather/query_builder.rs:463-484`; `crates/kernel/src/gather/extension.rs:254-259,380-386`; `templates/gather/row.html:4`; `templates/gather/query--blog_listing.html:15,30` | open | 1.0 | additive |
| BL-23 (site-fixable 1) | `trovato_contact` renders validation errors as a plain list above the form, with no `role="alert"`, `aria-invalid` or `aria-describedby`, and its errors carry no field key to associate. The report called this site-fixable; a site can only wrap the plugin's markup, so it belongs here. | `plugins/trovato_contact/src/lib.rs:107-131,229-253` | open | 1.0.x | additive |
| BL-24 (site-fixable 2) | The `filtered_html` format allows no `span` and no `class` on `code` or `pre`, so highlighted code is flattened to text. | `crates/kernel/src/content/filter.rs:136-179,205-210` | open | 1.0.x | additive |

The report's third site-fixable note (no `hreflang` alternates) is BL-18 and BL-06:
on 0.102.0 the theme has the language, `requested_path` and
`available_translations`.

## From Netgrasp

Netgrasp's `FRICTION.md` says it was verified at `KERNEL_API_VERSION (1,0)`, which
no public revision has ever been. Apart from that header and the two findings added
for 0.102, its text is the copy that lived in this tree at `2ff3a62`, which is why
five of its "residual" findings describe defects 0.99.0 had already fixed. Each
such row says so.

| ID | Finding | Where | Status | Class | Surface |
|---|---|---|---|---|---|
| BL-25 (G-SAVE-ITEM-BYPASSES-SERVICE, AI-1) | `save-item` and `delete-item` call the `Item` model directly, so a plugin's item write fires no `tap_item_presave`, `tap_item_insert`, `tap_item_update`, `tap_item_update_index` or `tap_item_delete`, and runs no access check. `delete-item` returns 0 for an item that does not exist. The embedding half of the finding is fixed: a plugin save enqueues an embed job. This is deliberate (the module doc cites re-entrancy) and undocumented at the WIT. | `crates/kernel/src/host/item.rs:1-6,186,217,222,281,283`; `crates/kernel/src/content/item_service.rs:368,399,655,832` | partly fixed (embedding at `2ff3a62`) | post, **Ritrovo gate** | frozen; an opt-in save through the service, and a WIT note now, are additive |
| BL-26 (G-SAVE-ITEM-BYPASSES-SERVICE) | The same direct path skips `ItemService`'s cache invalidation and file-reference sync, so after a plugin updates or deletes an item a page can serve the old or deleted item for up to `CACHE_TTL_ITEMS` (300 s by default). | `crates/kernel/src/host/item.rs:186,217,281`; `crates/kernel/src/content/item_service.rs:409,416-431,665,668,842`; `crates/kernel/src/config.rs:50` | open | 1.0.x | additive |
| BL-27 (G-EMBED-OPTOUT-IS-NOT-AN-OPTOUT) | `EmbedPolicy` chooses synchronous or asynchronous embedding and has no "never" state, and a manifest cannot declare a policy for its types. The plugin save path also ignores the policy and does not check a provider is configured. Netgrasp's "the cost is moot today" rests on the embedding claim that is no longer true: plugin-written items are embedded now. | `crates/kernel/src/services/embed_index.rs:62-94`; `crates/kernel/src/content/item_service.rs:724,734-751`; `crates/kernel/src/host/item.rs:42-45` | open | post | additive |
| BL-28 (G-TWO-WRITER-NO-CONTRACT) | The database allowlist is per table, so no manifest can say a column belongs to another writer, and `raw_sql` reaches any table the database role can. | `crates/kernel/src/plugin/db_policy.rs:177`; `crates/kernel/src/plugin/info_parser.rs:183,190-196` | open | post | additive |
| BL-29 (G-NO-RECORD-WRITE-SURFACE, Argus record admin residual) | Record-type admin is list and view only. A plugin can now serve its own edit form through `tap_api`, which Netgrasp's text predates. | `crates/kernel/src/routes/admin_record_type.rs:196-200` | open | post | additive |
| BL-30 (G-NO-GATHER-AGGREGATION, Argus M3) | `QueryDefinition` has no grouping or aggregate projection and no tile type computes a number, so a count is a pager total over a list. | `crates/kernel/src/gather/types.rs:15-60`; `crates/kernel/src/services/tile.rs:79-120` | open | post | additive |
| BL-31 (G-EXPOSED-FILTER-NO-MATCH-ALL, Argus M3) | A blank exposed filter produced `= ''` and a 500 over a uuid column. Unanswered exposed filters are now dropped for every operator except the null checks. Netgrasp still lists it as residual. | `crates/kernel/src/gather/gather_service.rs:1086-1137` | fixed `2ff3a62` | closed | n/a |
| BL-32 (G-DISPLAY-CONFIG-CANNOT-STYLE-A-ROW) | A gather's display configuration has no per-row conditional class. | `crates/kernel/src/gather/types.rs:464-508` | open | post | additive |
| BL-33 (G-USER-API-NO-ADMIN-BYPASS) | `current-user-has-permission` is a literal membership test, while the assistant's gate lets an administrator through, so an administrator can open a plugin's conversation and have every tool refuse them. The kernel has two notions of administrator: `require_admin` reads the `users.is_admin` column and `UserContext::is_admin` reads the `administer site` permission string. | `crates/kernel/src/host/user.rs:49`; `crates/kernel/src/routes/assistant.rs:169-176`; `crates/kernel/src/tap/request_state.rs:100-107`; `crates/kernel/src/routes/helpers.rs:86,116` | open | 1.0.x | frozen; a second "effective permission" function and a WIT note are additive |
| BL-34 (G-SDK-NO-ESCAPE, Argus M3) | The SDK exports no HTML escaping helper, so plugins each write one (Argus, Netgrasp, `trovato_contact`, `trovato_book`, `trovato_seo`, `test_plugin_api`), and the SDK's own doc example calls an `escape_html` it does not provide. | `crates/plugin-sdk/src/types.rs:824`; `crates/kernel/src/routes/helpers.rs:903` (the kernel's private copy) | open | 1.0.x | additive |
| BL-35 (G-VIEW-OUTPUT-JSON-ENCODED, Argus M3, AI-4) | `tap_item_view` HTML reached the page as a JSON string literal. The macro still serializes a `String` return, and the kernel now decodes it before appending. Netgrasp's pinning test inspects raw dispatcher output, so it cannot see the fix. | `crates/kernel/src/content/item_service.rs:56-71,586-590`; `crates/plugin-sdk-macros/src/lib.rs:158` | fixed `2ff3a62` | closed | n/a |
| BL-36 (G-DB-HOST-TYPE-COVERAGE) | The `db` host decodes nine Postgres types and falls through to `try_get::<String>().ok()`, so `timestamptz`, `numeric`, `date`, `inet`, `bytea` and arrays arrive as `null`, indistinguishable from a real null. The gather path renders the same column as an ISO string. Netgrasp's pinning test exercises a copy of the function, not the kernel's. | `crates/kernel/src/host/db.rs:104-161` (fall-through `:151-156`); `crates/kernel/src/gather/gather_service.rs:834,858` | open | 1.0.x | frozen; an opt-in typed decode is additive |
| BL-37 (G-RECORD-ID-MUST-BE-UUID) | The record view route took a uuid path parameter, so bigint-keyed records could be listed and not opened. It now compares `{id_column}::text`. `info_parser.rs:81` still documents "a UUID primary key". | `crates/kernel/src/routes/admin_record_type.rs:115-146` | fixed `3c98c74` | closed | n/a |
| BL-38 (G-RECORD-STRUCTURAL-COLUMNS-UNVALIDATED) | `created_column` and `changed_column` default to columns most external tables lack, nothing checks a declared column exists, and the record list orders by `changed_column`, so an omitted field registers cleanly and 500s when opened. | `crates/kernel/src/plugin/info_parser.rs:115-120`; `crates/kernel/src/routes/admin_record_type.rs:83-86`; `crates/kernel/src/content/record_type.rs:173-200` | open | post | additive as a startup check; frozen if the defaults are removed |
| BL-39 (G-ITEM-NO-MERGE, Argus M2) | A `save-item` update that supplies `fields` replaces the whole object; there is no partial update. | `crates/kernel/src/models/item.rs:326` | open | post | frozen as a default; an opt-in merge key is additive |
| BL-40 (G-DB-NO-TX, Argus M2) | The `db` interface has no transaction. Each statement runs in its own, and semicolons are refused. | `crates/wit/kernel.wit:33-40`; `crates/kernel/src/host/db.rs:71-73,195-222` | open | post | additive |
| BL-41 (G-ADMIN-UI-IS-ADMIN-ONLY, Argus M3) | Every `/admin/content` handler gates on `require_admin`, which checks `users.is_admin`, while the JSON item routes check `create {type} content`. A role granted content permissions can use the API and not the screens. | `crates/kernel/src/routes/helpers.rs:76-94`; `crates/kernel/src/routes/admin_content.rs:102,163,184,218,352,415,569,616` | open | 1.0, **Ritrovo gate** | additive |
| BL-42 (G-NO-PRESAVE-VETO, Argus M3) | `tap_item_presave` can rewrite `fields` and cannot refuse a save, so plugin validation becomes silent coercion. `tap-form-validate` is declared and not dispatched from the admin content route. | `crates/kernel/src/content/item_service.rs:353-390,613-644` | open | post, **Ritrovo gate** | additive |
| BL-43 (G-ITEM-FORM-MISMATCH, Argus M3) | The two item form stacks disagreed about the post encoding and the stored shape, so a `RecordReference` lost its value on edit. Both now accept either shape. What remains is usability: the admin form renders a reference as a text box for a pasted uuid. | `crates/kernel/src/routes/item.rs:127-300`; `crates/kernel/src/content/form.rs:542-564`; `templates/admin/content-form.html:102-105` | fixed `2ff3a62` | closed | n/a |
| BL-44 (G-NO-PLUGIN-HTTP, Argus M3) | A plugin could not serve an HTTP request. Menu entries with `handler_type = "api"` now dispatch `tap_api` with services and the user. | `crates/kernel/src/routes/plugin_api.rs:357`; `crates/kernel/src/menu/registry.rs:32` | fixed `2ff3a62` | closed | n/a |
| BL-45 (64 KB tap buffer, Argus M1) | Tap input and SDK tap output are both capped at 65536 bytes. | `crates/kernel/src/tap/dispatcher.rs:322-331`; `crates/plugin-sdk-macros/src/lib.rs:134-136,252-254` | open | post | frozen |
| BL-46 (cron cadence external only, Argus M1 to M4) | Only `POST /cron/{key}` runs `tap_cron`; the in-process runner drains queues and never dispatches it. The stock `docker-compose.yml` gives the main service no cron poker (only the `argus` profile has one), and no install document mentions one, so `trovato_scheduled_publishing`, enabled by default, silently never publishes. `tap_cron` gets `{timestamp}` and no key, and the WIT declares it with no parameter. | `crates/kernel/src/routes/cron.rs:22`; `crates/kernel/src/cron/mod.rs:738-743,1216-1260`; `docker-compose.yml:110-125`; `crates/wit/kernel.wit:389` | open | 1.0 | additive |
| BL-47 (5 s statement timeout, Argus M1) | Plugin SQL runs under a fixed 5000 ms statement timeout. | `crates/kernel/src/host/db.rs:19,201-203` | open | post | additive |

## From Argus

Rows already carried above from Netgrasp are not repeated: G-ITEM-NO-MERGE (BL-39),
G-SDK-NO-ITEM (BL-19), G-DB-NO-TX (BL-40), G-NO-PRESAVE-VETO (BL-42),
G-NO-GATHER-AGGREGATION (BL-30), G-ADMIN-UI-IS-ADMIN-ONLY (BL-41), G-SDK-NO-ESCAPE
(BL-34), G-VARIABLES-DOUBLE-NAMESPACE (BL-03), G-NO-PLUGIN-HTTP (BL-44),
G-VIEW-OUTPUT-JSON-ENCODED (BL-35), G-ITEM-FORM-MISMATCH (BL-43),
G-EXPOSED-FILTER-NO-MATCH-ALL (BL-31), the read-only record admin (BL-29), the 64 KB
buffer (BL-45), external cron (BL-46) and the statement timeout (BL-47).

Every closure the M1, M2 and M3 status notes claim holds in the code. The M3 note
misses one, G-COMMENTS-UNRENDERED, fixed later. `M4-FRICTION.md` has no status note,
and the first three items of its closing "Remaining gaps before this is sellable"
are stale: the kernel routes embedding requests (BL-54), a plugin save enqueues an
embed job (BL-56), and a plugin can serve a reader write path (BL-44). Its fourth
item, delegated administration (BL-41), still stands.

| ID | Finding | Where | Status | Class | Surface |
|---|---|---|---|---|---|
| BL-48 (G-HTTP-META) | The streaming `http-open` returned a bare handle, so conditional GET was impossible on the streaming path. It now returns status and headers. | `crates/wit/kernel.wit:156`; `crates/kernel/src/host/http.rs:613,636` | fixed `2ff3a62` | closed | n/a |
| BL-49 (G-COST-OPAQUE) | A plugin could not read its own AI cost. `AiResponse.cost_estimate` now carries it. | `crates/plugin-sdk/src/types.rs:1351`; `crates/kernel/src/host/ai.rs:892` | fixed `2ff3a62` | closed | n/a |
| BL-50 (G-QUEUE-RETRY-SIGNAL) | A queue worker has no typed outcome: any successful return deletes the job, and every failure records the same fixed reason string, so the worker's own reason never reaches the dead-letter row and there is no "dead now" outcome. The log's premise that a trap is the only retry signal is no longer true: an `Err` from a `#[plugin_tap_result]` worker also retries. | `crates/kernel/src/cron/mod.rs:228-235`; `crates/plugin-sdk-macros/src/lib.rs:36-41,248-290` | open | post | additive as an opt-in outcome shape; frozen if a plain return is reinterpreted |
| BL-51 (G-SSRF-LOCAL, G-SSRF-NO-TEST-ALLOWANCE) | The `http` host blocks loopback, private ranges and internal hostnames at the URL and again in the resolver, with no allowance of any kind, so no automated test on one machine can exercise a plugin's outbound success path. | `crates/kernel/src/host/http.rs:360-386,416,524-575` | open | 1.0.x | additive |
| BL-52 (150 s background epoch) | Background taps get 150 s and others 10 s. | `crates/kernel/src/plugin/limits.rs:59,62` | open | post | additive |
| BL-53 (256 KB SDK output buffer) | SDK host call results are capped at 256 KB. | `crates/plugin-sdk/src/host.rs:13` | open | post | additive |
| BL-54 (G-AI-EMBED-UNROUTED) | `ai-request` served an embedding request as a chat completion. It now branches on the operation. | `crates/kernel/src/host/ai.rs:175-199,307-360` | fixed `2ff3a62` | closed | n/a |
| BL-55 (G-QUEUE-CONCURRENCY-COLLAPSED) | The drain takes the largest `concurrency` a plugin declares across all its queues and applies it to every job, and claims without filtering by queue, so a stage declared serial runs four wide. `docs/plugin-queue.md:64-68` says concurrency "is now honored" without saying it is per plugin; the commit message of `d3f4cd7` calls per-plugin the documented model. Whether this is a defect or a documentation gap is itself undecided. | `crates/kernel/src/cron/mod.rs:49,179-193,981,1093-1139` | open | 1.0.x | frozen if the existing key gains per-queue meaning; documentation now and an opt-in per-queue key are additive |
| BL-56 (G-ITEM-NO-EMBED) | A plugin-created item was never embedded. `save-item` now enqueues an embed job after every successful write. | `crates/kernel/src/host/item.rs:52-70,222` | fixed `2ff3a62` | closed | n/a |
| BL-57 (G-AI-BASEURL-UNCHECKED, AI-3) | Provider base URL policy is inconsistent, in both directions. `validate_base_url` runs only in the admin form, `embed` and `test_connection`; the plugin `ai-request` path (chat and embedding), the chatbot, AI search, AI assist and assistant tool calling do not call it, `save_provider` does not, and the AI HTTP client has no resolver fence. The check itself is string-only (a hostname resolving to a private address passes) and misses bracketed IPv6 literals. Meanwhile the admin form rejects `localhost` with no allowance, although `docs/design/ai-integration.md:371` describes a local model as "a base_url pointing to localhost". | `crates/kernel/src/services/ai_provider.rs:394-424,475-478,578-593,803,876`; `crates/kernel/src/host/ai.rs:175-217`; `crates/kernel/src/services/ai_chat.rs:494,534`; `crates/kernel/src/services/ai_tools.rs:232`; `crates/kernel/src/routes/admin_ai_provider.rs:256,436` | open | 1.0 | additive (kernel policy; a site with a private base URL sees `ai-request` refused, which is the point) |
| BL-58 (G-COMMENTS-UNRENDERED) | The comment API and template existed and nothing rendered them. Item pages now render the thread and the form works. | `crates/kernel/src/routes/item.rs:778-788`; `crates/kernel/src/routes/comment.rs:940-1006` | fixed `76a5256` | closed | n/a |
| BL-59 (G-ITEM-NO-CREATE-WITH-ID) | A non-nil `id` in a `save-item` payload always means update; if no such item exists nothing is created and the host returns `null`. The WIT documents `save-item` as "insert or update based on whether id exists", which the code does not do. | `crates/kernel/src/host/item.rs:163-186,235-244`; `crates/wit/kernel.wit:11` | open | 1.0.x | frozen either way (the code or its documented contract changes); an opt-in create flag is additive |
| BL-60 (G-CSRF-NO-BEARER-BYPASS) | Decided rather than deferred: a bearer-authenticated plugin API write needs no CSRF token, a cookie session still does. | `crates/kernel/src/routes/plugin_api.rs:249-251,294-310` | fixed `2ff3a62` | closed | n/a |
| BL-61 (G-QUERY-RAW-FIRST-KEYWORD) | The `query-raw` read-only guard checks only that the first keyword is `SELECT` or `WITH`, so a data-modifying CTE writes through it. Argus depends on that for its two atomic claims. Not privilege escalation, since `raw_sql` already grants `execute-raw`, but the SDK documents the function as SELECT-only. | `crates/kernel/src/host/db.rs:40-68,169`; `crates/plugin-sdk/src/host.rs:208-210`; `plugins/argus/src/notify_ports.rs:285,385` | open | 1.0.x | frozen if tightened (and it breaks Argus); documenting it, or a strict variant, is additive |
| BL-62 (G-QUEUE-NO-INTROSPECTION) | No host function reads queue state, so a plugin asks about its own backlog by reading `plugin_queue` through `raw_sql`. | `crates/wit/kernel.wit:177-190` | open | post | additive |
| BL-63 (G-NO-PLUGIN-TIMER) | Nothing tells a plugin author there is no sleep and the queue's `delay` is the only timer. `docs/plugin-development.md` has no epoch note to put it beside. | `docs/plugin-development.md`; `docs/plugin-queue.md:35,88` | open | 1.0.x | additive (documentation) |
| BL-64 (G-ITEM-QUERY-NO-ORDER) | `query-items` does order, by `changed DESC`, but documents no order, and `changed` is whole seconds with no tiebreaker, so paging is unstable within a second and across any update. | `crates/kernel/src/models/item.rs:45,512-516`; `crates/wit/kernel.wit:17-18` | open | 1.0.x | additive (a tiebreaker, documentation, an opt-in `order_by`); frozen if the default order changes |
| BL-65 (M4 migration list, generalised) | Nothing compares a plugin's `[migrations] files` list with the files in its `migrations/` directory, so an unlisted migration is silently never run. Argus shipped a milestone that way. | `crates/kernel/src/plugin/info_parser.rs:521-545`; `crates/kernel/src/plugin/migration.rs:41-80` | open | 1.0.x | additive as a warning; frozen as an error |

## Already in ROADMAP.md and KNOWN-ISSUES.md

Verifying these found four places where KNOWN-ISSUES.md described the code wrongly.
They are corrected in the same change as this page and noted in the rows (BL-68,
BL-69, BL-72, BL-81). Two entries there are not listed as findings: the lettre and
rsa advisory suppressions, whose vulnerable code is not compiled, and the note that
the local test run is a stronger gate than CI, which is advice rather than a defect.

| ID | Finding | Where | Status | Class | Surface |
|---|---|---|---|---|---|
| BL-66 (security review) | The security findings from private development were fixed by their author and never independently re-verified. Nothing in the tree records a review; `docs/security-audit.md` is the cargo-audit suppression policy. | `KNOWN-ISSUES.md` "Security"; `ROADMAP.md` "Security review, in public" | open | 1.0 | n/a (process) |
| BL-67 (page-builder CSS) | The page-builder sanitizer allows the `style` attribute wholesale; the TODO to restrict it to a property allowlist is the only real TODO in `crates/kernel/src` (the other three are the plugin scaffold's template text). `trovato_page_builder` is off by default. | `crates/kernel/src/content/page_builder.rs:91-92` | open | 1.0 | additive; may refuse stored content that uses other properties |
| BL-68 (CSP `style-src`) | `style-src` keeps `'unsafe-inline'` for 305 inline `style=` attributes and 22 `<style>` blocks across the stock templates. KNOWN-ISSUES.md said `script-src` no longer needs an inline exception; the scripts that need one are simply blocked (BL-08). | `crates/kernel/src/middleware/security_headers.rs:37`; `templates/` | open | 1.0 | additive to the plugin contract; tightening it breaks any theme or plugin markup that carries `style=` |
| BL-69 (`tap_perm`) | `tap_perm` is declared and never dispatched, so the kernel knows no plugin's permissions: config import refuses one no role already holds, and the permission grid cannot show or grant one. KNOWN-ISSUES.md said to grant them at the grid; the grid cannot, so the only path is SQL. | `crates/wit/kernel.wit:338-342`; `crates/kernel/src/routes/admin_user.rs:786,821`; `crates/kernel/src/config_storage/yaml.rs:808-840` | open | 1.0, **Ritrovo gate** | additive |
| BL-70 (theme taps) | `tap_theme` and `tap_preprocess_item` are declared and not dispatched, each pending a decision about its semantics. The pinning test matches `dispatch("tap_theme"` only, so a `dispatch_to_plugin("tap_theme", …)` would pass it. | `crates/wit/kernel.wit:371-383`; `crates/kernel/tests/plugin_surfaces_test.rs:216-238` | open | post | additive, though the preprocess overwrite rules are permanent once shipped |
| BL-71 (test isolation) | Some tests were said to fail on a second run against the same database. The concrete cases (fixed WebAuthn usernames) were fixed in `a4fcc7a`, and spot checks of the remaining fixed-name tests found them cleaning up after themselves. Two consecutive full runs on one database would settle it. | `ROADMAP.md` "Test isolation" | cannot reproduce | 1.0.x | n/a |
| BL-72 (plugin mail) | Described as "a plugin's mail from a cron tap is not rate-limited". The real state: background dispatch has no email service, so plugin mail from `tap_cron` or `tap_queue_worker` always fails with `ERR_MAIL_NOT_CONFIGURED` and logs that SMTP is not configured even when it is. The web path is limited per client IP. | `crates/kernel/src/tap/request_state.rs:193-211`; `crates/kernel/src/cron/mod.rs:748-757`; `crates/kernel/src/host/mail.rs:115-121` | open | 1.0.x, **Ritrovo gate** | frozen if background mail starts sending (an existing function's behaviour changes); a truthful error and log line are additive |
| BL-73 (committed binary) | `plugins/ritrovo_importer/ritrovo_importer.wasm` is committed with no manifest, so the loader skips it; its only consumer is the paired-consumer test. | `plugins/ritrovo_importer/` | open | 1.0 | n/a |
| BL-74 (tutorial part 2) | `docs/tutorial/part-02-ritrovo-importer.md` tells the reader to build a package that is not a workspace member, to read source that is not in the tree, and that the manifest enables the plugin by default. | `docs/tutorial/part-02-ritrovo-importer.md:76,83,92,428` | open | 1.0 | n/a (documentation) |
| BL-75 (quick-xml advisories) | RUSTSEC-2026-0194 and -0195 are suppressed: quick-xml 0.38.4 is pinned by plist 1.8.0, reachable only through bundled syntax themes, waiting on a plist release. The suppression file's own review dates (2026-06-01, 2026-07-01, 2026-07-09) have lapsed and one entry still reads "PENDING JEREMY REVIEW". | `.cargo/audit.toml:10,41,44,52-54` | open | 1.0.x | n/a |
| BL-76 (rmcp) | RUSTSEC-2026-0189 is suppressed because only `transport-io` is compiled. Taking rmcp 1.4 is a breaking change to `trovato-mcp`, not to the plugin contract. | `.cargo/audit.toml:68`; `crates/mcp-server/Cargo.toml:18` | open | 1.0.x | additive to the plugin contract |
| BL-77 (pre-freeze manifests) | The compatibility check has no floor, so `api_version = "0.2"` passes at `(0, 102)`. The 1.0 tag ends it by construction, since the major must match, and in the same stroke refuses every plugin still declaring a 0.x API, in or out of tree. | `crates/kernel/src/plugin/info_parser.rs:635-680,1097-1105` | open | post | frozen |
| BL-78 (registry) | There is no plugin package format, install from a URL, or index. | `KNOWN-ISSUES.md` "There is no plugin registry" | open | post | additive |
| BL-79 (vector index) | Semantic similarity is exact; the ivfflat index is commented out. | `crates/kernel/migrations/20260402000001_create_item_embeddings.sql:42-43` | open | post | additive |
| BL-80 (forward-only migrations) | No down migrations and no rollback. | `crates/kernel/migrations/` | open | post | additive |
| BL-81 (template reload) | KNOWN-ISSUES.md described a development-only filesystem watch. There is none: `ThemeEngine::reload` has no caller, so every template change needs a restart in every mode. | `crates/kernel/src/theme/engine.rs:871` | open | post | additive |
| BL-82 (`robots_txt_custom`) | The one kernel variable with no screen and no recorded reason; it is settable only by config import. | `crates/kernel/src/routes/sitemap.rs:132`; `KNOWN-ISSUES.md` "What is configuration import only, in full" | open | 1.0.x | additive |
| BL-83 (plugin variables) | A plugin's variables have no admin screen, by the recorded decision against a generic variable editor. | `crates/kernel/tests/config_admin_coverage_test.rs:48` | open | post | additive |
| BL-84 (freeze held by policy) | Under 0.x SemVer rules `cargo-semver-checks` cannot fail a contract break, so the freeze is held by review. It ends at the 1.0.0 tag with no work. | `.github/workflows/ci.yml:499`; `docs/design/Versioning.md` | open | post | n/a |

## From the AI assistant work

Four things the 0.102 assistant implementation ran into and nobody wrote down. None
is new; each is a row above, with what verifying it added.

| ID | Reported as | Row | What verification found |
|---|---|---|---|
| AI-1 | `save-item` bypasses `ItemService`, so a plugin write fires no taps | BL-25, BL-26 | True, deliberate, and undocumented at the WIT. The same path also skips cache invalidation (BL-26), which is the half a site's visitors see. It does enqueue embedding. |
| AI-2 | No `delete-item` binding in the SDK | BL-19 | Wider: the SDK binds none of the four `item-api` functions. |
| AI-3 | The admin form rejects loopback base URLs | BL-57 | True, with no allowance, while six other outbound AI paths never validate at all. One policy, broken in both directions. |
| AI-4 | `tap_item_view` output reaches the page JSON encoded | BL-35 | Fixed at `2ff3a62`: the kernel decodes view output before appending it. The report came from reading the macro, which still encodes. |

## Found while verifying

Three defects no source recorded, found while checking the ones they did.

| ID | Finding | Where | Status | Class | Surface |
|---|---|---|---|---|---|
| BL-85 | One template that fails to parse empties the whole theme engine. Every root loads in a single call, a Tera parse error fails it, and startup substitutes an empty engine with a WARN, so every page, kernel pages included, is served as a 200 raw dump. This is what the trovato.rs site hit and reported as a missing comments template (BL-14). | `crates/kernel/src/state.rs:656-663`; `crates/kernel/src/theme/engine.rs:244-285` | open | 1.0 | additive |
| BL-86 | A plugin-served request body is capped at 256 KB, sized for "the tap I/O buffer", while the dispatcher refuses tap input over 64 KB. A body in between passes the 413 check and is answered 502 "Plugin handler failed". | `crates/kernel/src/routes/plugin_api.rs:88-92,380-386`; `crates/kernel/src/tap/dispatcher.rs:322-331` | open | 1.0.x | additive |
| BL-87 | `CRON_KEY` defaults to the constant `default-cron-key`, which `.env.example` also ships, and nothing warns when a site runs with it. `POST /cron/{key}` is public, so a site that never set the key lets anyone trigger cron (one run at a time, under the Redis lock), including plugin AI spend and index rebuilds. | `crates/kernel/src/config.rs:642`; `crates/kernel/src/routes/cron.rs:22,35-40`; `.env.example:32` | open | 1.0 | additive |

## From Ritrovo

[Ritrovo](https://github.com/jeremyandrews/ritrovo) is the reference conference
site: five plugins, a bulk importer, an editorial pipeline and admin screens of
its own, run against the released `v0.102.0` image. Its `FRICTION.md` holds 34
entries, two carried from earlier work and the rest written on 2026-09-17 by a
host-in-the-loop test run and a status audit that set every promise in its design
brief against the running demo.

Ten of the 34 are findings this page already carried under another name. They are
not repeated as rows: each is named in the table below with the `BL-` number it
folds into, and where Ritrovo's evidence sharpens that row (a reproduction, a
file and line, a second half nobody had recorded) the sharpening is written into
the row itself.

Read against the source: `FRICTION.md` at Ritrovo `824a884` and
`docs/ritrovo/STATUS.md` at the same commit, which is `main` since pull request
#8 merged at 2026-09-17T16:46:41Z. Every status below is verified against this
tree at `980bf0a` with a file and line. A `FRICTION.md` claim the code did not
bear out is recorded as such in the row rather than dropped.

### The ten that were already here

| Ritrovo entry | Row | What Ritrovo added |
|---|---|---|
| `G-PERM-TAP-NOT-DISPATCHED` | BL-69 | A second consumer, and the count: `ritrovo_access` declares seven permissions, `ritrovo_importer` five, `ritrovo_notify` two, and no role can hold one. The workaround KNOWN-ISSUES.md gave (grant by SQL) is undone by BL-90. |
| `G-TRANSLATION-NO-WRITE-PATH` | BL-02 | Config import has no translation entity (`crates/kernel/src/config_storage/yaml.rs:51-65`), so a seed cannot ship translations either. A plugin can reach the table by declaring it in `db_tables`, which skips cache invalidation, search and revisions. |
| `G-ITEM-API-BYPASSES-ITEM-SERVICE` | BL-25 | `save-item` also cannot set a stage or a language on create (`crates/kernel/src/host/item.rs:213-214`, both `None`), which is why a plugin cannot land content anywhere but Live. Recorded under BL-92, which is where the fix belongs. |
| `G-MAIL-UNAVAILABLE-IN-BACKGROUND` | BL-72 | Confirmed at `crates/kernel/src/tap/request_state.rs:188,208`: both background constructors set `email: None`. Every mail Ritrovo's brief sends is sent from cron or a queue worker, so this is the whole of its notification story, not a corner of it. |
| `G-PRESAVE-CANNOT-REFUSE` | BL-42 | The presave input is `{item_type, title, fields, status}` (`crates/kernel/src/content/item_service.rs:355-361`): no id, no stage, no author, so a tap cannot tell a create from an update. |
| `G-ADMIN-SCREENS-ARE-ADMIN-ONLY` | BL-41 | The comment moderation queue gates the same way (`crates/kernel/src/routes/admin.rs:524,529`), which BL-41 did not name. `trovato_comments` creates a `comment_moderator` role holding `administer comments` that cannot open the queue it exists for. |
| `G-QUEUE-WORKER-ERROR-IS-SUCCESS` | BL-50 | The SDK half: `crates/kernel/src/cron/mod.rs:228-231` treats any returned output as success, so a `#[plugin_tap]` worker returning `{"status":"error"}` has its job deleted with no retry and no dead letter. Only `#[plugin_tap_result]` can signal failure, and nothing in the signature says so. |
| `G-USER-API-NO-ADMIN-BYPASS` | BL-33 | Re-confirmed at `crates/kernel/src/host/user.rs:49` against `crates/kernel/src/tap/request_state.rs:100-102`. |
| `G-API-RATE-LIMITS-FIXED` | BL-01 | Two halves BL-01 did not carry: the limits are not per role (`crates/kernel/src/middleware/rate_limit.rs:99-120`, one `api` figure for everyone), and API tokens exist with no page to manage them, so a site cannot issue one through the interface. |
| `G-NO-REQUEST-PROFILER` | BL-17 | Nothing called Gander exists in this tree, which is worth recording because three design documents name it. Pull request #80 is open against BL-17. |

### The 24 that are new

| ID | Finding | Where | Status | Class | Surface |
|---|---|---|---|---|---|
| BL-88 (G-ITEM-INSERT-OUTPUT-DISCARDED) | `tap_item_insert` runs after the row exists and its output is bound to `_results` and never read, an `Err` included. `tap_item_update`, `tap_item_delete` and the revert dispatch do the same. Both plugin documents give the tap's return type as `Result<(), String>` and its purpose as pre-insert validation, which it cannot be in either half: it runs too late to prevent anything and nothing reads what it reports. The delete site even comments "can abort deletion". | `crates/kernel/src/content/item_service.rs:396-401,652-658,829-834,1505`; `docs/plugin-development.md:195,217`; `docs/plugin-quick-reference.md:60,74` | open | 1.0.x, **Ritrovo gate** | frozen; the contract says one thing and the code does another, so one of them changes |
| BL-89 (G-PLUGIN-INSTALL-WARNS-ON-AN-OVERLAY) | `trovato plugin install` derives a workspace root two directories above the plugin it found and warns when there is no build output there, without first looking at whether the module is already in place. For a plugin on an appended `PLUGINS_DIR` search path, which is how every external plugin is installed, that directory is the overlay's parent and has no `target/`, so the warning prints on every install and tells the operator to build something already built. | `crates/kernel/src/plugin/cli.rs:386-412` (`wasm_src.exists()` with no test of `wasm_dest`) | open | 1.0.x | additive |
| BL-90 (G-PERM-GRID-SAVE-REVOKES-PLUGIN-GRANTS) | Saving the permission grid revokes every plugin permission from every role. The handler builds each role's desired set by filtering `KERNEL_PERMISSIONS` against the submitted checkboxes, and the save has replace semantics, so a permission the grid never rendered is absent from the set and is revoked. This is the other end of BL-69: the only way to hold a plugin permission is SQL, and an administrator changing an unrelated checkbox takes it away. | `crates/kernel/src/routes/admin_user.rs:821-829`; `crates/kernel/src/models/role.rs:202-213` | open | 1.0, **Ritrovo gate** | additive |
| BL-91 (G-REVISION-HISTORY-500) | The revision history page and revert fail for any item that has a revision. `ItemRevision` carries `change_summary` and `ai_generated` beyond the eight columns `get_revisions` and `get_revision` select. Both have `#[serde(default)]`, which does nothing for `sqlx::FromRow`, and neither has `#[sqlx(default)]`, so decoding a row fails on a missing column. An empty result decodes, which is why the page works until the first edit. | `crates/kernel/src/models/item.rs:96-105` (the two fields), `:396-400` and `:409-413` (the two queries); the route reports it at `crates/kernel/src/routes/item.rs:1243` | open | 1.0, **Ritrovo gate** | additive |
| BL-92 (G-NO-ITEM-STAGE-TRANSITION) | Nothing moves one item from one stage to another. The item forms carry no stage and creation passes `stage_id: None`; the bulk actions are `publish`, `unpublish` and `delete`, which set `status`; `save-item` has no stage on create or update; `StageService::publish` moves a whole stage and no route calls it (`state.stage()` has no caller outside `state.rs`). The tutorial ships `variable.workflow.editorial.yml` describing the transitions and their permissions, and the kernel says in its own source that nothing reads it. | `crates/kernel/src/routes/item.rs:957`; `crates/kernel/src/routes/admin_content.rs:293,625-626`; `crates/kernel/src/host/item.rs:213`; `crates/kernel/src/stage/mod.rs:358-363`; `crates/kernel/src/routes/admin_stage.rs:14-19` | open | 1.0, **Ritrovo gate** | additive for the route, the form field and the bulk action; frozen for the `save-item` half (an existing host call starts honouring a field it ignores) |
| BL-93 (G-DEFAULT-STAGE-IGNORED-ON-CREATE) | `/admin/structure/stages` tells the administrator that exactly one stage is the default, "which is where new content lands", and stores `is_default` on `stage_config`. Nothing reads it: creation binds `input.stage_id.unwrap_or(LIVE_STAGE_ID)`, and a search of the item service, the item routes, the content admin routes and the item model finds no reader. Marking a stage default changes nothing. | `crates/kernel/src/models/stage.rs:107-108,290-297` (stored); `crates/kernel/src/models/item.rs:262` (ignored) | open | 1.0, **Ritrovo gate** | frozen; what a create with no stage does is observable to every plugin that calls `save-item` |
| BL-94 (G-MAIL-CANNOT-REACH-A-USER) | A plugin has no way to send mail to one of the site's own users. `mail` has one function and the recipient is always the site's configured contact address, and `user-api` exposes no address either. The refusal to be a relay is right and it also rules out every message a site sends to its own members on a plugin's behalf. | `crates/wit/kernel.wit:212-225`; `crates/kernel/src/host/mail.rs:1-14`; `crates/wit/kernel.wit:55-58` | open | post, **Ritrovo gate** | additive (a new `send-to-user(user-id, subject, body)` where the kernel resolves the address) |
| BL-95 (G-FORM-TAPS-UNREACHABLE) | `tap_form_alter`, `tap_form_validate` and `tap_form_submit` are declared, dispatched by `FormService`, and `FormService` is called by no route: it is constructed on `AppState` and `state.forms()` has no caller. The item forms are built by `FormBuilder` directly and the profile form is hand written. `form_state_cache` exists and its only writer is the content-type field screen, so there is no multi-step flow. The kernel records all of this in its own source. Subsumes the second sentence of BL-42, which named one tap and one route. | `crates/kernel/src/routes/plugin_api.rs:14-18`; `crates/kernel/src/form/service.rs:41-161`; `crates/kernel/src/state.rs:667,1177`; `crates/kernel/src/routes/item.rs:884,1090`; `crates/kernel/src/routes/auth.rs:1092-1101`; `crates/kernel/src/routes/admin_content_type.rs:366-368` | open | 1.0.x, **Ritrovo gate** | additive, on the BL-69 precedent: a declared tap that has never fired has no observable behaviour to change |
| BL-96 (G-QUEUE-NO-CROSS-PLUGIN) | A queue belongs to the plugin that pushes into it, so two plugins cannot share one. The queue host inserts every job under the caller's own plugin name, and the drain hands a claimed job to that plugin's `tap_queue_worker`; the queue name is a free label inside that namespace. `tap_queue_info` is read for one key, `concurrency`, from a JSON array, so `max_retries` and `retry_delay_seconds` are parsed by nothing. | `crates/kernel/src/host/queue.rs:124-127`; `crates/kernel/src/cron/mod.rs:179-193,224-231` | open | post, **Ritrovo gate** | additive as a declared shared queue with the worker resolved by the declaring plugin; frozen if `queue-push` changes meaning |
| BL-97 (G-NO-REQUEST-LANGUAGE) | A plugin cannot tell which language the page is being rendered in. No host call returns the negotiated language, `ApiRequest` carries none, request-context keys are the plugin's own, and variables are namespaced per plugin (BL-03) so the site's language set is unreadable too. `tap_item_view` is dispatched inside `load_for_view`, which the item route calls before it applies the translation overlay, so a view tap sees the untranslated item as well. | `crates/plugin-sdk/src/types.rs:784-812`; `crates/kernel/src/host/request_context.rs`; `crates/kernel/src/host/variables.rs:4-5`; `crates/kernel/src/content/item_service.rs:575-583` against `crates/kernel/src/routes/item.rs:353-356` | open | 1.0.x | additive |
| BL-98 (G-LOCALE-STRINGS-NEVER-LOADED) | UI strings for any language but the default are never loaded, and nothing imports a `.po` file. `trovato_locale` preloads the default language and there is no other `load_language` call; `LocaleService::import_translations` has no caller anywhere in the tree: no route, no CLI command, no config entity. The tutorial ships `docs/tutorial/config/locale/it.po` and nothing reads it. A site configured in two languages serves the second one entirely in the first one's strings. | `crates/kernel/src/state.rs:645` (default only); `crates/kernel/src/services/locale.rs:74` (no caller) | open | 1.0, **Ritrovo gate** | additive |
| BL-99 (G-TILE-GATHER-QUERY-RENDERS-NOTHING) | `gather_query` and `menu` tiles render an empty placeholder that nothing fills: `<div class="tile-gather" data-query-id="…">` and `<nav class="tile-menu" data-menu="…">`, with no server-side rendering and no script in `static/` or `templates/` that reads either class. The tile type match is closed, so a plugin cannot supply one. Two of the five tile types an administrator can place do nothing. | `crates/kernel/src/services/tile.rs:78-126` (the closed match), `:98-107` and `:109-118` (the two empty placeholders); no consumer of `tile-gather` or `tile-menu` outside that file and its tests | open | 1.0, **Ritrovo gate** | additive |
| BL-100 (G-SEARCH-PAGE-BLANK-WITHOUT-INDEX) | `/search` throws away the results the server rendered whenever the Pagefind index is absent, which on a stock install is always: `trovato_search` is disabled by default, so nothing builds one. The mechanism is sharper than the report had it. `search-init.js` hides `#search-fallback`, which holds the server-rendered results, and calls `Scolta.init` as soon as the `Scolta` symbol exists; `Scolta.init` then overwrites its own container with an empty search UI. Neither step waits on Pagefind, and the `import` of `pagefind.js` that fails is never checked. The server-rendered results are still in the HTML, which is what a `curl`-based check sees, so a smoke test passes for a page no visitor can use. | `static/js/search-init.js:20-26`; `static/js/scolta.js:1146,1153-1180` (container overwritten), `:196-198` (unchecked import); `templates/search.html:10,56` | open | 1.0 | additive |
| BL-101 (G-USER-PROFILE-NOT-EXTENSIBLE) | A profile is a username, an email, a timezone and a password, and nothing can add to it. There is no display name, bio, avatar or notification preference, no public profile route, and `users.data` is exposed by no form. The form does not go through `FormService` (BL-95), so a plugin cannot alter it, and a plugin page cannot take an avatar because a plugin route's body is UTF-8 text (BL-107). | `crates/kernel/src/routes/auth.rs:1092-1101`; `templates/user/profile.html:37-59` | open | post, **Ritrovo gate** | additive |
| BL-102 (G-BATCH-NO-EXECUTOR) | A batch can be created, polled, cancelled and deleted, and nothing ever runs one. `update_progress`, `complete` and `fail` are called from nowhere outside `crates/kernel/src/batch/`, and `operation_type` is a free string with no implementation behind any value. `/api/batch` is a published endpoint that records intentions. | `crates/kernel/src/routes/batch.rs:88-114`; `crates/kernel/src/batch/service.rs:99,122,142` (no external caller) | open | 1.0.x, **Ritrovo gate** | additive |
| BL-103 (G-AJAX-ADMIN-ONLY-NO-CONDITIONAL-FIELDS) | `POST /system/ajax` requires the administrator flag and runs with `RequestState::without_services`, so a `tap_form_ajax` handler has no database, and it is closed a third time by a `form_state_cache` lookup nothing writes. There is no conditional field mechanism in the form types at all. | `crates/kernel/src/routes/admin.rs:386-418`; `crates/kernel/src/routes/plugin_api.rs:16-18` (the kernel's own account) | open | post, **Ritrovo gate** | additive |
| BL-104 (G-TUTORIAL-CONFIG-SET-DEFECTS) | The tutorial config set ships in the image (`Dockerfile:72`) and has four defects. (1) The main menu's "Call for Papers" link is `/open-cfps` where the gather's canonical URL and alias are `/cfps`, so the link in the shipped menu is a 404. (2) The three Italian aliases are stored with the prefix and `language: it`; the language middleware rewrites the URI before the alias middleware looks the path up, so those rows can only ever match a doubled prefix. (3) `item_type.conference.yml` declares no `field_topics`, which the tutorial's own importer writes and its gathers filter on, so one save through the edit form drops it. (4) `variable.workflow.editorial.yml` describes transitions nothing reads (BL-92) and `locale/it.po` is imported by nothing (BL-98). | `docs/tutorial/config/menu_link.0193a5a0-0004-7000-8000-000000000003.yml:4` against `gather_query.ritrovo.open_cfps.yml:27`; `url_alias.f1a2b3c4-…yml:4-5` (and two siblings) against `crates/kernel/src/middleware/language.rs:7,158-161` and `crates/kernel/src/middleware/path_alias.rs:66-93`; `item_type.conference.yml` (no `field_topics`) | open | 1.0, **Ritrovo gate** | n/a (configuration and documentation) |
| BL-105 (G-NO-USER-DIRECTORY) | A plugin cannot look up a user it is not currently serving: `user-api` answers only about the caller's own request. The escape hatch is declaring the kernel's `users` table in `db_tables`, which no policy refuses and which hands the plugin every column. That second half is recorded on BL-28, whose allowlist it is, and belongs in the scope of the security review (BL-66). | `crates/kernel/src/host/user.rs:12-56`; `crates/wit/kernel.wit:55-58`; `crates/kernel/src/plugin/db_policy.rs:147-174` | open | post, **Ritrovo gate** | additive (a lookup by id returning public profile fields) |
| BL-106 (G-VIEW-TAP-INPUT-CARRIES-NO-VIEWER) | The plugin documentation describes `tap_item_view(input: ItemViewInput) -> RenderElement`, and there is no `ItemViewInput` in the SDK: the kernel serialises the `Item` alone and the SDK tap returns a `String`. The viewer is reachable anyway, because the tap runs with the viewer's request state, and hiding a field is not a view tap's job: `tap_field_access` carries the viewer and removes fields before any view tap runs. A documentation defect that sent a real plugin to wait for a kernel change nobody needs. | `crates/kernel/src/content/item_service.rs:573,575-583`; `docs/plugin-development.md:215,292` | open | 1.0.x | additive (documentation) |
| BL-107 (G-FILE-NO-HOST-API) | A plugin cannot accept or store a file. There is no file interface among the WIT world's twelve imports, and a plugin route's body is UTF-8 text capped at 256 KiB, so a multipart upload to a plugin page is refused before the plugin sees it. | `crates/wit/kernel.wit:258-270` (the import list); `crates/kernel/src/routes/plugin_api.rs:92,309-313` | open | post, **Ritrovo gate** | additive if the body gains a field or a host call is added; frozen if `ApiRequest::body` changes type |
| BL-108 (G-PLUGIN-ROUTE-NO-HEADERS) | A plugin route sees no request headers and sets none. `ApiRequest` carries callback, method, path, params, query, body, user and a CSRF token; `ApiResponse` sets status, body, content type, theme and title. So a plugin cannot redirect after a POST, set `Cache-Control`, or read `Accept-Language`. | `crates/plugin-sdk/src/types.rs:784-812,884-918`; `crates/kernel/src/routes/plugin_api.rs:408-421` | open | post | additive (new optional fields on both records) |
| BL-109 (G-S3-STORAGE-REMOVED) | There is no S3-compatible storage backend. This is a recorded decision, not an omission: it was an unused, non-default optional feature and the last carrier of the legacy AWS-SDK TLS chain, and the source says so and names the way back. Listed so the documents that promise S3 stop being read as describing something outstanding. | `crates/kernel/src/file/storage.rs:1-8` | decided | closed, **Ritrovo gate** | n/a |
| BL-110 (G-REVISION-NO-COMPARE) | `item_revision.change_summary` holds the added, removed and changed fields, and no route or template renders a comparison of two revisions. Waits on BL-91, since the history page is where a compare view is reached from. | `crates/kernel/src/models/item.rs:96-101`; no compare route in `crates/kernel/src/routes/item.rs` | open | post, **Ritrovo gate** | additive |
| BL-111 (G-SEARCH-NO-ADMIN-OR-ANALYTICS) | Search ranking is a JSON block in a template rather than configuration, there is no search settings screen, and nothing records a query: a search of `crates/kernel/src` and `templates/` finds no query log, no top-query report and no expansion hit rate. Per-type field weights, which do have a screen, are a different thing. | `templates/search.html:100-121`; no `admin/config/search` route in `crates/kernel/src/routes/mod.rs` | open | post, **Ritrovo gate** | additive |

### What the code did not bear out

Nothing in `FRICTION.md` was contradicted outright. Three entries are narrower or
wider than their text, and the rows above carry the corrected version:

- `G-SEARCH-PAGE-BLANK-WITHOUT-INDEX` blames the search template for loading
  `scolta.js`. The template is not the mechanism: `static/js/search-init.js:20-26`
  hides the server results and starts Scolta on the mere presence of the symbol,
  before any Pagefind load is attempted, and `Scolta.init` overwrites its
  container unconditionally. The finding is real and the fix is in the two scripts
  rather than the template (BL-100).
- `G-PLUGIN-INSTALL-WARNS-ON-AN-OVERLAY` describes the derived path. The
  reportable defect is narrower: the code never asks whether the module is already
  at its destination before warning that it is missing (BL-89).
- `G-REVISION-HISTORY-500` says neither extra field carries `#[sqlx(default)]`.
  Both carry `#[serde(default)]`, which reads as a default at a glance and does
  nothing for `sqlx::FromRow`. That is why the defect survived review (BL-91).

`G-S3-STORAGE-REMOVED` is not a defect in either direction: it is a decision the
kernel recorded, and it is listed as closed (BL-109) rather than open.

### Every blocked Ritrovo row, mapped

`docs/ritrovo/STATUS.md` sets all 107 promises in Ritrovo's design brief against
the running demo. Forty-eight carry the status "blocked on the kernel", meaning
they cannot be built on the released image without a kludge. Each row below names
the one identifier whose closure is what lets the row be built: close it and the
Ritrovo prompt that owns the row can do its half.

Where a row has more than one blocker the others are named as well, because the
row is not unblocked until all of them close. The primary is the one the row waits
on longest or the one the others depend on, and it is what the
[unblock order](#ritrovo-unblock-order) schedules.

All 48 map. None maps to nothing, so this pass found no gap in `FRICTION.md`:
every capability a blocked row names is a finding that was already written down,
either here or in Ritrovo's log.

Row ids are Ritrovo's: BMAD story numbers (29.1 to 39.7), `E4.x` for Epic 4's
unnumbered stories, `D1` to `D25` for the brief's "What It Demonstrates" table,
and `P1` to `P19` for its intended plugin taps.

| Ritrovo row | What it needs | Closes it | Also waits on | Ritrovo step |
|---|---|---|---|---|
| 30.3 | Configurable client-side ranking signals | BL-111 | | A9 |
| 30.6 | Sentiment analysis on search | BL-111 | | A9 |
| 30.7 | Search settings screen and query analytics | BL-111 | | A9 |
| 31.6 | AI Assist buttons injected into forms | BL-95 | | A9 |
| 34.4 | Slots, tiles, navigation and breadcrumbs | BL-99 | BL-104 | A9 |
| 35.2 | Role permissions with plugin Grant and Deny | BL-69 | BL-90 | A5 |
| 35.3 | Incoming, Curated, Live with enforced transitions | BL-92 | BL-93 | A5 |
| 35.4 | Revision history with revert, five scenarios | BL-91 | BL-110 | A5 |
| 36.1 | Form API pipeline with `tap_form_alter` | BL-95 | | A6 |
| 36.3 | Conditional CFP fields | BL-103 | | A6 |
| 36.4 | Three-step submission form landing in Incoming | BL-95 | BL-107, BL-92, BL-93 | A6 |
| 36.5 | `ritrovo_cfp` date validation and closing events | BL-42 | BL-96, BL-25 | A6 |
| 36.6 | `ritrovo_access` stage gating and field access | BL-69 | | A5 |
| 36.7 | Profile form with bio, avatar, preferences | BL-101 | BL-95 | A6 |
| 37.2 | Comment moderation queue for editors | BL-41 | | A7 |
| 37.4 | `ritrovo_notify` notifications and digests | BL-94 | BL-72, BL-105, BL-25 | A7 |
| 37.5 | Plugin to plugin through a shared queue | BL-96 | | A7 |
| 37.6 | Comment notifications to subscribers | BL-94 | BL-72 | A7 |
| 38.1 | Multilingual content model | BL-02 | | A8 |
| 38.2 | Language routing, switcher, interface strings | BL-98 | BL-104 | A8 |
| 38.3 | `ritrovo_translate` detection, queue, side by side | BL-02 | BL-15, BL-88 | A8 |
| 38.4 | Italian conferences with English translations | BL-02 | | A8 |
| 38.5 | REST endpoints, including the write half | BL-25 | | A8 |
| 39.2 | Batch operations with progress | BL-102 | BL-92, BL-41 | A5 |
| 39.3 | S3-compatible storage | BL-109 | | A4 |
| D3 | Stages and revisions end to end | BL-92 | BL-91 | A5 |
| D6 | Form API: edit, multi-step, profile, subscription | BL-95 | BL-101 | A6 |
| D8 | Five plugins demonstrating the full tap lifecycle | BL-69 | BL-25, BL-94, BL-95, BL-96 | A7 |
| D9 | Plugin to plugin through the notifications queue | BL-96 | | A7 |
| D10 | Cron: daily import, digest emails, cleanup | BL-94 | BL-72 | A7 |
| D12 | Tiles: CFPs, this month, topic cloud, subscriptions | BL-99 | | A9 |
| D15 | Users and auth: profiles with bio and avatar | BL-101 | | A6 |
| D16 | Permissions: five roles, Grant and Deny, stage scope | BL-69 | BL-90 | A5 |
| D19 | REST API with keys and rate limits | BL-25 | BL-01 | A8 |
| D20 | Revisions: log, preview, revert to N, compare | BL-91 | BL-110 | A5 |
| D21 | i18n: translated content, interface strings, switcher | BL-02 | BL-98 | A8 |
| D22 | Multi-step submission with state in PostgreSQL | BL-95 | | A6 |
| D23 | AJAX: conditional fields, toggle, filters | BL-103 | | A6 |
| D25 | Batch: bulk publish Curated to Live, progress | BL-102 | BL-92, BL-41 | A5 |
| P6 | `ritrovo_cfp` validate dates, emit `cfp_closing_soon` | BL-42 | BL-88, BL-96, BL-25 | A6 |
| P9 | `ritrovo_notify` queue a notification on change | BL-25 | | A7 |
| P11 | `ritrovo_notify` worker sends mail or queues a digest | BL-94 | BL-72 | A7 |
| P12 | `ritrovo_notify` cron digest email | BL-94 | BL-72 | A7 |
| P13 | `ritrovo_translate` detect language, flag for translation | BL-88 | BL-25 | A8 |
| P15 | `ritrovo_translate` cron processes the translation queue | BL-02 | | A8 |
| P16 | `ritrovo_translate` language selector on the edit form | BL-95 | | A8 |
| P17 | `ritrovo_access` Grant, Deny, Neutral by role and stage | BL-69 | | A5 |
| P18 | `ritrovo_access` declares the editorial permissions | BL-69 | | A5 |

Seventeen of the 48 map to an identifier this page already carried (BL-02, BL-25,
BL-41, BL-42, BL-69) and 31 to one of the new rows. Counted by identifier, the
weight is where Ritrovo's own audit put it: BL-69 with BL-90 carries six rows,
BL-95 six, BL-94 five, BL-02 five, BL-25 three.

### Eight findings a blocked row names in passing

These carry the gate mark although no row's primary, because a row above does not
close until they close too: **BL-15** (38.3 cannot use translate screens whose
templates do not exist), **BL-72** (37.4, D10, P11 and P12 send their mail from
cron or a queue worker), **BL-90** (35.2 and D16 lose by the grid what SQL
granted), **BL-93** (35.3 and 36.4 need new content to land where the operator
said), **BL-104** (34.4's menu links and 38.2's aliases), **BL-105** (37.4 must
resolve a subscriber), **BL-107** (36.4's logo upload), and **BL-110** (35.4 and
D20 want a comparison).

### Findings with no blocked row behind them

Six findings from Ritrovo's log block no row with the status "blocked on the
kernel", and so do not carry the mark. They are recorded because a Ritrovo step
still names them, and because two are 1.0 blockers on the day-one test in their own
right. Nothing here is a waiver: the mark follows the rule, and the rule is about
blocked rows.

| Finding | Why no mark | Where it still matters |
|---|---|---|
| BL-100 | E4.5, 34.5 and D14 are "kernel has it, Ritrovo does not use it" | A9 step 1 names it, and it is a 1.0 blocker on the day-one test |
| BL-97 | P14's row is "Ritrovo must build it" | A8 step 2 needs it for a language switcher that knows which language it is on |
| BL-108 | 37.3's row is "Ritrovo must build it" | A7 needs it for a subscribe toggle that works without JavaScript |
| BL-33 | P8 and P19 are "Ritrovo must build it" | A5 and A7, once either checks the viewer |
| BL-50 | P3 is "Ritrovo must build it" | A4 makes the importer's worker a result type to work around it |
| BL-17 | D24 is "kernel has it, Ritrovo does not use it" | A9's cache walkthrough has nothing to profile with |

BL-89 and BL-106 block nothing at all; they are recorded for the operator and the
plugin author respectively.

## The fix series against this page

Twelve pull requests are open against rows on this page as of 2026-09-17, and
none has merged, so no row's status changes because of them and no finding below
carries a fixed-at commit from this series. They are listed because a row they
touch should not be worked twice, and because one of them is a Ritrovo gate.

| Pull request | Row | Gate |
|---|---|---|
| #70 Point the login page's Forgot password link at the recovery page | BL-07 | |
| #71 Bump rustls to 0.23.45 for RUSTSEC-2026-0285 | advisory, no row | |
| #72 Move the login page's passkey script out of the inline block its CSP blocks | BL-08 | |
| #73 Serve .md, .txt and .xml static files with a type a browser displays | BL-09 | |
| #74 Write the two content translation admin templates the routes render | BL-15 | **Ritrovo gate** (38.3) |
| #75 Default the timestamps a hand-written config file omits | BL-11 | |
| #76 Import an item's promote and sticky, and update created on re-import | BL-10 | |
| #77 Stop trovato_blog declaring a tap_item_view it never exported | BL-12 | |
| #78 Remove the menu callbacks the kernel never dispatches from 16 plugins | BL-13 | |
| #79 Render a themed plugin page's title as its heading | BL-16 | |
| #80 Apply the request timing middleware that was attached to no router | BL-17 | |
| #81 Read the field the blog plugin defines in the blog listing teaser | BL-21 | |

#74 closes half of what Ritrovo row 38.3 waits on. The other half is BL-02, the
write path itself: templates for two GET routes do not let anyone write a
translation, and 38.3 stays blocked until BL-02 lands. The Ritrovo unblock order
below depends on #74 rather than repeating it.

RUSTSEC-2026-0285 has no row on this page: it was raised after the verification
pass that produced it. It belongs with BL-75 and BL-76 in the advisory group and
should get a row when the next pass runs.

## Tally

111 distinct findings after merging duplicates: 94 open (one of them, BL-25, partly
fixed), 14 fixed, 2 that do not reproduce, and 1 decided (BL-109). Of the 94 open,
30 are proposed to block 1.0, 30 to ship in 1.0.x, and 34 to wait until after 1.0.
Of the two that do not reproduce, BL-14 is closed and BL-71 stays in 1.0.x until two
consecutive runs on one database confirm it.

The 30: BL-01, BL-02, BL-06, BL-07, BL-08, BL-09, BL-11, BL-12, BL-15, BL-21, BL-22,
BL-41, BL-46, BL-57, BL-66, BL-67, BL-68, BL-69, BL-73, BL-74, BL-85, BL-87, BL-90,
BL-91, BL-92, BL-93, BL-98, BL-99, BL-100, BL-104.

The eight Ritrovo added to that list are argued in
[Why these block 1.0](#why-these-block-10) with the rest. Twenty-six findings
additionally carry the **Ritrovo gate** mark, which is a separate thing from the
class: see [How to read a row](#how-to-read-a-row) and
[Ritrovo unblock order](#ritrovo-unblock-order).

## Why these block 1.0

Each is argued against the test in [How to read a row](#how-to-read-a-row). Effort
is a rough size: small is an afternoon, medium a few days.

**BL-01, static assets rate-limited as API calls.** Day one. A public deployment
sits behind a TLS proxy, and with `TRUSTED_PROXIES` unset (the default) every visitor
shares the proxy's single budget of 100 GETs a minute, pages and assets together.
With it set, crawlers and cold-cache visitors on asset-heavy pages still meet 429s,
and an operator cannot tune it without rebuilding. Exempt `/static` and read the
limits from configuration. Small.

**BL-02 and BL-15, content translation.** The first contradicts the definition: a
site configured in two languages cannot be given translated content through the
interface, only by SQL, while 0.102.0's headline was multilingual and KNOWN-ISSUES.md
pointed operators at a plugin that writes nothing. The second is day one: the plugin
is enabled by default, so both admin routes are mounted and return 500. Either a
writer ships (a config entity or an admin form, with the two templates and a POST
handler) or 1.0 says plainly that content translation is written by SQL or by a
plugin and the two routes come out. Medium, or small for the second choice.

**BL-06, relative URLs and the route panic.** Day one for anyone who submits a
sitemap: the protocol requires absolute URLs, as does `hreflang`, and `SITE_URL`
already exists to build them. The startup panic means installing a plugin whose route
happens to match a kernel route stops the site booting, where it should be a refusal
naming the plugin. Small.

**BL-07, the password reset.** Day one, at the first forgotten password: the link is
a 405, and no part of the email flow is a page a browser can use, so the only reset
is a shell command. Small to medium.

**BL-08, inline scripts under the kernel's own CSP.** Day one: passkey sign-in never
appears on the login page; account recovery, session revocation, passkey management
and the admin recovery settings do not work; and two delete buttons lose their
confirmation. The kernel's security header disables its own security features.
Moving the scripts to `static/js/` is additive. Medium.

**BL-09, missing MIME types.** Day one for any site serving `llms.txt`, a feed, a
WebP image, a PDF or a web app manifest from `static/`: under `nosniff` a browser
downloads what it should display. Small.

**BL-11, config files that require `created`.** Hand-written config for six entity
types fails, and a role's timestamp is a string where every other is an integer. The
config file format is the operating interface for everything without a screen, so
which fields are required and what a timestamp is should be settled before 1.0 calls
that format stable. A default is additive; unifying the types means accepting both.
Small.

**BL-12, the blog's missing export.** The blog is enabled by default, so every item
view on a stock install logs an ERROR and instantiates a module for nothing. An
operator's first look at the logs shows an error per request, which teaches them to
ignore errors. One line in a manifest. Small.

**BL-21, `body` against `field_body`.** Day one on the default blog: teasers render
with no text, and `page` items get no meta, Open Graph or feed description. Read both
names. Small.

**BL-22, listings that cannot link to aliases.** Day one for any listing, the
kernel's own included: every link is `/item/{uuid}`, which serves 200 rather than
redirecting, so a site with aliases publishes two addresses per page and readers
share the ugly one. Medium.

**BL-41, the content admin gating on `is_admin`.** Contradicts the definition for any
site with more than one role: an editor granted content permissions can use the API
and not the screens. And changing who is authorised for what is exactly the change
that should not arrive in a patch release after 1.0. Medium.

**BL-46, cron that nothing drives.** Day one: the stock compose file runs no cron
poker for the main service, no install document mentions one, and scheduled
publishing, enabled by default, silently never publishes. The 1.0 fix need not be a
scheduler; a poker service and a paragraph in the install guide are enough, and an
in-process scheduler can follow as an addition. Small.

**BL-57, the AI provider URL policy.** Both halves. The security clause: the SSRF
check the author wrote runs on two of eight outbound paths, misses IPv6 literals and
never resolves hostnames, which is what an independent review will find first. And
day one: anyone running a local model, which the design document describes as
pointing `base_url` at localhost, is refused with no allowance. One policy: private
addresses refused everywhere unless an operator allows them, enforced at the
resolver. That also gives BL-51 its test allowance. Medium.

**BL-66, the security review.** The definition names it.

**BL-67 and BL-68, the page-builder allowlist and inline styles.** Neither is a day
one problem: the page builder is off by default, and `'unsafe-inline'` for styles is
defence in depth. They block for two other reasons. The definition's security clause
scopes them into the review. And removing `'unsafe-inline'` after 1.0 would break
every theme and plugin fragment carrying `style=`, including the page builder's own
output, which is why the two have to be designed together and before the tag. If the
review decides inline styles stay, that becomes a recorded permanent decision and
both rows move to post.

**BL-69, `tap_perm` not dispatched.** Day one for any site with a non-admin role that
uses a stock plugin with its own permission (comments, media, translation): the only
way to grant it is SQL. Contradicts the definition. Additive. Medium.

**BL-73, the committed binary.** The day-one test does not catch it. It blocks
because ROADMAP.md already decided it does ("Before 1.0, one of those"), and a 1.0
source release should not carry a binary its own loader skips. Small.

**BL-74, tutorial part 2.** A stranger learning Trovato follows the tutorial, and
part 2's first build command fails. Small.

**BL-85, one bad template empties the theme.** Day one for anyone who themes a site:
one typo in an override serves every page as a raw dump with status 200 and only a
startup WARN. Fail startup naming the file, or keep the kernel's templates. Small.

**BL-87, the default cron key.** Day one: the key is a published constant, so a site
that never set it has a public cron trigger. Warn at startup, or refuse to serve cron
with the default key outside development. Small.

**BL-90, the permission grid revoking plugin grants.** Contradicts the definition
together with BL-69. With `tap_perm` undispatched the only way a role can hold a
plugin's permission is SQL, and this row means an administrator saving the grid for
an unrelated reason silently takes it back. So a site running any plugin with a
permission cannot be operated through the interface at all: the interface undoes the
only mechanism that works. Revoke only what the grid rendered. Small.

**BL-91, revision history 500.** Day one, and it needs no plugin: edit any item once
through the admin form and its history page answers 500, as does revert. Revisions
are a headline feature with a screen and an API. Two columns in two queries, or two
attributes on two fields, plus the integration test that edits an item before
loading its history, which is the reason nobody caught it. Small.

**BL-92, no item stage transition.** Contradicts the definition. Stages have a
schema, an admin screen, a gather filter and a documented editorial workflow, and
there is no route, form field, bulk action or host call that moves one item between
them, so the feature can be configured and not used. A site that puts content on a
non-default stage has no way to publish it. The tutorial teaches an editorial
workflow that cannot be performed. Medium.

**BL-93, the default stage ignored on create.** The stage admin screen states in so
many words that the default stage is where new content lands, and nothing reads the
flag. A shipped screen that makes a false statement is worse than a missing feature,
and this one is load bearing: it is how an operator would expect to route incoming
content. Either honour it or take the sentence and the flag out. Small.

**BL-98, interface strings never loaded.** Contradicts the definition for the
feature 0.102.0 led with. A site can declare a second language, negotiate it, serve
it under its own prefix, and every string on the page is still the default
language's, because only the default is ever loaded. `import_translations` exists
with no caller, so a `.po` file cannot be imported by any route, command or config
entity: the tutorial ships one that nothing can read. Multilingual that cannot
translate its own interface is not multilingual. Medium.

**BL-99, two tile types that render nothing.** Day one: an administrator places a
tile through `/admin/structure/tiles`, and a `gather_query` or `menu` tile renders a
titled, empty box on every page it is placed on. Two of the five types the screen
offers do nothing, with no warning on the screen and no script anywhere that would
fill them. Medium.

**BL-100, the search page blank without an index.** Day one on a stock install:
`trovato_search` is disabled by default so no Pagefind index exists, and the search
page hides the results the server rendered and replaces the container with an empty
search box. The server-rendered results stay in the HTML, so a check that greps the
response passes for a page that is blank in a browser. Leave the server results
until the index has actually loaded, which is the progressive enhancement the search
design promises. Small.

**BL-104, the tutorial config set.** Same reason as BL-74, with more surface: the set
ships in the image and the tutorial depends on it from Part 2 onward, so a stranger
learning Trovato meets a 404 in the shipped menu, Italian aliases that answer only at
a doubled prefix, and a form save that drops a field the same set's importer writes.
Three files and a decision about who owns the set. Small.

### Considered and not blocking

- **BL-16**, themed plugin pages without an `<h1>`: a real accessibility defect and a
  broken SDK promise, but `trovato_contact`, the stock plugin that shows it, is off
  by default. 1.0.x.
- **BL-26**, stale pages after a plugin writes an item: visible, but only on sites
  running a plugin that writes items, and bounded by the cache TTL. 1.0.x.
- **BL-36**, the `db` host's silent nulls: the worst finding for a plugin author and
  invisible to a site operator, and the fix that does not change a frozen function
  is an addition that can land at any time. 1.0.x.
- **BL-61**, `query-raw` accepting writing CTEs: not an escalation, because `raw_sql`
  grants `execute-raw` anyway. Documenting it is 1.0.x; tightening it is a 2.0
  question.
- **BL-82**, `robots_txt_custom` without a screen: config import sets it. 1.0.x.

## Ritrovo unblock order

The 26 Ritrovo-gate findings, grouped into twelve batches. Each batch is one
kernel pull request series of a size one session can finish and verify, and the
batches are in the order Ritrovo needs them: A5 editorial, A6 forms, A7 community,
A8 global and API, A9 layout and search. Where a fix serves more than one step it
sits in the earliest batch that needs it.

A batch closes its identifiers. It does not close the Ritrovo rows: those close
when the Ritrovo prompt does its half, which is why the "unblocks" column names
rows rather than claiming them.

**On collisions.** The prompt for this pass asked each batch to say whether it
collides with Core A3, A4 or A5 as those prompts are described in `ROADMAP.md` and
this page. Neither file describes them, and neither does anything else under
`docs/`: a search of `ROADMAP.md`, `KNOWN-ISSUES.md`, `docs/BACKLOG.md` and the
whole of `docs/` for "Core A3", "Core A4" and "Core A5" finds nothing. So the
collision column is written against what this repository does record: the twelve
open pull requests listed in
[The fix series against this page](#the-fix-series-against-this-page), and the
rows already on the 1.0 blocker list. If the Core A prompts exist outside the
repository, their ownership has to be checked against this table by hand before a
batch starts, and the one to check first is BL-69 and BL-02, the two the prompt
named.

### The batches

| Batch | Closes | Unblocks | Surface | Files it touches | Collides with |
|---|---|---|---|---|---|
| **K1 permissions** | BL-69, BL-90 | 35.2, 36.6, D16, P17, P18; most of D8 | additive | `plugin/info_parser.rs` and wherever boot walks the registry, `routes/admin_user.rs`, `config_storage/yaml.rs`, `models/role.rs`, `templates/admin/permissions.html` | BL-69 is already a 1.0 blocker on this page; no open pull request touches it |
| **K2 editorial workflow** | BL-92, BL-93 | 35.3, D3 (with K3), and lets Ritrovo's importer land in Incoming (P3) | **contract change**, twice | `routes/item.rs`, `routes/admin_content.rs`, `models/item.rs`, `models/stage.rs`, `stage/mod.rs`, `host/item.rs`, `crates/wit/kernel.wit`, `templates/admin/content-form.html` | none open; needs K1 first, because a transition is gated on a permission |
| **K3 revisions** | BL-91, BL-110 | 35.4, D20, D3 (with K2) | additive | `models/item.rs`, `routes/item.rs`, `templates/item/revisions.html`, one new compare template, `crates/kernel/tests/` | none; independent of every other batch |
| **K4 delegated administration and batch** | BL-41, BL-102 | 37.2, 39.2, D25 | additive | `routes/helpers.rs`, `routes/admin_content.rs`, `routes/admin.rs`, `batch/service.rs`, a new executor module | BL-41 is already a 1.0 blocker; needs K1 for permissions that exist and K2 for a stage-publish operation to execute |
| **K5 forms reachable** | BL-95, BL-42, BL-103 | 36.1, 36.3, D22, D23, 31.6, P16, and the form half of 36.4, 36.5, D6 | additive | `form/service.rs`, `routes/item.rs`, `routes/auth.rs`, `routes/admin.rs`, `content/item_service.rs`, `content/form.rs` | none open; needs K1 to permission-gate `/system/ajax` |
| **K6 profile and file** | BL-101, BL-107 | 36.7, D15, and 36.4's logo upload | additive if the request body gains a field rather than changing type | `routes/auth.rs`, `templates/user/profile.html`, `routes/plugin_api.rs`, `crates/plugin-sdk/src/types.rs`, `crates/wit/kernel.wit` | needs K5, because the profile form has to be built through `FormService` before a plugin can add to it |
| **K7 item writes through the service** | BL-25, BL-88 | 38.5's write half, P9, P13, D19, and the imported-change half of 37.4 | **contract change**, twice | `host/item.rs`, `content/item_service.rs`, `crates/wit/kernel.wit`, `crates/plugin-sdk/src/`, `docs/plugin-development.md`, `docs/plugin-quick-reference.md` | none open; independent, and the longest lead time of any batch because it needs a decision first |
| **K8 mail and user lookup** | BL-94, BL-72, BL-105 | 37.4 (with K7), 37.6, D10, P11, P12 | BL-72 is a **contract change**; BL-94 and BL-105 are additive | `crates/wit/kernel.wit`, `host/mail.rs`, `host/user.rs`, `tap/request_state.rs`, `cron/mod.rs` | none open; independent |
| **K9 shared queue** | BL-96 | 37.5, D9, and the event half of 36.5 and P6 | additive as a declared shared queue; a change to `queue-push` semantics would be frozen | `host/queue.rs`, `cron/mod.rs`, `plugin/info_parser.rs`, `docs/plugin-queue.md` | touches the same drain as BL-55, which is open and undecided; settle BL-55's per-queue question in the same series |
| **K10 translation write path** | BL-02, BL-15 | 38.1, 38.3, 38.4, D21, P15 | additive | `routes/admin_translation.rs`, two new `templates/admin/content-translate-*.html`, `config_storage/yaml.rs`, `crates/wit/kernel.wit`, `host/`, `content/item_service.rs` | **depends on pull request #74**, which writes the two templates. Do not rewrite them: take #74 and add the POST handler, the config entity and the host call |
| **K11 interface strings and language** | BL-98, BL-104, and BL-97 with them | 38.2, D21 (with K10), 34.4's menu half | additive | `state.rs`, `services/locale.rs`, `main.rs` (a CLI import), `config_storage/yaml.rs`, `docs/tutorial/config/`, `routes/plugin_api.rs`, `crates/plugin-sdk/src/types.rs` | BL-97 carries no gate mark: it is here because A8 step 2 needs it and it is the same file set |
| **K12 layout and search** | BL-99, BL-111, and BL-100 with them | 34.4, D12, 30.3, 30.6, 30.7 | additive | `services/tile.rs`, `static/js/search-init.js`, `static/js/scolta.js`, `templates/search.html`, a new search settings route and template | BL-100 carries no gate mark and is a 1.0 blocker on the day-one test; it is in this batch because it is the same page |

**BL-109 is in no batch.** There is no S3 backend and its removal is a recorded
decision with a named way back. Ritrovo row 39.3 waits on it and therefore cannot
close, which makes 39.3 the one gate this order cannot schedule. That is recorded,
not waived: lifting the mark on 39.3, or reopening S3, is a decision for the person
who made the ruling.

### The critical path

What has to land before each Ritrovo step can start. A step can start when its
"blocks the start" batches are in; the "needed during" batches can land while the
step is running, because the Ritrovo prompt has buildable rows to work on first.

| Ritrovo step | Blocks the start | Needed during | Can start after |
|---|---|---|---|
| A5 editorial | K1 | K2, K3, K4 | K1 |
| A6 forms | K5 | K6 | K1, K5 |
| A7 community | K8 | K7, K9, and K4 for 37.2 | K4, K8 |
| A8 global and API | K10 | K11, and K7 for the write endpoints | K7, K10 |
| A9 layout and search | none | K12, K11 for the switcher | now |

Read down the "blocks the start" column and the order is K1, then K5, then K8 and
K10, with K2, K3 and K4 following K1 as fast as they can be reviewed. K3 is the
one batch with no dependency in either direction and the smallest surface, so it
is the obvious first thing to write while K1 is being designed.

A9 can start today. Ritrovo's own audit proposed moving its first three items to
the front of A4 because they are defects a visitor meets rather than features, and
this order agrees: nothing in K12 waits on anything.

## Frozen-surface findings

[docs/design/Versioning.md](design/Versioning.md) freezes the plugin contract
**before** 1.0 as well as after it, so "lands before the tag" is not a free window: a
contract change now needs the same explicit exception it would need at 2.0. For every
row marked frozen, the practical route is an opt-in addition (a new function, a new
key, a new variant), which is additive and can land in any minor release.

What does have to happen before the tag is **documentation of current behaviour**,
because at 1.0 what the contract documents becomes the promise. These are additive
and small, and each should land before 1.0 even though its row is classed 1.0.x or
post:

- BL-25: the WIT should say `item-api` writes fire no taps.
- BL-36: the WIT should say which column types the `db` host decodes, and that the
  rest arrive as null.
- BL-55: the queue documentation should say concurrency is per plugin.
- BL-61: the SDK should stop calling `query-raw` SELECT-only.
- BL-64: `query-items` should document its order.

One frozen row has a case for changing code before the tag instead: **BL-59**. The
WIT already documents `save-item` as insert or update by whether the id exists, and
the code does not do that. Making the code match the frozen documentation is a bug
fix to the contract as written; changing the documentation to match the code would
be the contract change. It needs deciding before 1.0 either way.

BL-77 needs no work: the 1.0 tag makes the major version check refuse every 0.x
manifest, pre-freeze or not. Every out-of-tree plugin will have to redeclare its
`api_version` at that tag, which belongs in the 1.0 release notes.

The full list of rows whose obvious fix is frozen: BL-03 (if `get` leaves the
namespace), BL-13 (if the SDK default changes), BL-25, BL-33, BL-36, BL-38 (if the
defaults go), BL-39, BL-45, BL-50 (if plain returns are reinterpreted), BL-55, BL-59,
BL-61, BL-64 (if the default order changes), BL-65 (if a warning becomes an error),
BL-72, BL-77, BL-88, BL-92 (the `save-item` half only), BL-93, BL-96 (if `queue-push`
changes meaning) and BL-107 (if `ApiRequest::body` changes type).

### The five Ritrovo gates that are contract changes

Of the 26 Ritrovo-gate findings, 21 are additive and five change the contract. Each
one is a decision rather than a patch, and the recommendation for each is here so
that the batch that carries it does not have to make the decision on its own.

- **BL-25, `save-item` through `ItemService`.** Recommend the **opt-in addition
  now**, not the behaviour change: a second function, or a flag on the payload,
  that saves through the service with taps, access checks and cache invalidation,
  leaving today's function as it is. Making the existing one do it would start
  firing taps inside plugins' own writes and could refuse a write that succeeds
  today, and both Argus and Netgrasp are written against the current behaviour.
  The default can flip at 2.0. What must land before 1.0 either way is the WIT
  note saying `item-api` writes fire no taps.
- **BL-88, what `tap_item_insert` is for.** Recommend **before 1.0**, as the
  documentation plus an SDK signature that returns nothing. At the tag what the
  contract documents becomes the promise, and today it promises pre-insert
  validation from a tap that runs after the insert and whose return value nobody
  reads. The real validation belongs on presave (BL-42), which is additive and is
  in the same batch as the forms work.
- **BL-72, mail from background dispatch.** Recommend **before 1.0**. An existing
  host call goes from always failing to sending, which is exactly the change that
  should not arrive in a patch release after the tag, and the present state is
  worse than a gap: it reports that SMTP is not configured on a site that has
  configured it.
- **BL-93, the default stage on create, and BL-92's `save-item` half.** Recommend
  **before 1.0**, together, because they are one decision about what a create with
  no stage means. Today it means Live and the admin screen says it means the
  default stage. Changing it later would move every plugin's content silently.

BL-96 and BL-107 are listed as frozen only for the variant nobody should choose.
Recommend the additive variant in both: a declared shared queue rather than new
meaning for `queue-push`, and a new field on the request record rather than a new
type for `body`.

## Where the sources and the code disagree

So that nobody works from the original write-ups without knowing:

- **Netgrasp `FRICTION.md`** claims verification at API `(1,0)`, which never
  existed; calls five 0.99.0 fixes residual (BL-31, BL-35, BL-43, BL-44, and the
  embedding half of BL-25); lists BL-37 as open after 0.100.0 fixed it; cites
  `crates/kernel/tests/netgrasp_sync_test.rs`, which moved out in `3c72d79`; and has
  two pinning tests (BL-35, BL-36) that cannot detect the kernel change they are
  meant to announce.
- **Argus `M4-FRICTION.md`** has no status note, and the first three of its
  remaining gaps are closed. The M3 status note misses G-COMMENTS-UNRENDERED
  (BL-58). M1's "the only retry signal is a trap" (BL-50) and M4's "`query-items`
  promises no ordering" (BL-64) are out of date, and M2's base URL finding (BL-57)
  is narrower than the code.
- **trovato.rs `REPORT.md`**: the comments template is in the image (BL-14);
  `/user/recover` is not a working alternative to the dead link (BL-07); the CSP
  blocks five templates, not one (BL-08); re-import does update `changed` (BL-10);
  six entity types require `created`, not one (BL-11); the startup warnings number
  20 on a default install, not seventeen (BL-13); `item-api` has four functions and
  Argus uses three (BL-19); and the "site-fixable" contact form errors are plugin
  markup in this repository (BL-23).
- **`KNOWN-ISSUES.md`** had four descriptions the code contradicted, corrected with
  BL-68, BL-69, BL-72 and BL-81, and the bookkeeping errors below.
- **`docs/design/ai-integration.md:371`** describes a local model as a localhost
  `base_url`, which the admin form refuses (BL-57).
- **The `d3f4cd7` commit message** calls per-plugin queue concurrency the documented
  model; the queue documentation says concurrency is honoured and does not say per
  plugin (BL-55).

## Bookkeeping corrected alongside this page

Not findings about the kernel, but errors in the files that describe it, all
verified on 2026-09-17 and corrected in the same change:

- `KNOWN-ISSUES.md` still described `argus_notify_test` as timing-sensitive.
  `d3f4cd7` (#67) made the test arrange the ordering it asserts. The entry is gone.
- The changelog credited that fix to `#66`, which is the issue it closed; the pull
  request is `#67`. The entry also sat inside the tagged `v0.102.0` section although
  it merged after the tag (`20baa12`); `docs/RELEASING.md` says such entries
  accumulate under `## Unreleased`, so it moved there.
- `KNOWN-ISSUES.md` and `ROADMAP.md` said the runtime is on wasmtime 47.0.3.
  `Cargo.lock` has 47.0.4 since `a3357b5` (#65).
- `KNOWN-ISSUES.md` said content translations are what `trovato_content_translation`
  handles per item. That plugin is 46 lines declaring one permission and two menu
  entries, and nothing outside the tests inserts an `item_translation` row (BL-02).
- The four descriptions corrected with BL-68, BL-69, BL-72 and BL-81, listed in
  their rows.
