# Argus Milestone 3 — Design Gate

The A5 scope opens with one decision to make deliberately before any code:
**how admins manage feeds and topics.** This note records that decision, the
evidence it rests on, and the three consequences that follow from it.

Verified against the tree at `KERNEL_API_VERSION (0,99)` (`crates/kernel/src/plugin/mod.rs`).
The contract is frozen: nothing here proposes a kernel change, and every gap
found on the way is routed to `M3-FRICTION.md` for the post-1.0 ledger.

---

## The question

M1 deviated from its own story list by keeping feeds and topics as operational
plugin tables rather than Items (`M1-FRICTION.md`, deviation 1), on the grounds
that they carry mutable fetch state (`etag`, `failure_count`, `last_fetched_at`)
that would pay the full Item tax on every fetch. That deviation was correct for a
pipeline nobody had to administer. M3 puts a human in front of it, and a human
has to be able to add a feed.

The gate offers two options, (a) preferred unless the tree argues otherwise:

- **(a)** keep feeds and topics as operational plugin tables and build admin
  surfaces over them.
- **(b)** promote them to Items (`argus_feed`, `argus_topic`), have the pipeline
  read them from the kernel, and reverse the M1 deviation.

## What the frozen kernel actually offers

Option (a) needs one thing: a way for an authenticated admin to **write** a row
in a plugin-owned table. Every candidate surface was checked against the tree,
and every one of them is closed.

| Candidate | Verdict | Evidence |
|---|---|---|
| A plugin-served route via `tap_menu` | **Closed.** The SDK's `MenuDefinition` has a `callback` field; the kernel's does not, so it is dropped on deserialize. The registry feeds navigation links, permission metadata and local tasks. Nothing dispatches a plugin handler for a menu path. | `crates/plugin-sdk/src/types.rs:575` vs `crates/kernel/src/menu/registry.rs:13-42`; consumers at `crates/kernel/src/routes/helpers.rs:206,455` |
| Form taps (`tap_form_alter` / `_validate` / `_submit`) | **Closed.** `FormService` is constructed and exposed on `AppState`, and no route calls `build` or `process`. The taps are never dispatched on any HTTP path. | `crates/kernel/src/state.rs:610,1090`; `crates/kernel/src/form/service.rs:39-160` |
| `tap_form_ajax` via `POST /system/ajax` | **Closed three times over.** The route is `require_admin`; it passes `RequestState::without_services`, so a dispatched tap has no DB handle at all; and it first loads a `form_state_cache` row keyed by `form_build_id`, which nothing ever writes because nothing ever builds a form. | `crates/kernel/src/routes/admin.rs:316-368`; `crates/kernel/src/form/service.rs:164-249,435` |
| Record-type admin | **Closed.** List and view only; there is no create, edit or delete route for a record type. Re-confirms the M1 `[Low, RESIDUAL]` note from the writing side. | `crates/kernel/src/routes/admin_record_type.rs:181-183` |
| `public_functions` + `invoke` | **Closed.** Plugin-to-plugin only; never reachable from a request. | `crates/kernel/src/host/plugin_api.rs:98-130,325-327` |
| Config import (`ConfigEntity`) | **Closed.** The entity list covers item types, categories, tags, variables, languages, gather queries, URL aliases, items, roles, stages, tiles and menu links. Plugin tables are not among them. | `crates/kernel/src/config_storage/mod.rs:109-182` |

Against that, the Item tier is wide open, and it is open **generically** — not
for kernel types only:

- `tap_item_info` upserts each declared type into `item_type`, so a
  plugin-declared type is a first-class content type from boot
  (`crates/kernel/src/content/type_registry.rs`, `sync_from_plugins` →
  `register_type`).
- **The admin UI** — `GET/POST /admin/content/add/{type}` and
  `/admin/content/{id}/edit` — renders a Tera form from the type's declared
  fields, validates required fields and re-renders with errors, and saves
  through `ItemService::create`/`update`, which dispatches `tap_item_presave`
  and `tap_item_insert`/`_update`
  (`crates/kernel/src/routes/admin_content.rs:211-345,706-720`).
- **A JSON API** — `POST /item/add/{type}`, `/item/{id}/edit`,
  `/item/{id}/delete` — takes `Json<CreateItemRequest>`, authenticates from the
  session (which the API-token middleware populates from an
  `Authorization: Bearer` header), requires an `X-CSRF-Token` header, and gates
  on `create|edit|delete <type> content`, exactly the strings
  `PermissionDefinition::crud_for_type` generates
  (`crates/kernel/src/routes/item.rs:684-703`;
  `crates/kernel/src/middleware/api_token.rs:40-95`;
  `crates/plugin-sdk/src/types.rs:638-657`).

Two caveats found by using them, both recorded in `M3-FRICTION.md` rather than
worked around:

