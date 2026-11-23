# Bottega's Needs Against `main`

**Status:** Assessment, nothing built
**Date:** 2026-11-22
**Read against:** Trovato `origin/main` at `754119b`
**Question:** Which of the capabilities a multi-author, multi-blog host needs does
Trovato already have, which are on the backlog, and which block a launch?

---

## Answer

Of 38 needs, 5 are present, 18 are partly present, 15 are missing, and none is
covered only by an existing backlog row or an open pull request. Every gap now has
a row in [BACKLOG.md](../BACKLOG.md#from-the-bottega-needs-check-bg1), BL-129 to
BL-153.

The biggest gap is the first one: one install cannot serve more than one site.
The tenant tables and a `tenant_id` column on ten kernel tables are there, but
nothing routes a request to a tenant, nothing scopes a read or a write by it, and
the plugin interface has no idea tenants exist. Only one gap must be settled before
the 1.0 tag, BL-129: what tenancy means for the plugin contract and the stored
data. Building it can come after.

Comments, mail and two-server operation are mostly in place. The open-web
standards (IndieAuth, Webmention, ActivityPub, microformats2) are entirely
absent, and all of them can be plugins after 1.0. Old content can be imported with
its ids and dates but not its authors, users or comments.

The three open pull requests touch no need directly: #178 (a category reference
field) is near 3.2, #191 (a request profiler) is near the scale needs, and #81 (the
blog teaser field) is near the blog listing.

## How to read the table

**Present** means a file and symbol, or a test, shows it working on `main`.
**Partly present** names what is missing. **Planned** would mean an existing
backlog row or open pull request covers the gap; no need fell there. **Missing**
means nothing exists. "Rows" are backlog rows; those numbered BL-129 and up were
added with this note. Paths are under `crates/kernel/` unless they say otherwise.

## The needs

