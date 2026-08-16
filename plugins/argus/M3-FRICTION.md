# Argus Milestone 3 — Friction Log

> **Status note (added 2026-08-13).** Five findings here are **closed** by the
> post-freeze kernel-usability batch that shipped in 0.99.0:
> **G-NO-PLUGIN-HTTP** (a plugin serves HTTP requests via `tap_api`, so
> reactions and subscriptions have writers — deviation 5 undone),
> **G-VIEW-OUTPUT-JSON-ENCODED**, **G-ITEM-FORM-MISMATCH** (deviation 3 undone:
> the feed's topic is a `RecordReference` again), **G-EXPOSED-FILTER-NO-MATCH-ALL**
> (deviation 4 is no longer forced, though it was not taken), and
> **G-CSRF-NO-BEARER-BYPASS**, which was decided rather than deferred: a
> bearer-authenticated plugin-api write needs no CSRF token. The rest are
> carried on the road to 1.0; see `KNOWN-ISSUES.md`. This log is the record of
> what the build met at the time.
>
> Two claims in this log did not survive verification, and are corrected there
> and here: `trovato_series`' view tap returned a JSON metadata blob rather than
> markup, and its sibling query read a column (`item_type`) that does not exist
> on `item`, so it had never returned anything at all — the mangled-markup
> symptom was real in kind but not in that plugin.

Produced by building the Argus **reader surface, admin management and reader-state
API** as a pure WASM plugin against the **frozen** PF-5 contract. Where M1 met the
kernel as a pipeline and M2 met it as an intelligence engine, M3 is the first
milestone to meet it as a **UI and API consumer** — and that is a different
kernel. Every item is severity-tagged with `file:line` evidence and phrased as a
concrete, decidable post-1.0 ledger item. **NEW** findings were surfaced by this
build; **RESIDUAL** ones are re-confirmed from the consumer side. No-friction
findings are last, as required.

Verified at `KERNEL_API_VERSION (0,99)`
(`crates/kernel/src/plugin/mod.rs`) with **no kernel, WIT, SDK or
kernel-migration change** in the build session. The design decisions these
findings forced are argued in `M3-DESIGN.md`.

Three findings are load-bearing rather than cosmetic. **G-NO-PLUGIN-HTTP** is why
the reader-state write API does not exist. **G-VIEW-OUTPUT-JSON-ENCODED** puts
stray quote characters on every page any plugin renders into, today, including
`trovato_series`. **G-ITEM-FORM-MISMATCH** means one of the kernel's two
item-form stacks cannot successfully submit at all.

---

## Findings

### G-NO-PLUGIN-HTTP — **[High, NEW]** a plugin cannot serve an HTTP request, so it cannot own an API

This is the finding the milestone was really about, and it is worth stating in
one line: **there is no surface in Trovato 1.0 through which a plugin serves a
request, and therefore no way for an authenticated user to write a plugin-owned
table.**

Each candidate closes for a different reason, which is why it reads as an absence
rather than a decision:

- `tap_menu` looks like routing and is not. The SDK's `MenuDefinition` carries a
  `callback` field (`crates/plugin-sdk/src/types.rs:575,595`); the kernel's
  does not (`crates/kernel/src/menu/registry.rs:13-42`), so it is dropped on
  deserialize. The registry feeds navigation links, permission metadata and local
  tasks (`crates/kernel/src/routes/helpers.rs:206,455`) and nothing else. A
  plugin author reading the SDK will believe they have registered a handler.
- The form taps are unreachable. `FormService` is constructed and exposed on
  `AppState` (`crates/kernel/src/state.rs:610,1090`) and no route calls `build`
  or `process`, so `tap_form_alter` / `tap_form_validate` / `tap_form_submit`
  never fire on any HTTP path.
- `tap_form_ajax` has a route — `POST /system/ajax` — and it is closed three
  times over (`crates/kernel/src/routes/admin.rs:316-368`): it is `require_admin`,
  so no reader reaches it; it passes `RequestState::without_services`, so a
  dispatched tap has no DB handle and could not write a table if it did reach it;
  and it first loads a `form_state_cache` row keyed by `form_build_id`
  (`crates/kernel/src/form/service.rs:164-175,435`), which nothing ever writes
  because nothing ever builds a form.
- `public_functions` + `invoke` is plugin-to-plugin only
  (`crates/kernel/src/host/plugin_api.rs:98-130,325-327`).
- Record-type admin is list and view only
  (`crates/kernel/src/routes/admin_record_type.rs:181-183`).

**Impact, concrete.** `argus_reactions`, `argus_read_state` and
`argus_subscriptions` ship with schemas, indexes, storage functions and unit
tests, and **two of the three have no writer**. Read state works only because
`tap_item_view` happens to fire on a reader's request with both the
authenticated user and a services handle
(`crates/kernel/src/content/item_service.rs:299-301,455-491`) — that is a view
transform being used as the only available write trigger, which works and is
honest, but it is not an API. An upvote has nowhere to go.

**What was deliberately not done:** modelling a reaction as an Item to get at
`POST /item/add/{type}`. It would have worked and it would have been wrong — a
revisioned Item per upvote, per reader, per story — and the scope explicitly
asked for the gap rather than the workaround. `M3-DESIGN.md` Decision 5.

**Recommendation (post-1.0):** honour `MenuDefinition.callback` for a
`handler_type` of `api`, dispatching a `tap_page`/`tap_api` with the request
method, path parameters, query and body, and with services and the authenticated
user context attached. The permission field already on `MenuDefinition` is the
natural gate. Failing that, at minimum **remove `callback` from the SDK type**,
because a field that silently does nothing is worse than no field.

### G-VIEW-OUTPUT-JSON-ENCODED — **[High, NEW]** view-tap HTML reaches the page JSON-encoded

`#[plugin_tap]` serializes a tap's return value with
`serde_json::to_string(&result)` (`crates/plugin-sdk-macros/src/lib.rs:158`).
For a `String`-returning tap that produces a **JSON string literal**: wrapped in
quotes, with every inner `"` escaped to `\"`. The item route then appends that
text to the page's children **without decoding it**
(`crates/kernel/src/routes/item.rs`, "Include plugin render outputs",
`children_html.push_str(&output)`).

So a view tap that returns `<div class="x">hi</div>` puts
`"<div class=\"x\">hi</div>"` on the page: a stray `"`, an attribute whose value
is a backslash, and a second stray `"`.

**This is live today, not hypothetical.** `plugins/trovato_series/src/lib.rs:34`
has exactly this signature and builds double-quoted markup, so every blog page in
a series is already rendering mangled HTML.

**Argus response (a mitigation, not a fix):** the story fragment uses
**single-quoted attributes** and an escaper that emits `&quot;`/`&#x27;` rather
than raw quotes, so the fragment contains no character serde escapes. The round
trip then adds only the two wrapping quotes and leaves the markup itself intact
(`plugins/argus/src/story_view.rs`). A unit test asserts the fragment contains no
`"` or `\`, and
`crates/kernel/tests/argus_reader_test.rs::the_view_output_is_json_encoded_by_the_contract`
pins the kernel behaviour so the day it is fixed, a test says so.

**Recommendation (post-1.0, small):** decode a view tap's output as a JSON string
before appending it, or define view taps as returning
`{ "html": "…" }` and read that key. Either is a few lines; the current behaviour
means no plugin can render correct markup.

### G-ITEM-FORM-MISMATCH — **[High, NEW]** one of the two item-form stacks cannot be submitted, and the other disagrees with it about storage

The kernel has two ways to create an Item of a given type, and they do not agree.

1. `GET /item/add/{type}` renders a form from `FormBuilder`
   (`crates/kernel/src/routes/item.rs:624-656`), emitting
   `<form method="post" action="/item/add/{type}">`
   (`crates/kernel/src/content/form.rs:49-56`). The matching `POST` extracts
   `Json<CreateItemRequest>` (`crates/kernel/src/routes/item.rs:684-691`). An
   HTML form posts `application/x-www-form-urlencoded`, so **submitting the page
   the kernel just rendered cannot succeed** — it is a 415 by construction, and
   no JavaScript in `static/js/` intercepts it.
2. `GET/POST /admin/content/add/{type}` renders a Tera template and extracts
   `Form<ContentFormData>` (`crates/kernel/src/routes/admin_content.rs:211-216`).
   This one works, validates, and re-renders with errors.

They also disagree about the stored field shape. The admin stack writes **flat**
values (`extract_content_fields`, `admin_content.rs:23-33`) and its template
reads them flat (`templates/admin/content-form.html:57-58`), so it round-trips.
`FormBuilder` reads `{"value": …}` (`content/form.rs:435-441`), which nothing on
that path writes — so even if its POST worked, every saved value would render
back as empty. The same mismatch makes `FieldType::RecordReference` lose its
value on edit: `static/js/record-ref.js:51` sets the hidden input to a bare uuid,
and `content/form.rs:309-315` re-reads `value.target_id`.

**Argus response:** configuration Items store **flat** values, matching the stack
that works; and the feed's topic is a plain `Text` field holding a uuid rather
than a `RecordReference`, because losing a feed's topic on every admin edit is
worse than pasting an id (`plugins/argus/src/lib.rs`, `tap_item_info`).

**Recommendation (post-1.0):** pick one shape, and make `FormBuilder`'s page post
what its handler accepts — or delete the `/item/add/{type}` HTML form and keep
the route as the JSON API it actually is.

### G-NO-PRESAVE-VETO — **[Medium, NEW]** a plugin cannot refuse a save, only rewrite it

`tap_item_presave` is the only hook a plugin has on the Item save path, and
`ItemService::create` merges whatever `fields` object comes back and then saves
**unconditionally** (`crates/kernel/src/content/item_service.rs:308-345`; the
update path is the same at `:511-518`). There is no return value that means "do
not save this", the merge covers `fields` only — a returned `status` is ignored —
and `tap_form_validate`, which would be the right hook, is on the unreachable
form path (G-NO-PLUGIN-HTTP).

**Impact.** "Validate the admin's input" becomes "coerce the admin's input".
Argus clamps the fetch interval to `[300s, 7d]` and the relevance threshold to
`0..=100`, and reports every adjustment in a note field the admin can read
(`crates/argus-core/src/config.rs`). A URL that is not a URL can be neither
refused nor parked — presave cannot unpublish the Item — so it is blanked, and
the **scheduler** declines to poll a feed with no usable URL
(`plugins/argus/src/config_host.rs`, `load_enabled_feed_configs`). The
enforcement lands two layers away from the mistake, and the admin finds out by
noticing the feed never fetches.

**Recommendation (post-1.0, additive):** let `tap_item_presave` return
`{"reject": {"field": …, "message": …}}` and have the service surface it as a
form error, or dispatch `tap_form_validate` from the admin content route. Either
turns a silent coercion into the thing an admin actually needs: being told they
typed something wrong, at the moment they typed it.

### G-EXPOSED-FILTER-NO-MATCH-ALL — **[Medium, NEW]** an exposed `equals` filter with no value matches nothing

`resolve_exposed_filters` overwrites a filter's value only when the user supplied
one (`crates/kernel/src/gather/gather_service.rs:1060-1073`); an exposed filter
the user left blank keeps its definition value. The query builder then emits
`field = ''` for `Equals` (`crates/kernel/src/gather/query_builder.rs:500-503`).
It **does** skip empty values for `In`, `NotIn` and `FullTextSearch`
(`:564,571,581`) and drops non-UUID values for `HasTagOrDescendants`
(`gather_service.rs`, documented as "no value → no constraint"), so the
inconsistency is with the kernel's own behaviour elsewhere, not just with
expectations.

**Impact, and it is worse over a record type than over Items.** On an Item gather
the JSONB extraction is text, so a blank exposed filter quietly returns an empty
list. On a **record** gather the column is a real Postgres type, and an empty
string bound against a `uuid` column **raises**:

```text
argus_article_list failed: failed to execute count query:
invalid input syntax for type uuid: ""
```

So M1's shipped `argus_article_list` — and the `/articles` route pointed at it —
does not return an empty page with its filter left blank, which is the default
state of the page. **It returns a 500.** Nothing executed that gather in M1 or
M2; M3's gather tests are what surfaced it, and
`crates/kernel/tests/argus_reader_test.rs::m1s_article_gather_returns_nothing_while_its_topic_filter_is_blank`
pins it so the fix is noticed.

Switching the operator to `Contains` gives match-all when blank (`ILIKE '%%'`)
but the JSONB extraction is `NULL` for a story with no topic, and
`NULL ILIKE '%%'` is not true — so the default view would silently drop every
untopiced story. There is no operator that expresses "optional equality".

**Argus response:** `/stories` and `/stories/archive` carry no exposed topic
filter, and topic filtering is its own route, `/stories/topic?topic=<id>`, where
the value is always supplied
(`plugins/argus/migrations/004_argus_reader.sql`).

**Recommendation (post-1.0):** skip an exposed filter whose resolved value is
empty, for every operator — matching what `In` and `FullTextSearch` already do.

### G-NO-GATHER-AGGREGATION — **[Medium, NEW]** gathers cannot count, so an operational tile cannot be a number

`QueryDefinition` has fields, filters, sorts, relationships and includes
(`crates/kernel/src/gather/types.rs:15-76`) and no grouping or aggregate
projection; `FilterOperator` is comparison, tag, full-text and semantic
(`:184-236`). Tiles come in four types — `custom_html`, `menu`, `gather_query`,
`chat` (`crates/kernel/src/routes/tile_admin.rs`, `build_config`) — and none of
them computes anything.

**Impact.** The scope's "pipeline health: article counts by state" is not
expressible as one tile. Each figure would need its own gather, and the only
number available is the pager's total row count.

**Argus response:** the operational tiles are `gather_query` tiles whose pager
count carries the figure and whose rows show what is behind it. That is the shape
the kernel imposes, not one chosen.

**Recommendation (post-1.0):** an aggregate projection on `QueryDefinition`
(`{"field": …, "function": "count"}` plus an optional group-by), or a `count`
tile type that renders a gather's total without its rows.

### G-COMMENTS-UNRENDERED — **[Medium, NEW]** the comment system has an API and a template and nothing joins them

`/api/item/{id}/comments` GET and POST are a real authenticated JSON API with
CSRF, working for any Item (`crates/kernel/src/routes/comment.rs:876-880,198-227`).
`templates/elements/comments.html` is a complete comment thread with a reply
form. **Nothing includes it** — grep for `elements/comments` across `templates/`
and `crates/` returns only the file itself — and the item view route never loads
comments into its context (`crates/kernel/src/routes/item.rs`, `view_item`).

**Impact.** "Stories are Items, so comments come for free" is half true: the
storage and the API are free, the UI is not there. A story page can only carry a
mount point and leave fetching to a client the kernel does not ship.

**Argus response:** the story fragment emits
`<section class='argus-story__comments' data-comments-for='{id}'>` so a client has
somewhere to attach; the plugin does not ship JavaScript, which would be
out-of-scope guessing at the theme.

**Recommendation (post-1.0, small):** include `elements/comments.html` from the
item template and populate the context in `view_item`. The template and the
queries both already exist.

### G-ADMIN-UI-IS-ADMIN-ONLY — **[Medium, NEW]** the content admin ignores permissions, so a management role cannot manage

Every `/admin/content/...` route is gated on `require_admin`, which returns 403
unless `users.is_admin` is true (`crates/kernel/src/routes/helpers.rs:74-91`);
`require_permission` exists immediately below it (`:99-104`) and is not used
there. The JSON item routes do check permissions
(`crates/kernel/src/routes/item.rs:632-640,694-698`).

**Impact.** The seeded `argus_admin` role grants `create argus_feed content` and
friends, and a user holding it can create a feed through `POST /item/add/…` but
gets a 403 on the screens. Delegating feed management to a non-superuser is
therefore not possible through the UI, which is the ordinary reason to have a
role at all.

**Recommendation (post-1.0):** gate the content admin on
`create|edit|delete <type> content` (falling back to `is_admin`), which is what
the routes it links to already enforce.

### G-ITEM-NO-CREATE-WITH-ID — **[Low, NEW]** `save-item` cannot create an Item with a chosen id

`save-item` reads a non-nil `id` in its payload as "update this Item"
(`crates/kernel/src/host/item.rs:83-133`), so there is no way to create one with
an id the plugin picked.

**Impact.** M3's backfill has to carry M1/M2 configuration rows onto Items whose
ids are already referenced by `argus_articles.feed_id`/`.topic_id`. Preserving
the ids is not expressible, so the backfill mints new Items and repoints its own
rows, recording the mapping in `argus_state` so an interrupted pass resumes
rather than duplicating (`plugins/argus/src/config_host.rs`,
`backfill_legacy_config`). The alternative — writing the `item` table directly
through the `db` host, which `raw_sql = true` permits — was rejected because it
skips the kernel's own creation path.

**Recommendation (post-1.0, additive):** an explicit `{"create": true}` flag, or
honour a supplied id on create when no Item holds it.

### G-SDK-NO-ESCAPE — **[Low, NEW]** the SDK ships no HTML escaping helper

A plugin that renders anything must escape it — the kernel does not escape view
output (G-VIEW-OUTPUT-JSON-ENCODED) — and `crates/plugin-sdk/src/` provides no
helper, while the kernel keeps a good one to itself in
`crate::routes::helpers::html_escape`. Argus writes its own in
`plugins/argus/src/story_view.rs`. Every plugin that renders will write it again,
and some will write it wrong.

**Recommendation (post-1.0, trivial):** export `html_escape` from the SDK.

---

## Residual findings re-confirmed from the consumer side

- **[High, RESIDUAL] G-ITEM-NO-EMBED.** Still exactly as M2 recorded it, and now
  visible from the reader's side: `/stories` is a plain recency list because the
  stories it lists have no embeddings, so the semantic-similarity gather the Item
  tier was chosen for still needs a manual admin backfill to work at all. M3
  changes nothing about this and does not work around it.
- **[Medium, RESIDUAL] G-ITEM-NO-MERGE.** Met from a new direction: it is the
  reason a feed's fetch state stays in `argus_feeds` rather than moving onto the
  feed Item, since every fetch would otherwise have to rewrite the admin's
  configuration to store an ETag (`M3-DESIGN.md` Decision 2).
- **[Medium, RESIDUAL] G-DB-NO-TX.** Shapes the backfill: with no transaction, a
  pass can die between creating an Item and repointing the rows that referenced
  the old id, so the id mapping is persisted per row as it is carried and the
  remapped state row maps to itself as a re-entry guard.
- **[Medium, RESIDUAL] G-SDK-NO-ITEM.** M3 needed `get-item` and `query-items`
  as well as `save-item`, so the hand-rolled binding in
  `plugins/argus/src/item_host.rs` grew rather than shrank.
- **[Low, RESIDUAL] record admin is read-only.** M1 filed this as "no hit —
  read-only admin is exactly right for operational inspection". From the
  management side it is the single fact that decided the design gate: with no
  write surface for a plugin table anywhere, feeds and topics had to become Items.
  Same observation, opposite conclusion, because the milestone changed.

---

## No-friction findings (surfaces that just worked)

- **Plugin-declared content types are first-class.** `tap_item_info` upserts into
  `item_type` at startup, and from that moment the admin content UI, the generic
  item routes, permissions, revisions and gathers all work for `argus_feed` and
  `argus_topic` with no special casing. Reversing M1's deviation cost **five
  method bodies** in `plugins/argus/src/host_ports.rs` and nothing in
  `argus-core` — the host-agnostic split paid for itself again.
- **Item gathers over JSONB fields.** Filtering on
  `fields.field_is_active.value` and sorting by `changed` worked first try,
  including the nested path through the `{"value": …}` wrapper
  (`query_builder.rs`, `jsonb_extract_expr`).
- **`ContextualValue::UrlArg`.** `/stories/topic?topic=<id>` is one line of
  gather definition. `CurrentUser` resolving to the nil uuid for an anonymous
  viewer is the right default — a reader-scoped gather is correctly empty rather
  than leaky.
- **Plugin migrations may seed kernel tables.** Gathers, URL aliases, roles,
  role permissions and tiles all seed from `004_argus_reader.sql` with plain
  `INSERT … ON CONFLICT`, which is what makes a plugin able to ship a whole
  surface rather than just a schema.
- **`tap_item_view` gets the viewer and the services.** Being handed both the
  authenticated `UserContext` and a live DB handle is what makes read state
  recordable at all; it is the one place the kernel gives a plugin a reader's
  request.
- **The WASM-1 capability gate.** Adding `current_user_id()` failed the load with
  a precise message naming the missing `user-api` declaration, rather than
  trapping at call time. Exactly the right failure.

---

## Deviations from the M3 scope list (with reasoning)

1. **Feeds and topics became Items**, reversing M1 deviation 1. This is the
   design gate's option (b), taken because (a) is inexpressible: no write surface
   exists for a plugin-owned table. `M3-DESIGN.md` Decision 1.
2. **The legacy configuration columns are not dropped.** A migration cannot
   precede the backfill (`item.type` references `item_type`, whose rows are
   written at runtime), so the columns are inert rather than gone. **Follow-up:
   one migration dropping `url`, `name`, `topic_id`, `fetch_interval_seconds` and
   `enabled` from `argus_feeds`, and `argus_topics` entirely, once every install
   has cycled.** `M3-DESIGN.md` Decision 3.
3. **The feed→topic reference is a plain `Text` uuid, not a `RecordReference`** —
   forced by G-ITEM-FORM-MISMATCH; the reference widget does not survive an edit.
4. **`/stories` has no exposed topic filter** — forced by
   G-EXPOSED-FILTER-NO-MATCH-ALL. Topic filtering is `/stories/topic?topic=<id>`.
5. **Reactions and subscriptions have no write path** — forced by
   G-NO-PLUGIN-HTTP. Tables, indexes, storage functions and unit tests ship; the
   callers do not exist. `M3-DESIGN.md` Decision 5.
6. **Validation is coercion, enforced at the scheduler** — forced by
   G-NO-PRESAVE-VETO.
7. **Operational tiles are gather tiles, not counters** — forced by
   G-NO-GATHER-AGGREGATION.
8. **The story fragment uses single-quoted attributes** — forced by
   G-VIEW-OUTPUT-JSON-ENCODED.
9. **M2 deviation 8 is not fixed here.** M2 recorded that an unparseable decide
   response loses its cost and flagged it as "worth fixing in M3". It was not:
   M3's scope is the reader and admin surface, and changing the M1 `run_decide`
   signature mid-milestone would have put a pipeline change in a UI commit. It
   stays on the list.