- The two stacks disagree about how a field value is stored. The admin form
  writes **flat** values and its template reads them back flat, so it
  round-trips; `FormBuilder`, which backs the *other* `GET /item/add/{type}`
  page, reads `{"value": …}` and so renders every saved value as empty — and
  that page's own `<form>` posts urlencoded to a handler that only accepts JSON,
  so submitting it cannot succeed at all (**G-ITEM-FORM-MISMATCH**). Argus
  stores flat values, which is what the working stack uses.
- The admin UI is gated on `require_admin`, i.e. the `users.is_admin` flag, not
  on permissions (`crates/kernel/src/routes/helpers.rs:74-91`). So the seeded
  `argus_admin` role grants feed management through the **JSON** route but not
  through the admin screens, which remain site-admin-only
  (**G-ADMIN-UI-IS-ADMIN-ONLY**). The role is still worth seeding — it is what
  the permission checks on the JSON path and the gathers read — but it does not
  buy a non-admin a management UI.

## Decision 1 — feeds and topics become Items

**Option (b).** Not because (a) is worse, but because (a) is *inexpressible*:
under the frozen contract there is no way for an admin to create or edit a row
in a plugin-owned table through any surface the kernel serves. "Admin management"
under (a) means handing an operator a SQL client, which is not management.

This reverses M1 deviation 1, as the gate contemplated, and the reversal is
recorded there as well as here.

The reversal is cheaper than it looks, and that is worth stating plainly: the
`Store` trait in `crates/argus-core/src/ports.rs` already speaks in domain terms
(`load_feed`, `load_enabled_feeds`, `load_decide_context`) with no SQL in its
signatures. Feeds moving to the Item tier changes **five method bodies in
`plugins/argus/src/host_ports.rs` and nothing in `argus-core`.** The §9.6 hedge
— a core that names no host function — paid for itself here without being
invoked.

## Decision 2 — configuration lives on the Item, runtime state stays in the table

A feed is two things wearing one name:

- **Configuration**, which an admin owns and edits: name, URL, topic, fetch
  interval, enabled.
- **Runtime fetch state**, which the pipeline owns and rewrites on every fetch:
  `etag`, `last_modified`, `last_fetched_at`, `failure_count`, `last_error`.

Putting both on the Item was considered and rejected on two independent grounds.
`Item::update` replaces the whole `fields` object rather than merging
(**G-ITEM-NO-MERGE**, `M2-FRICTION.md`; `crates/kernel/src/models/item.rs:295`),
so every fetch would have to rewrite the admin's configuration to record an ETag
— and an admin editing a feed while a fetch is in flight would lose one write or
the other. And at 100 feeds on a 15-minute interval that is ~9,600 Item saves a
day, each carrying a revision, to store a string the reader never sees.

So: **`argus_feed` Item = configuration. `argus_feeds` table = fetch state,
keyed by the feed Item's id.** The columns are disjoint; every field has exactly
one writer and exactly one source of truth. This is not the "both
representations" the gate forbids — nothing is duplicated, and no value is
written in two places. It is one entity whose configuration and runtime state
live in the two tiers that fit them.

Topics carry no runtime state, so `argus_topic` is configuration only and the
`argus_topics` table stops being read.

## Decision 3 — the backfill is a one-shot tap, not a migration

The obvious way to move existing rows into Items is a SQL migration. It cannot
work, for a reason worth writing down because it will catch the next plugin
author too:

`item.type` is `NOT NULL REFERENCES item_type(type)`
(`crates/kernel/migrations/20260212000005_create_items.sql:13`), and `item_type`
rows for a plugin's declared types are written at **runtime** by
`ContentTypeRegistry::sync_from_plugins`, which necessarily runs *after*
migrations. A plugin migration that inserts an `argus_feed` Item violates that
foreign key every time. (`item.author_id NOT NULL REFERENCES users(id)` is a
second, smaller obstacle: a migration has no user to attribute the row to.)

So the backfill runs as a **one-shot inside `tap_cron`**, guarded by a key in
`argus_state`, reading the legacy rows through the `db` host and creating Items
through `save-item`. It is idempotent, it is covered by tests, and it converges
on the first cron cycle after upgrade.

**Consequence, stated rather than hidden:** because a migration cannot precede
the backfill, the legacy configuration columns on `argus_feeds`/`argus_topics`
cannot be dropped in the same release that stops using them. After the backfill
they are **inert** — never read, never written, by any code path. Dropping them
is a one-line migration scheduled for the next milestone, and it is listed in
`M3-FRICTION.md` so it does not get lost. Inert is not the same as duplicated,
but it is not as clean as dropped, and the gap is the kernel's ordering, not a
choice.

## Decision 4 — validation is coercion, because rejection is not expressible