| # | Need | Status | Evidence on `main` | Rows |
|---|---|---|---|---|
| **1** | **Hosting many blogs** | | | |
| 1.1 | Many blogs on one install, each with its own name | partly present | `tenant`, `user_tenant` and `tenant_id` on ten tables (`migrations/20260330000006` to `08`), `models/tenant.rs`. But `middleware/tenant.rs` `resolve_tenant` is never mounted, `TenantContext` is read by nothing, `gather/query_builder.rs` `with_tenant` has no caller, `site_config.key` and `url_alias` uniqueness ignore the tenant, and the WIT has no tenant. | BL-129, BL-130 |
| 1.2 | Own address: subdomain and custom domain | missing | `TenantResolution` is `Default` or `Header` only; `tenant` has no host column; `SITE_URL` is one value (`config.rs`). | BL-130 |
| 1.3 | Own theme | missing | One active theme for the install: `theme/engine.rs` `ThemeEngine::active`, `current_theme_name`. | BL-131 |
| 1.4 | Authors and roles per blog (owner, author, moderator) | missing | `roles`, `user_roles`, `role_permissions` have no tenant; `user_tenant` has no role (`migrations/20260212000002`, `20260330000008`). | BL-132 |
| 1.5 | New blog from the admin or self-service signup, no restart | missing | No tenant route, screen or command. Account self-registration exists: `routes/auth.rs` `register_form`, `models/site_config.rs` `RegistrationMode` (default `AdminOnly`). | BL-133 |
| 1.6 | Gathers, routes, feeds and templates read only at startup? | partly present | Confirmed for routes, feeds and templates; not for gather definitions. Definitions reload: `gather/gather_service.rs` `reload_from_db` on `CacheKind::Gather`, though BL-121 reports new rows unserved until restart. Gather routes and feed routes are built once in `main.rs` (`build_gather_route_router`, `routes::feed::build_feed_router`). Templates are read once (`theme/engine.rs` `ThemeEngine::reload` has no caller). | BL-134, BL-81, BL-121 |
| **2** | **Old content** | | | |
| 2.1 | Import keeping item ids, authors and original dates | partly present | `trovato config import` keeps id, `created`, `changed` (`config_storage/direct.rs` `save_item`) but writes the nil author (BL-120). Users and comments are not entity types (`config_storage/yaml.rs` `ENTITY_TYPE_ORDER`). Item routes, plugin `save-item` and MCP `create_item` assign a new id, the acting user and now. | BL-135, BL-120 |
| 2.2 | 301s for every legacy path | present | `redirect` table (status 301 by default, `plugins/trovato_redirects/migrations/001_create_redirect.sql`) served by `middleware/redirect.rs` `check_redirect`; bulk-loaded as `redirect.<uuid>.yml` by config import, tested in `tests/config_import_test.rs`. There is no admin screen (BL-115), which an import does not need. | BL-115 |
| 2.3 | Revisions removable | missing | Revision routes list, compare, preview and revert only (`routes/item.rs`); trigger `item_revision_immutable` (`migrations/20260330000005_add_revision_ai_generated.sql`); `retention_days` is read by nothing. | BL-136 |
| **3** | **Discovery** | | | |
| 3.1 | Network front page merging the newest posts across blogs | partly present | A gather with no `item_type` sorted by `created` lists every type (`gather/types.rs` `QueryDefinition`), and `routes/front.rs` `with_front_page` serves any path at `/`. None ships, and "across blogs" depends on how BL-129 lets a listing span tenants. | BL-129, BL-130 |
| 3.2 | Tags shared across blogs | partly present | Vocabularies are install-wide when unowned (`migrations/20261124000003_category_owner.sql`), so shared today. But `category` and `category_tag` carry `tenant_id`, so sharing under tenancy is a BL-129 decision. | BL-129 |
| 3.3 | Follow an author | missing | `models/subscription.rs` subscribes to one item and is never called. Per-author feeds exist (`plugins/trovato_blog/migrations/002_author_and_tag_feeds.sql`). | BL-137 |
| 3.4 | Network search over every type, field and comment | partly present | `item_search_vector()` (`migrations/20261125000001_search_covers_text_fields_and_comments.sql`) covers titles, text fields and published comments. Not covered: references, numbers, dates; a comment match returns the item; `/search` is blank without the Pagefind plugin (BL-100). | BL-138, BL-100 |
| 3.5 | Related posts across blogs from the semantic index | partly present | `item_embeddings`, `services/vector_store.rs` `similarity_search`, `gather/types.rs` `FilterOperator::SemanticSimilarity` (text query only, no item-to-item mode). No approximate index (BL-79). | BL-139, BL-79 |
| 3.6 | "On this day" listing | missing | `gather/types.rs` `FilterOperator` has no date-part operator; no `FilterHandler` registered. | BL-140 |
| 3.7 | A random archive item | missing | `gather/types.rs` `SortDirection` is `Asc` or `Desc`; no `SortHandler` registered; no `/random` route. | BL-140 |
| **4** | **Comments without spam** | | | |
| 4.1 | Account required, or a verified Webmention | partly present | Accounts required: `routes/comment.rs` `create_comment_inner` calls `require_login_json`, then checks `post comments`; `tests/comment_frontend_test.rs`. Webmention is absent (5.2). | BL-145 |
| 4.2 | Trust ladder: first comment held, later ones post once one is approved | present | `models/comment.rs` `CommentStatus::for_new_comment`, `trust_threshold`, `Comment::approved_count_for_author`; `tests/comment_trust_ladder_test.rs`. Needs `comment_default_status = pending` and `comment_trust_threshold = 1` (the default is 3, and the threshold is not on any form). | none |
| 4.3 | Rate limits per account and per domain | partly present | `comment` bucket per IP or per user, overridable per role (`middleware/rate_limit.rs`). Nothing per domain. | BL-141 |
| 4.4 | Spam plugin and captcha that work by default | partly present | `plugins/trovato_spam` (needs an AI provider) and `plugins/trovato_captcha` (Turnstile, needs a key, passes requests through without one) exist, both `default_enabled = false`; the comment route never runs `tap_form_validate`. | BL-142 |
| 4.5 | Moderation queue per blog | partly present | `routes/admin.rs` `list_comments` at `/admin/content/comments`, `tests/comment_moderation_test.rs`; one queue for the install. | BL-132 |
| 4.6 | Blocklist shared across the network | partly present | `services/access_rule.rs` `is_blocked` (username, email, host patterns), checked at login and registration only; no link-domain or word list. | BL-143 |
| **5** | **Open standards and ownership** | | | |
| 5.1 | Sign in with your own domain (IndieAuth) | missing | No match for `indieauth`. `plugins/trovato_oauth2` is an authorization server, not a client. | BL-144 |
| 5.2 | Webmention, sent and received, source checked | missing | No match for `webmention` or `pingback`. | BL-145 |
| 5.3 | ActivityPub: follow blogs and authors, replies come back as comments | missing | No match for `activitypub`, `webfinger` or `inbox`. Plugin routes see no request headers (BL-108). | BL-146, BL-108 |
| 5.4 | microformats2 (`h-entry`, `h-card`) in stock templates | missing | No match in `templates/`. | BL-147 |
| 5.5 | Export everything you wrote as Markdown plus JSON | partly present | JSON only: `routes/user_data_export.rs` `export_my_data` at `/user/data-export` (items, comments, account, sessions), `tests/user_data_export_test.rs`. No Markdown; `tap-user-export` is not dispatched, so plugin data is missing. | BL-148 |
| 5.6 | Delete your account | present | `routes/user_delete.rs` (`delete_start` to `delete_confirm_submit`, reauthentication, last-admin guard), `models/user.rs` `User::reattribute_content`; `tests/account_deletion_test.rs`. Posts and comments are kept and reattributed to the anonymous user, not deleted. | none |
| **6** | **Mail** | | | |
| 6.1 | Verification, password reset and comment notification mail | present | `services/email.rs` `EmailService`; `routes/auth.rs` (`PURPOSE_REGISTRATION`, `/user/verify/{token}`); `routes/password_reset.rs`; `routes/comment.rs` `send_comment_notification`; templates in `templates/email/`. | none |
| 6.2 | Mail queued and retried | missing | Sent inline or on a spawned task; on failure it is logged and dropped. `CircuitBreaker` stops attempts but does not retry; no mail table. | BL-149 |
| **7** | **Safety and law** | | | |
| 7.1 | Abuse report form | missing | No report or flag feature; `plugins/trovato_contact` is a general form that stores nothing. | BL-150 |
| 7.2 | Notice and action records with timestamps (EU DSA) | missing | No table or code. | BL-150 |
| 7.3 | Audit rows for moderation | present | Every write service records an `audit_log` row (`plugins/trovato_audit_log`) through `services/audit.rs` `AuditTrail` when the plugin is enabled, listed at `/admin/reports/audit`; `audit.rs` `SecurityAudit` adds role, permission, membership and blocked-state events. | none |
| 7.4 | Upload quotas per user | partly present | Per file only: `file/service.rs` `MAX_FILE_SIZE` (10 MiB, fixed) and the `uploads` rate bucket. No per-user total. | BL-152 |
| 7.5 | Per-blog isolation: no owner reads or edits another's drafts | partly present | Per author today: `content/item_service.rs` `check_access` (own and any permissions), `plugins/trovato_blog` `tap_item_access` grants authors their own posts, `gather/access.rs` filters listings. No grouping above the user (BL-129, BL-132), and BL-120 lets anonymous visitors edit nil-author imported items under such a grant. | BL-129, BL-132, BL-120 |
| **8** | **Scale** | | | |
| 8.1 | Two application servers | present | `docker-compose.cluster.yml`, `docs/deployment/multi-server.md`; Redis sessions (`session.rs`), Redis cron lock (`cron/mod.rs` `CRON_LOCK_KEY`); `tests/cluster_test.rs`. Cron still needs an external trigger (BL-46). | BL-46 |
| 8.2 | Cheap anonymous page and feed reads | partly present | Feeds: `routes/feed.rs` sends `ETag`, `304` and `public, max-age=300` (`ANONYMOUS_MAX_AGE_SECS`). HTML pages: no cache headers and no page cache. | BL-153 |
| 8.3 | Cache stays correct when a blog is created or changed | partly present | Cross-server invalidation: `cache/generation.rs` (`CacheKind`, `bump`), cluster tests s04 and s05. But new gather and feed routes need a restart (BL-134), new gather rows may not be served (BL-121), and in-process caches are not keyed by tenant (BL-130). | BL-134, BL-121, BL-130 |

