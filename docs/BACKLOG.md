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
