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

## Sources

| Source | What it is | Written against |
|---|---|---|
| trovato.rs site | `docs/REPORT.md` "What was found" in [trovato-site](https://github.com/jeremyandrews/trovato-site), 22 items plus 3 site-fixable notes | v0.101.0 |
| Netgrasp | `plugins/netgrasp/FRICTION.md` in [netgrasp-trovato](https://github.com/jeremyandrews/netgrasp-trovato), 15 findings plus 9 residual bullets | text from 0.99, two findings from 0.102 |
| Argus | `plugins/argus/M1-FRICTION.md` through `M4-FRICTION.md` in this tree | 0.99 |
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
| BL-02 (SITE-2) | Nothing can write a content translation. The kernel reads and overlays `item_translation`, but no route, API, config import path, host function or shipped plugin inserts into it; outside tests the only writer is SQL. `trovato_content_translation` supplies the table and two menu entries. | `crates/kernel/src/content/item_service.rs:474,490,506,536` (reads only); `crates/kernel/src/routes/admin_translation.rs:20-27` (GET only); `plugins/trovato_content_translation/src/lib.rs` | open | 1.0 | additive |
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
| BL-15 (SITE-15) | The two content translation admin routes render `admin/content-translate-list.html` and `admin/content-translate-edit.html`, which have never existed in git history. `trovato_content_translation` is enabled by default, so both routes are mounted and return a 500 to a user allowed to translate. | `crates/kernel/src/routes/admin_translation.rs:64,103`; `crates/kernel/src/routes/mod.rs:144-148` | open | 1.0 | additive |
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
| BL-25 (G-SAVE-ITEM-BYPASSES-SERVICE, AI-1) | `save-item` and `delete-item` call the `Item` model directly, so a plugin's item write fires no `tap_item_presave`, `tap_item_insert`, `tap_item_update`, `tap_item_update_index` or `tap_item_delete`, and runs no access check. `delete-item` returns 0 for an item that does not exist. The embedding half of the finding is fixed: a plugin save enqueues an embed job. This is deliberate (the module doc cites re-entrancy) and undocumented at the WIT. | `crates/kernel/src/host/item.rs:1-6,186,217,222,281,283`; `crates/kernel/src/content/item_service.rs:368,399,655,832` | partly fixed (embedding at `2ff3a62`) | post | frozen; an opt-in save through the service, and a WIT note now, are additive |
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
| BL-41 (G-ADMIN-UI-IS-ADMIN-ONLY, Argus M3) | Every `/admin/content` handler gates on `require_admin`, which checks `users.is_admin`, while the JSON item routes check `create {type} content`. A role granted content permissions can use the API and not the screens. | `crates/kernel/src/routes/helpers.rs:76-94`; `crates/kernel/src/routes/admin_content.rs:102,163,184,218,352,415,569,616` | open | 1.0 | additive |
| BL-42 (G-NO-PRESAVE-VETO, Argus M3) | `tap_item_presave` can rewrite `fields` and cannot refuse a save, so plugin validation becomes silent coercion. `tap-form-validate` is declared and not dispatched from the admin content route. | `crates/kernel/src/content/item_service.rs:353-390,613-644` | open | post | additive |
| BL-43 (G-ITEM-FORM-MISMATCH, Argus M3) | The two item form stacks disagreed about the post encoding and the stored shape, so a `RecordReference` lost its value on edit. Both now accept either shape. What remains is usability: the admin form renders a reference as a text box for a pasted uuid. | `crates/kernel/src/routes/item.rs:127-300`; `crates/kernel/src/content/form.rs:542-564`; `templates/admin/content-form.html:102-105` | fixed `2ff3a62` | closed | n/a |
| BL-44 (G-NO-PLUGIN-HTTP, Argus M3) | A plugin could not serve an HTTP request. Menu entries with `handler_type = "api"` now dispatch `tap_api` with services and the user. | `crates/kernel/src/routes/plugin_api.rs:357`; `crates/kernel/src/menu/registry.rs:32` | fixed `2ff3a62` | closed | n/a |
| BL-45 (64 KB tap buffer, Argus M1) | Tap input and SDK tap output are both capped at 65536 bytes. | `crates/kernel/src/tap/dispatcher.rs:322-331`; `crates/plugin-sdk-macros/src/lib.rs:134-136,252-254` | open | post | frozen |
| BL-46 (cron cadence external only, Argus M1 to M4) | Only `POST /cron/{key}` runs `tap_cron`; the in-process runner drains queues and never dispatches it. The stock `docker-compose.yml` gives the main service no cron poker (only the `argus` profile has one), and no install document mentions one, so `trovato_scheduled_publishing`, enabled by default, silently never publishes. `tap_cron` gets `{timestamp}` and no key, and the WIT declares it with no parameter. | `crates/kernel/src/routes/cron.rs:22`; `crates/kernel/src/cron/mod.rs:738-743,1216-1260`; `docker-compose.yml:110-125`; `crates/wit/kernel.wit:389` | open | 1.0 | additive |
| BL-47 (5 s statement timeout, Argus M1) | Plugin SQL runs under a fixed 5000 ms statement timeout. | `crates/kernel/src/host/db.rs:19,201-203` | open | post | additive |