Totals: present 5 (2.2, 4.2, 5.6, 6.1, 8.1); partly present 18; planned 0;
missing 15.

## Where each fix belongs

Under the kernel minimality rule, tenancy (BL-129 to BL-134), import seams
(BL-135), revisions (BL-136), search (BL-138, BL-139), rate limits (BL-141), the
access-rule check on comments (BL-143), the stock templates (BL-147), user export
(BL-148), mail delivery (BL-149), audit call sites (BL-151), upload quotas
(BL-152) and page caching (BL-153) are kernel work. Following authors (BL-137),
date and random listings (BL-140), spam and captcha (BL-142), IndieAuth (BL-144),
Webmention (BL-145), ActivityPub (BL-146) and abuse reports (BL-150) are plugins;
some need a small kernel seam first, which each row names. Any implementation is
Rust, like the rest of the tree.

## What has to happen before the 1.0 tag

Only BL-129. Every other gap can be added later without changing an existing
host function's answer or a stored row's meaning. BL-129 cannot: today every host
function answers for the whole install, and `site_config` and `url_alias` are
keyed without a tenant. Decide whether host functions and gathers are scoped to
the request's tenant and how a network-wide listing opts out, whether users and
vocabularies are shared, which unique keys gain `tenant_id`, and whether config
export carries a tenant. Then write that into the WIT and the data model.
Two smaller notes belong in the contract documentation before the tag: BL-149's
"a successful `mail` call means accepted, not delivered", and BL-148's dispatch of
the already-declared `tap-user-export`.