The scope asks for validation on admin edits: URL shape, a fetch-interval floor,
topic threshold ranges. The admin form does validate — but only what the *kernel*
knows: that a required field is non-empty, and the built-in per-type checks
(`crates/kernel/src/routes/admin_content.rs:234-258`). A plugin's own rules have
one hook on the save path, `tap_item_presave`, and its contract is **modify, not
veto**: the service merges any `fields` a plugin returns into the input and then
saves unconditionally. There is no return value that refuses the write, and
`tap_form_validate` — which would be the right place — is on the unreachable
form path (`crates/kernel/src/content/item_service.rs:308-345`).

So Argus validates by **coercing in `tap_item_presave`**: the fetch interval is
clamped to its floor, the threshold to `0..=100`, the URL is trimmed and
normalized, and every adjustment is written to a note field the admin can read
rather than applied silently.

A URL that is not a plausible absolute `http(s)` URL can be neither rejected nor
parked — presave can only rewrite `fields`, so it cannot unpublish the Item
either. It is blanked, the reason is written to the note, and the **scheduler**
declines to poll a feed with no usable URL. That is the one place the rejection
can actually be enforced, so that is where it lives.

This is the single most consequential kernel gap M3 found, and it is
**G-NO-PRESAVE-VETO** in the friction log.

## Decision 5 — reader-state writes are not expressible; the gap is the deliverable

The scope flagged the reader-state endpoints as deliberately exploratory. The
answer is negative and worth stating precisely, because the exploration was the
point.

`argus_reactions`, `argus_read_state` and `argus_subscriptions` are plugin-owned
tables, and per the table above there is **no surface in the frozen kernel
through which an authenticated user can write a plugin-owned table.** The
findings behind that (`tap_menu` has no callback; the form/AJAX path is
admin-only, service-less and unreachable) are the same ones that decided the
feed question, and they bite harder here: the feed problem had the Item tier as
an escape, and reader state does not. Modelling an upvote or a per-story read
flag as a revisioned Item is the wrong shape for the entity by two orders of
magnitude, and doing it anyway to claim the box was ticked is the hack the scope
told me not to write.

What M3 ships instead, in order of how real it is:

1. **Read state works end to end.** `tap_item_view` fires on the story page with
   the authenticated `UserContext` **and** a live services handle
   (`ItemService::load_for_view` → `tap_state` → `RequestState::new(user, services)`,
   `crates/kernel/src/content/item_service.rs:299-301,455-491`), and the SDK
   exposes `current_user_id()`. Recording "this user has seen this story" at view
   time is not a workaround — it is what a view tap is for. `argus_read_state` is
   genuinely populated.
2. **Comments and reactions-by-proxy work, because the kernel already serves
   them.** `/api/item/{id}/comments` is a real authenticated JSON API with CSRF,
   for any Item, including `argus_story`
   (`crates/kernel/src/routes/comment.rs:876-880`). Stories being Items means
   discussion needs wiring, not building.
3. **Reactions and subscriptions ship as tables plus read-only,
   `CurrentUser`-scoped gathers, and no writer.** The gathers resolve
   `ContextualValue::CurrentUser` to the viewer's id, and to the nil UUID for an
   anonymous viewer, so they are correctly empty rather than leaky
   (`crates/kernel/src/gather/gather_service.rs:1277-1282`). They are a
   placeholder, they are labelled as one, and **G-NO-PLUGIN-HTTP** is the
   post-1.0 item that would make them real.

The iOS series that dispatches from here should read finding
**G-CSRF-NO-BEARER-BYPASS** before designing its client: API-token auth works,
but `require_csrf_header` has no bearer exemption, so a token client still needs
a session-establishing round-trip to obtain a CSRF token.

> **Resolved by K1 (2026-08-13), `KERNEL_API_VERSION (0,99)`.** Decisions 5 and 4
> above are both overtaken. There *is* now a surface through which an
> authenticated reader writes a plugin-owned table — a `tap_menu` entry with
> `handler_type = "api"` is dispatched to `tap_api` with the authenticated user
> and a live services handle — and `plugins/argus/src/reader_api.rs` is the
> writer `argus_reactions` and `argus_subscriptions` were waiting for. On
> **G-CSRF-NO-BEARER-BYPASS** the posture was decided rather than deferred: a
> state-changing plugin-api request is CSRF-exempt **iff** an
> `Authorization: Bearer` API token authenticated it, because no browser attaches
> a bearer token to a cross-site request by itself and there is therefore no
> forgery to protect against. A cookie-authenticated write still requires
> `X-CSRF-Token`. **The iOS client needs no session round-trip.** Decision 4's
> coercion-not-rejection remains true; `k2-presave-veto` is its ledger entry.

## What this does not change

- `argus_story` stays an Item, for the reason §9.4 gave.
- `argus_articles` stays a lightweight record type. It is the high-volume entity
  and nothing about M3 argues for moving it.
- No plugin table is renamed (the hard fence), and no kernel file is touched.