## What blocks a private launch

A private launch has invited authors only. Blocking:

- Several blogs on one install: BL-129, BL-130, BL-131 and BL-132, plus the admin
  half of BL-133. Without them, one blog is one install.
- Bringing old blogs across whole: BL-135, so authors, users and comments survive
  the import, and BL-120, so imported items do not become editable by anonymous
  visitors.

A rolling restart after each new blog is tolerable at this size, so BL-134 is not
a blocker yet. Inline mail, missing page caching and the spam gaps do not hurt
when every account is invited.

## What blocks an open launch

An open launch lets strangers sign up. Everything above, and also:

- Strangers creating blogs: the self-service half of BL-133, and BL-134 so a new
  blog's routes and feeds work without a restart.
- Spam and abuse: BL-142 (defences on by default, captcha reaching comments),
  BL-143 (blocklist checked on comments), BL-141 (per-domain limits), BL-150
  (reports and notice-and-action records, which a host in the EU owes), BL-151
  (moderation audit rows) and BL-152 (upload quotas).
- Mail strangers depend on: BL-149, because a lost verification or reset mail
  locks a new user out.
- Load from the open web: BL-153, so anonymous page views stop rendering every
  time.

The open-web standards, following, related posts, "on this day", random items,
Markdown export and revision removal (BL-136, BL-137, BL-139, BL-140, BL-144 to
BL-148) block neither launch. They are what Bottega promises beyond a blog host,
and each can ship after the tag.
