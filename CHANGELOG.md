# Changelog

## Unreleased

- Menus have an administration screen. `/admin/structure/menus` lists a site's
  menus, renders each as an indented tree, and creates, edits, reorders and
  deletes links (title, path, parent, weight, hidden).

  This was the largest parity hole in the tree. Drupal 6 shipped a menu admin UI
  in core; here a site's navigation was editable by exactly one path, hand-writing
  YAML and running `trovato config import`, which is not a thing you ask someone
  to do to rename a link in a navigation bar. `ROADMAP.md` put the form before 1.0
  for that reason, and `menu_admin_absent_test.rs` existed to pin the absence
  until it was built. That test is removed, as its own doc comment said to.

  **It needed no kernel plumbing.** The obvious worry was a menu registry built at
  startup, which would have meant an invalidation hook. There is no such cache for
  this: `routes/helpers::inject_site_context` queries `menu_link` on every render,
  so an edit shows on the next request. A test pins that property from the outside
  (create a link through the form, fetch the front page, see it; hide it, fetch
  again, it is gone) so a future move to a cached registry cannot break it
  quietly. The startup-built `MenuRegistry` holds the *plugin-registered* half,
  which this screen does not write.

  What the screen decides, since a form has to answer questions the model does
  not:

  - **A path must be a local absolute path.** One leading slash, no scheme, no
    protocol-relative `//host`, no `..`, no whitespace or control characters. It is
    not checked for resolving to anything: the router is an axum `Router`, which
    cannot be enumerated, and half the paths a site wants in a menu are aliases
    created after the link. A path that resolves to nothing 404s when clicked,
    which a form cannot know at save time and should not pretend to.
  - **A link cannot be its own ancestor.** Rejected on save by walking the parent
    chain, and unreachable through the interface as well: the parent select omits
    the link's own subtree.
  - **Deleting a link promotes its children to its own parent**, and the listing
    says so before you click. The foreign key's `ON DELETE SET NULL` would instead
    turn a nested branch into a row of top-level links, which is a different
    answer and a worse one.
  - **A parent must be a link in the same menu.** A cross-menu parent would render
    as a root anyway, so accepting it would be a lie about the tree.
  - **Plugin-owned navigation is read-only.** `tap_menu` entries are not rows at
    all; they live in the in-memory registry. They are listed on the index,
    attributed to their plugin, with no edit affordance. A `menu_link` row a plugin
    stamped with its own name is treated the same way, and the routes refuse an
    edit or delete for one even when the URL is typed by hand rather than only
    hiding the button.

  `main` and `footer` are always offered because those are the two the theme
  renders; a menu under any other name is stored and listed, and nothing displays
  it until a template asks for it, which the screen says.

- `config import` writes a menu completely. Hierarchy, visibility, plugin
  ownership and stage placement now survive an import, and an export of what
  landed re-imports to the same thing.

  Root cause: `DirectConfigStorage::save_menu_link` bound seven of the eleven
  columns a menu link has, and `save_tile` bound twelve of thirteen. The omitted
  ones — a link's `parent_id`, `hidden` and `plugin`, and `stage_id` on both
  types — all have column defaults, so every insert succeeded and every row was
  quietly wrong: a tree imported as a flat list of siblings, a hidden link came
  back visible, a plugin's link came back owned by `core`, and anything declaring
  a non-Live stage landed on Live. The config files had to declare these fields to
  parse at all, which is what made it look like they were being applied. Since
  `config import` is the only supported way to edit a site's navigation, composed
  menus were unbuildable by any supported means.

  Binding `parent_id` exposed two things the old insert could not hit:

  - **Save order.** `menu_link.parent_id` is a foreign key onto the same table,
    and the import set arrives sorted by filename, which says nothing about tree
    order. The menu-link group is now ordered so a link's parent is always saved
    first; a link whose parent is an existing row is ready immediately.
  - **Cycles and missing parents.** A parent that is in neither the import set nor
    the database is now a validation failure naming the file and the missing id,
    like every other unresolvable reference, rather than a foreign-key error
    reported against whichever file was saved first. A parent chain that loops is
    also a validation failure: the foreign key permits a cycle, so without this
    check a cyclic menu imported successfully and then could not be rendered.

- A stage that already has its `category_tag` row gains its `stage_config` row
  instead of colliding, so `config export` followed by `config import` into an
  empty database works.

  Found by the round-trip test above rather than looked for. `save_stage` branched
  on "does this stage exist", which has three answers and not two: a stage's tag
  row is exported as a `tag` entity as well as inside the `stage` entity, and tags
  import before stages, so by the time the stage file is applied its tag row
  exists and its `stage_config` row does not. That half-present state read as
  absent, took the create path, and failed on `duplicate key value violates
  unique constraint "category_tag_pkey"` — for every stage a site had added beyond
  the seeded Live one. The three states are now handled as three, and a UUID that
  belongs to a tag in some other category is refused with that category named,
  rather than having a `stage_config` row attached to somebody's topic term.

- Everything in the tree speaks 0.99. Three places still spoke the private
  development repository's numbering, which was never released and which a reader
  of this repository cannot resolve:

  - `crates/wit/kernel.wit` said `tap-api` was "added in KERNEL_API_VERSION
    (1,1)". There is no such API version and never was; `tap-api` shipped in
    0.99.0.
  - `crates/argus-core/src/pipeline.rs` explained an assertion in terms of "the
    pre-(1,1) host", meaning the host before the embedding-routing fix, which
    also shipped in 0.99.0.
  - The inline fixture manifest in
    `crates/kernel/tests/ritrovo_paired_consumer_test.rs` declared
    `api_version = "0.2"` and `version = "1.1.0"`.

  The root cause is that the public repository was cut from a private one whose
  version line was different, and comments are not compiled: nothing checks the
  numbers written in prose, so the pre-cut ones survived the cut. The manifests
  and constants that *are* read by code were already consistent (34 plugin
  manifests at `api_version = "0.99"`, `KERNEL_API_VERSION (0, 99)`).

  Also corrected in the same file, and the more serious of the two problems
  there: the provenance header for the committed `ritrovo_importer.wasm` asserted
  a byte-for-byte reproducible build and cited "the contract-freeze commit
  `9791c24`" as the pinned SDK revision. That commit does not exist in this
  repository, so the claim was unverifiable by the only audience the header has.
  It now claims exactly two things a reader can check, and says plainly that the
  third is missing: the artifact's sha256 (now asserted by a test, so replacing
  the artifact without refreshing the header fails the suite) and the public
  Ritrovo commit it was compiled from. The SDK revision it was built against is
  in no published repository, so there is no rebuild recipe to give yet; saying
  so is more useful than a recipe that cannot be run. KNOWN-ISSUES.md carries the
  same correction.

- The Dockerfile builds the plugins that exist. Deleting `trovato_feeds` and
  `trovato_scolta` left both named in its `cargo build -p ...` list, so the image
  build failed with "package ID specification `trovato_feeds` did not match any
  packages" — after ten minutes of dependency compilation, since that step is
  near the end. `trovato_spam` is added for the same reason: it is a shipped
  plugin, so the image should carry it.

  Note for anyone reading the Docker Publish job: it was already failing before
  this, on `failed to push ghcr.io/jeremyandrews/trovato: denied:
  permission_denied: write_package`. That is a registry credential problem, not a
  build one, and it is untouched here.

- RSS feeds work, are config-driven, and are advertised in every page's head.
  `trovato_feeds` shipped two feeds that could not be served and has been
  removed.

  Two defects in one plugin. Its routes were declared
  `MenuDefinition::new(...).callback(...)`, which leaves `handler_type` at its
  default `"page"`, and it exported no `tap_api`, so
  `routes/plugin_api.rs` skipped both entries: `/rss/insights.xml` and
  `/rss/planet-drupal.xml` 404ed, and `build_rss_item` / `build_rss_feed` were
  dead code wearing `#[allow(dead_code)]` comments that claimed a route callback
  called them at runtime. And those two paths were one specific site's feeds
  hardcoded into a plugin that presented itself as generic.

  A gather query now declares a feed in its display config, and gets an RSS 2.0
  document at the path it names:

  ```yaml
  display:
    feed:
      path: /rss/blog.xml
      title: Blog          # defaults to the query's label
      description: ...     # defaults to the query's description
      items: 20            # capped at 200
  ```

  Feeds are registered at startup from the same query set the gather route
  aliases come from, and skipped with a warning when unusable (a relative path, a
  route pattern, a path a second query already claimed) rather than panicking
  axum on a route conflict. `templates/base.html` emits an autodiscovery
  `<link rel="alternate" type="application/rss+xml">` per feed, so a reader
  finds them without being told the URL out of band.

  Entries carry title, absolute link, matching `guid`, description and
  `pubDate`, and link the item's URL alias when it has one. Descriptions go in a
  CDATA section with any `]]>` split across two sections, so content cannot
  close the section and inject markup into the document.

  **Why this is kernel rather than a rebuilt plugin.** A feed is a rendering of a
  query result, and query execution is kernel infrastructure: it applies the
  stage filter, the access filter and the D-26 over-fetch bounds for a specific
  viewer. Plugin space has no seam onto it — `item-api` offers `query-items`,
  which is an unordered, unfiltered `SELECT ... LIMIT 100` with no viewer, so a
  plugin-built feed would publish whatever the access filter exists to withhold.
  Adding that seam is plugin-contract surface, and the contract is frozen before
  1.0. `routes/sitemap.rs` is the existing precedent for the same reasoning. A
  feed is therefore an execution of the query as whoever fetched it: an
  anonymous aggregator gets exactly what an anonymous visitor sees.

  Also added: `UrlAlias::canonical_aliases_for`, which resolves a page of
  sources in one query rather than one round trip per entry.

- Item pages carry a meta description, a canonical link and Open Graph tags.
  Nothing could emit them before, and the reason was structural rather than an
  oversight: `<head>` is not reachable from a plugin. `trovato_seo` implements
  `tap_item_view`, whose return value the item route appends to the item's body,
  so the best it could do was a hidden `<div data-description>` and its JSON-LD
  script blocks. `tap_item_view_alter`, which could have rewritten the
  surrounding document, is declared in `kernel.wit` but never dispatched. Search
  engines got the JSON-LD; every link preview on every chat and social platform
  got a title and nothing else.

  The kernel now derives the metadata (`content::page_meta`) and puts it in the
  template context, and `templates/base.html` emits it: `description`,
  `canonical`, `og:title`, `og:type`, `og:url`, `og:site_name`,
  `og:description`, `og:image`, `article:published_time`,
  `article:modified_time`, and the one Twitter tag that is not covered by the
  Open Graph fallbacks, `twitter:card`. Every tag is guarded by its value, since
  an empty description tag is a worse signal to a crawler than no description
  tag.

  The description is derived from `field_description`, then `field_body`, then
  the first paragraph block — the same two field names `trovato_seo` reads, plus
  a fallback for block-editor content types, which have no `field_body`. Tags
  are stripped, entities decoded, whitespace collapsed, and the result truncated
  to 160 bytes on a word boundary. `og:image` comes from the item's first image
  block, the only image the kernel can identify without a theme naming a lead
  image field. `og:type` is `article` for the `blog`, `article` and `news` item
  types, matching the mapping `trovato_seo` uses for its JSON-LD `@type` so the
  two cannot disagree on one page.

  Two details worth knowing. The canonical URL is the item's URL alias when it
  has one, so the address a crawler indexes is the address the site links to,
  and both `/item/{uuid}` and the alias name the alias as canonical. And the URL
  values are resolved with `url::Url` and emitted with `| safe`: Tera's escaper
  renders every `/` in a URL as `&#x2F;`, which is legal HTML that naive
  unfurlers read wrong. `Url` resolution percent-encodes anything that could
  close the attribute, non-http(s) schemes are dropped rather than emitted, and
  `&` is written as `&amp;`.

  `SITE_URL` is now on `RuntimeConfig`, since request handling needs it: a
  canonical link and `og:url` are absolute by definition, resolved by a crawler
  with no request context to resolve a relative path against.

- Comments are rendered on item pages, and the comment form works.

  `templates/elements/comments.html` existed and was rendered by nothing: the
  only comment template any route used was `admin/comments.html`. A site could
  accept comments through the JSON API and had no way to show them. The orphan
  template could not have worked either — its form posted
  `application/x-www-form-urlencoded` with no CSRF field, at a route that
  required a CSRF *header* — so rendering it as it stood would have produced a
  form that 415ed on submit.

  The item route now renders the thread under the item, through the theme engine
  so a theme can override it. Comment bodies go through the same
  `FilterPipeline` the API uses; author names are resolved once per author rather
  than once per comment; only published comments appear.

  `POST /api/item/{id}/comments` accepts both encodings, the same shape
  `routes::item::ItemSubmission` uses for the item form:

  - `application/json` with the token in `X-CSRF-Token` — unchanged, including
    the JSON response an API client already got.
  - `application/x-www-form-urlencoded` with the token in a `_csrf` field,
    answered with a redirect back to the item rather than JSON, because the
    caller is a browser following a form submission.

  So commenting works with JavaScript disabled. `static/js/comment-post.js` is a
  progressive enhancement on top: it posts JSON with the header, and on any
  failure hands the submission back to the browser rather than losing it.

  The redirect carries the outcome (`posted`, `pending`, `error`) and the page
  renders it. That exists because of the review queue: a held comment that simply
  does not appear looks to its author like a comment that vanished.

  Also added: `AppState::comments_if_enabled`, a non-panicking accessor.
  `comments()` panics off the plugin gate, which is right for the comment routes
  and wrong for a page render — an item page must not 500 because comments are
  switched off.

- A trust ladder on comment moderation: an account with approved comments skips
  the review queue, while the classifier still runs on everything it posts.

  On a moderated site every comment waits for a human or for the classifier,
  including comments from people who have been read and approved repeatedly. The
  ladder removes that wait where it has been earned: an author with at least
  `comment_trust_threshold` published comments — 3 by default — has new comments
  published immediately.

  Three properties make this safe to have.

  Only *published* comments count. A pending, hidden or spam comment is not
  evidence of anything, which is what stops an account earning trust by posting
  into the queue.

  The ladder only ever promotes out of pending. It cannot publish a comment on a
  site that holds nothing, and it cannot hold one on a site that publishes
  everything — the site's own default is the ceiling.

  It is not a security boundary, because classification is unchanged: `trovato_spam`
  still classifies every comment, and a `spam` verdict applies to a published
  comment as readily as a held one. A trusted account that starts spamming is
  caught by the same pass that catches everyone else; the ladder only decides
  whether the comment waits in the meantime.

  `comment_trust_threshold = 0` turns it off, as does a value that cannot be
  parsed: a ladder nobody can read should not hand out bypasses. The count itself is
  only queried when it could change the answer — a site that publishes immediately,
  or a commenter holding `skip comment approval`, needs no count.

- New plugin: `trovato_spam`, AI comment moderation. Classifies each new comment
  in the background and publishes it, leaves it for a human, or marks it as spam.

  Every piece this needs already existed and none of them were connected. The AI
  provider registry has a `Moderation` operation type that nothing invoked, and the
  admin AI-features screen already lets an operator point a provider at it. The
  `ai_background` capability authorizes an AI call made outside a web request. The
  queue host interface has retries and a dead-letter tier. `tap_comment_insert`
  and `tap_queue_worker` are both dispatched. What was missing was a comment status
  to classify *into*, which is why this arrives with the pending and spam statuses
  rather than before them.

  `tap_comment_insert` pushes a classification job, so nothing slow happens on the
  request that posted the comment. `tap_queue_worker` drains it under the
  background principal, asks the provider for a verdict, and writes the result
  back.

  **The failure posture is closed, into the review queue.** A provider that cannot
  be reached, or an answer that cannot be read as a verdict, traps — which is how
  the queue is told an attempt failed, so the job is retried with backoff and
  dead-lettered if the outage persists. The comment does not move. A `hold`
  verdict changes nothing, for the same reason. Only `publish` publishes, and only
  from pending, so a moderator's decision is never re-decided; `spam` applies to a
  pending or a published comment, because taking spam down after the fact is the
  point of classifying asynchronously. Each write carries the status it expects in
  its `WHERE` clause, making it a compare-and-set: if a human got there first, the
  update matches nothing.

  Every decision is logged, so a false positive can be found rather than inferred.

  Two notes on how it reaches the database. It declares `db_tables = ["comment"]`
  and calls the structured `update`, rather than taking `raw_sql = true` — for one
  column of one table, the checked narrow call beats the unchecked wide one. The
  SDK binds only the raw SQL pair from the `db` interface, so the structured call
  is hand-declared plugin-side in the same way `plugins/argus/src/item_host.rs`
  hand-declares `item-api`; the proper fix is an SDK binding for the structured
  four.

  `default_enabled = false`: a plugin that spends provider tokens is opt-in.

- Comments can be held for review, marked as spam, and the author notification
  fires when a comment becomes visible rather than when it is written.

  Comment status was two-valued (0 unpublished, 1 published) and
  `create_comment` hardcoded `status: Some(1)`, so every comment published the
  instant it was posted and there was no way to hold one. The `skip comment
  approval` permission that `trovato_comments` declares was read by nothing. The
  admin list, meanwhile, labelled status 0 as "Pending" — one `if` for what are
  really four states — so a comment a moderator had hidden displayed as though it
  were waiting for them.

  `CommentStatus` now names four values: unpublished (0), published (1), pending
  (2) and spam (3). Only published is visible, and a stored value this build does
  not recognise is treated as invisible rather than guessed at. Spam is a status
  rather than a deletion, so a false positive can be recovered and a classifier
  has something to learn from. The public read paths bind the published status as
  a parameter instead of spelling `status = 1` in five queries.

  A new `comment_default_status` site setting chooses what a new comment gets:
  `published` or `pending`. Unset means published, which is what every comment
  did before the setting existed — upgrading a site must not silently start
  holding its comments. A value that is set but *unrecognised* resolves to
  pending, because that is the recoverable direction: a comment wrongly held is
  sitting in a queue, while a comment wrongly published is already on the site.
  The setting is on the moderation screen, where its consequences are, and
  `skip comment approval` now does what it says: a commenter holding it bypasses
  the queue.

  The moderation list gained the pending and spam filters, labels that come from
  `CommentStatus` rather than from a template `if`, a "Spam" action, and
  per-status actions (nothing offers to approve a comment that is already
  published). The comment edit form offers all four statuses, and an unknown
  status submitted to either screen is rejected rather than stored.

  **The notification change.** `send_comment_notification` fired on creation. With
  a hold-for-review default that would have mailed the content author the full
  text of every held comment — including the ones the queue exists to catch, which
  is the worst possible recipient for comment spam. It now fires on the
  transition into published, wherever that happens: a comment created published
  still notifies immediately, approving a held comment notifies, re-saving an
  already published comment does not (so an edit cannot double-notify), and a
  comment entering any non-visible status never does. The rule is one pure
  function, `should_notify_on_publish`, so it is unit-tested rather than inferred
  from two call sites.

  Also fixed, found while moving that call: the email preview sliced
  `&comment_text[..500]` on a byte index, which panics on a multi-byte character
  straddling byte 500. It ran in a spawned task, so the panic would have taken
  the send down silently. It now walks back to a character boundary.

- Comment writes and the three AI search endpoints have their own rate-limit
  categories instead of sharing the generic `api` bucket.

  `categorize_path` sent every `/api/...` path to `api`, 100 requests a minute
  per IP and per user. Two things were wrong with that. Comment posting is an
  `/api/` path, so an account could write a hundred comments a minute — nobody
  legitimate does, and a spammer with an account does. And
  `/api/v1/search/expand`, `/api/v1/search/summarize` and
  `/api/v1/search/followup` each spend LLM provider tokens per call, at three
  different costs, while getting the same generous allowance; only `/search` and
  `/api/search` hit the tighter `search` category.

  New categories: `comment` at 4 a minute, and `search_expand` (30),
  `search_summarize` (10) and `search_followup` (5), the numbers
  `docs/design/search-architecture.md` recommends. The specific categories are
  tested before the generic `/api/` arm, which is the ordering that was missing.

  Comment *reads* stay in the `api` bucket: it is the writes that cost moderation
  attention, and a thread loading on a busy page must not spend a posting budget.
  The AI search paths are matched whole rather than by prefix, so a future
  `/api/v1/search/something-else` is not silently handed the cheapest of the
  three limits.

- Managed files carry alternative text, and no template uses a filename as `alt`
  any more.

  Media had no alt field, so every template that rendered an uploaded image
  reached for the nearest string: `templates/form/file-upload.html`,
  `templates/admin/media-library.html` and `templates/admin/file-details.html`
  all emitted `alt="{{ file.filename }}"`. A filename is not alternative text —
  at best it is noise a screen reader reads aloud, at worst it is
  "IMG_4821.jpg" standing in for the content of the image (WCAG F30). The block
  editor already did this correctly, including the decorative case; the media
  entity now can too.

  `file_managed.alt_text` is nullable, and NULL is meaningfully different from
  the empty string: NULL means nobody has said what the image shows, while `""`
  means explicitly decorative, which is the correct alt for an image that carries
  no information (WCAG H67). `FileService::set_alt_text` preserves that
  distinction, treating a whitespace-only value as decorative. Existing rows are
  NULL rather than backfilled with filenames, which would have encoded the defect
  as data.

  The file details page has a field to edit it, the media library shows at a
  glance which images still need it ("No alt text" / "Decorative" / the text
  itself), and all three templates now render the recorded value — falling back to
  `alt=""` rather than to the filename, since in each of those places the filename
  is already displayed as adjacent text.

  Two details for anyone extending this. The field is skipped when serializing
  `None`, because Tera has no `is null` test and a serialized `null` would be
  indistinguishable from `""` in a template; omitted means "never set". And the
  "Delete file" form on the file details page was posting no CSRF field at all, so
  it could not deserialize — found while adding a form to the same page, fixed
  with the token that page now generates.

- Every navigation landmark is labelled, and every active link says it is the
  current page.

  `templates/page.html` had four `<nav>` elements and not one `aria-label`: the
  site nav, two breadcrumbs and the footer nav. A screen reader lists landmarks by
  label, so several unlabelled navigations on one page mean entering each one to
  work out which is which. The active main-menu link was marked only by the CSS
  class `site-nav__link--active`, and assistive technology cannot see a class.

  Labelled: the site nav ("Main"), both breadcrumb trails ("Breadcrumb"), the
  footer nav ("Footer"), the pagination nav in `admin/aliases.html`, and the
  breadcrumbs in `admin/file-details.html`, `admin/tag-form.html` and
  `admin/tags.html`. The already-labelled pagers under `templates/macros/` and
  `templates/gather/` were the pattern.

  `aria-current="page"` now accompanies every active-link marker: the main menu
  (both the database-menu and plugin-menu branches), the trailing breadcrumb in
  the public theme and in the three admin trails, the current page in the gather
  and alias pagers, and all 18 links in the admin sidebar, where it sits inside
  the same condition that already emitted `class="active"`.

  The regression test is the general one rather than a list of expected labels: it
  renders nine pages, extracts every `<nav>` opening tag, and fails on any that
  carries neither `aria-label` nor `aria-labelledby`. A new template cannot
  quietly reintroduce the defect.

- `trovato_scolta` is removed, and the AI search endpoint namespace is settled on
  the routes the kernel actually serves.

  The plugin could not function as shipped, in three independent ways. Its routes
  were declared page-style with callbacks and no `tap_api` export, so
  `routes/plugin_api.rs` never registered them. Its three worker functions carried
  `#[allow(dead_code)]` comments claiming route callbacks called them at runtime,
  which was not true. And it declared `host_interfaces = []` while calling
  `ai-api`, so the WASM-1 linker would have refused it even if the routes had
  registered.

  Deleted rather than rebuilt: `crates/kernel/src/routes/api_search.rs` already
  serves query expansion, summarization and follow-up at `/api/v1/search/*`, so a
  rebuild would have been a second implementation of a working feature. 1.0 should
  not ship a plugin that cannot serve a request.

  The namespace question is settled the same way. `static/js/scolta.js` defaulted
  to the plugin's `/api/scolta/v1/*` paths, so any page relying on the defaults got
  a 404; the search page worked only because it overrode all three endpoints. The
  client now defaults to the kernel's paths, and the search page no longer restates
  them — which is what keeps the defaults honest, since a drift can no longer hide
  behind an override.

  Root cause, addressed: nothing failed loudly when a declaration had no consumer.
  A `callback` is only dispatched when `handler_type` is `"api"`, so a page-style
  entry naming one registers nothing, and both `trovato_feeds` and
  `trovato_scolta` were dead in exactly that way without a single warning at build
  time, plugin load or first request. `plugin_api::unreachable_callbacks` now finds
  those entries and startup logs each one.

- The registration mode in site configuration is the setting the register route
  reads. It used to be a no-op.

  The register route gated on the boolean `allow_user_registration`, default
  false. The admin site-config form offered a three-mode `user_registration`
  selector and saved it, but the only reader of that key was the same form
  re-rendering itself — so choosing "Open" changed nothing, and the only way to
  open registration at all was a config import of the boolean.

  `user_registration` is now the one key, read by the route through
  `RegistrationMode`. The boolean is still honoured as a fallback when no mode is
  stored, so a site that opened registration the only way it could keeps working
  across the upgrade; the first save from the admin form deletes it, leaving one
  setting that cannot contradict another.

  Two modes rather than three: "admin only" and "closed" differed in wording
  only. Both close the public register route, and neither can stop an
  administrator creating an account — the one account-creation path that has to
  keep working. A stored `closed` still reads as closed to the public, so no
  site's behaviour changes. A genuine third mode would be registration *with
  approval*, which needs an approval queue rather than a third label.

  An unparseable mode resolves to closed. For registration, the safe direction
  for a value nobody can parse is "not open".

- `KNOWN-ISSUES.md` no longer claims menus have an admin screen, and the missing
  form is on the 1.0 list.

  The line listed content types, fields, users, categories, content, gather
  queries, tiles, aliases, **menus**, plugins and AI providers as all having admin
  screens. There is no menu admin screen: no route under `/admin` matches `menu`,
  and `templates/admin/` holds no menu template. Menu links are rows in
  `menu_link`, read by the render layer and written only by config import — the
  same position roles, stages and system configuration are in, which that section
  is about.

  Menus therefore move into the import-only list, and into
  "The remaining admin screens" in `ROADMAP.md`, before 1.0 rather than after.
  That follows the project's own criterion for 1.0 — a site can be built,
  configured and operated through the interface — and a site's navigation is not
  an advanced feature. The `menu_link` row already carries everything a form would
  edit, so the work is the form and its route, not the model.

  A documentation defect drifts back unless something pins it, so the correction
  has a test: it asserts no menu admin path is served, that `KNOWN-ISSUES.md` does
  not list menus among the types that have screens, and that `ROADMAP.md` places
  the form before 1.0. It fails in both directions — build the screen and it tells
  you to update the docs and delete it.

- AI search query expansion is cached, so the same query stops re-billing the
  provider.

  Every call to `/api/v1/search/expand` went to the configured LLM provider.
  `docs/design/search-architecture.md` specified an expansion cache ("the same
  query always produces similar expansions") and none was built, so two people
  searching the same phrase paid twice, as did one person searching it twice.

  Expansions now go through the kernel's two-tier cache, with the TTL in
  `CACHE_TTL_SEARCH_EXPAND` — 30 days by default, because an expansion is a set
  of related terms that costs tokens to produce and does not go stale the way
  content does. `0` disables the cache, which is the switch to reach for while
  tuning the prompt. The design doc's older `3600` suggestion is annotated with
  what shipped, so the two do not disagree.

  The key is a hash of the *normalized* query plus the site name and slogan.
  Normalizing (trim, collapse whitespace, lowercase) means "  Rust   Async " and
  "rust async" are one entry. Including the site name and slogan matters because
  the prompt is built from them: a renamed site must not be served expansions
  produced under its old name. The parts are length-prefixed before hashing so no
  two different triples can produce the same key, and the query is hashed rather
  than interpolated because a query is arbitrary user text and a cache key is a
  Redis key.

  An empty term list is not cached: that is a parse failure, not an answer worth
  remembering for a month.
- Netgrasp is gone from this tree. It now lives in its own repository,
  `jeremyandrews/netgrasp-trovato`, where it builds against `trovato-sdk` as a
  pinned git dependency and installs by appending its own directories to
  `PLUGINS_DIR`, `TEMPLATES_DIR` and `STATIC_DIR`. Nothing about Trovato had to
  change for that to work, which is the point: an application built on Trovato
  should need no space inside it.

  Removed: `plugins/netgrasp`, `crates/netgrasp-core`,
  `crates/kernel/tests/netgrasp_sync_test.rs`, `docs/netgrasp-validation.md`,
  and the two `templates/gather/query--ng_*.html` theme templates. The workspace
  members, the Dockerfile's plugin build list, `scripts/pre-commit-check.sh`,
  CI's wasm build and copy steps, and the plugin list in `INSTALL.md` lose their
  netgrasp entries with them.

  The integration test was the one piece that was not a straight deletion: it
  drove the real netgrasp module through the real `TapDispatcher`, `ItemService`
  and `GatherService`, so deleting it would have dropped the only coverage
  anywhere that exercised a compiled plugin against a live host. It moved to the
  netgrasp repository rather than being discarded, where it consumes
  `trovato-kernel` as a dev-dependency pinned to the same revision as the SDK.

  Also renamed four `QueryDefinition`/`QueryDisplay` deserialization tests and
  one `info_parser` fixture that used netgrasp's names for their sample data.
  They test the kernel and always did; borrowing a downstream application's type
  names to do it made this repository look like it knew about that application.

- The WebAuthn registration tests pass on a database they have already run
  against. `webauthn_registration_test.rs` created its fixture users under fixed
  names, and `create_test_user` upserts on `LOWER(name)`, so every run resolved
  the same user row and inherited the previous run's credentials and audit
  events. Two per-user assertions are exact counts (one `passkey.registered`
  event, two credentials for the multi-passkey account), so the first run passed
  and every run after it failed until the database was recreated. CI provisions a
  fresh database per run and never saw it; a local `cargo test --all` paid for it
  every time. Each test now derives a fresh username per run, and its email from
  that name, so it only ever counts rows its own actions produced. Removed the
  corresponding `KNOWN-ISSUES.md` entry.

- `config import` no longer reports success over files it could not apply, and
  the tutorial's own config set now imports clean. Two halves of one defect.

  **The reporting half.** Import walked the config directory applying whatever
  parsed. A file that failed — malformed YAML or content that no longer matched
  its entity's schema — became one line in a warning list, did not stop the run,
  and did not change the exit code. `trovato config import <dir>` on a set with
  one typo printed "Imported N config entities" and exited 0. Roles and stages
  have no admin form (`KNOWN-ISSUES.md`), so for those two types import is the
  only management path and a skipped file was an entity that never arrived with
  nothing saying why.

  Import now validates before it applies. Every file is read, parsed and schema
  checked, and every reference it makes (a tag's category, a search field
  config's bundle, a tag's parents) is resolved against the import set and then
  the database — all before the first write. If anything fails, the run returns
  `ConfigImportFailed` naming every offending file with its reason, the CLI exits
  non-zero, and **nothing is written**: not even the valid files sitting next to
  the bad one, so a config set is atomic with respect to bad input. `--dry-run`
  runs exactly that validation and skips only the writes, which makes it a real
  preflight instead of a report of what would have been attempted. A failure in
  the save pass is also an error rather than a warning; those writes are not in
  one transaction (`ConfigStorage` is a trait over backends), so earlier writes
  stand and the fix is to re-run, but the exit code no longer says success.

  The failure messages also carry their cause now. `{e}` on an `anyhow::Error`
  prints only the outermost context, so every one of the tutorial's 18 failures
  read "invalid tile YAML" or "invalid stage YAML" with no hint of what was
  wrong; `{e:#}` appends the chain, so the same failure now names the missing
  field.

  What stayed a warning, deliberately: a file the config set does not claim (an
  unrecognized filename prefix, a symlink, an oversized file), a filename whose
  ID disagrees with its content, and a duplicate entity ID. These are advisory
  observations about the directory rather than a recognized config file that
  cannot be applied, and promoting them would hard-fail directories that are
  working today.

- The tutorial's config set no longer drifts from the config schemas. 18 of its
  76 files did not parse — every stage, role, tile and menu link — so
  `config import docs/tutorial/config` applied 58 and skipped the rest while
  reporting success. Each was missing required fields: `id` on all of them,
  `machine_name` on stages, `stage_id` and the timestamps on tiles and menu
  links. The files were repaired against the current schemas; no schema was
  loosened to accept them.

  They were also renamed. Import names an entity's file `{entity_type}.{id}.yml`
  where the ID is the entity's own identifier, and roles, stages, tiles and menu
  links are keyed by UUID, so `stage.incoming.yml` disagreed with its content and
  would have warned on every import. They now use the UUID that `config export`
  would write, drawn from the `0193a5a0-` family the stage seeds already use, so
  the directory is what an export produces and re-importing it round-trips.
  `docs/tutorial/config/README.md` is a new index mapping each UUID back to its
  machine name, and the tutorial and recipe text that referenced the old names
  was updated — including two recipe steps that told the reader roles and stages
  were "not importable" and had them create both by hand, which after the repair
  would collide with what the import creates.

- `DirectConfigStorage::save_stage` now honors the UUID a stage's config file
  declares, and its update path works at all. Two bugs found by making the
  tutorial's stage files import:

  The create path called `Stage::create`, which generates its own `Uuid::now_v7()`
  and ignored the `id` in the file. So a stage landed under a UUID nobody
  declared, export would not round-trip it, and a **second import failed**: the
  lookup by declared id still missed, so it tried to create the stage again and
  collided on `stage_config.machine_name`'s unique constraint. `create` now
  delegates to a new `Stage::create_with_id`, and config import passes the file's
  id.

  The update path ran `UPDATE stage_config ... WHERE stage_id = $4`, and that
  column is named `tag_id` — it has been `tag_id` since
  `20260225000002_create_stage_config.sql` and was never renamed. Every update of
  an existing stage therefore failed with "column stage_id does not exist",
  which importing the well-known Live stage hits immediately. `delete_stage` had
  the same wrong column. Fixed both, and the update now also writes
  `category_tag`'s label, description and weight inside a transaction with the
  `stage_config` update; it previously touched only `stage_config`, so
  re-importing a relabelled or reweighted stage silently did nothing.

- `config export` no longer fails on a database that contains a tag.
  `DirectConfigStorage::fetch_all_tags` selected seven of `Tag`'s eight columns,
  omitting `slug`, so `query_as::<_, Tag>` failed at row decode with "no column
  found for name: slug" and the command exited non-zero. The column has existed
  since `20260307000001_add_category_tag_slug.sql`; only this one query never
  asked for it, and it is the only `FROM category_tag` select in that file, so
  nothing compensated.

  The blast radius was every real site: stages are rows in the `stages` category,
  so a database has a tag as soon as it has stages, and `config export` was
  therefore broken for anyone with anything to export. `config import` was
  unaffected, because it never reads tags back, which is why an application could
  install from files while being unable to write them back out.

  Existing tag coverage missed it for a specific reason. `list("tag", filter)`
  routes a `category_id` filter to `Tag::list_by_category`, which does select
  `slug`; only the unfiltered path that export uses was broken, and the tag test
  listed by category. The regression test exports a database holding a tag with a
  slug set and asserts on the exported file's contents, so neither dropping the
  column from the struct nor restoring the old select can pass it.

- The admin record view route now honors the key column a record type actually
  declares. `[[record_types]]` lets a plugin name its own `id_column`, and every
  other reader trusts it — the list route projects `{id_column}::text`, gather
  resolves the logical `id` to it — but the view handler extracted the path
  segment as `Path<(String, uuid::Uuid)>`. Axum parses that before the handler
  body runs, so a record type keyed by a bigint (or any other scalar) listed
  correctly and returned **400 on every row link**, with the guard and the
  registry lookup never reached. The route was hard-coding the id type in the one
  place that did not need to assume it, one line above the query it built from
  `def.id_column`.

  The id is extracted as a string and compared as text, `WHERE {id_col}::text =
  $1`, with the segment bound as a parameter — as injection-safe as before, since
  the column name is still the registry-validated identifier and the value is
  still bound. One path serves a uuid key, a bigint key and any other scalar key,
  with no per-type branching and no registry addition. A uuid-shaped segment is
  normalized to the canonical lowercase form Postgres renders `uuid::text` as, so
  the uppercase, braced and unhyphenated spellings the uuid extractor accepted
  still open their row. A miss renders not-found rather than erroring, including
  for a segment that is not a number at all on a bigint-keyed type, and
  `require_admin` still gates the route.

  The reference plugin `trovato_record_ref` grew a second record type,
  `legacy_record` over a BIGINT-keyed table, so the non-uuid case is exercised
  end-to-end through the real router and the real registry rather than asserted
  in the abstract.

- `Config::from_env` is now the only place in the process that reads the
  environment. Twelve settings were read lazily and repeatedly at the point of
  use instead: the static search path on every served file, the CSP headers on
  every response, the cron key on every call to `/cron/{key}`, the tenant
  strategy and the slow-request threshold on every request, the security-audit
  retention window on every prune, and the gather over-fetch bounds through
  `LazyLock` statics that froze on first touch. Reading configuration at the
  point of use has three costs, and this pays off all of them: the work is
  repeated (a `HeaderValue` was rebuilt and re-parsed per response, a search path
  re-split per file), the value cannot be steered by a caller at all, and the
  only way for a test to reach it was to mutate a process-global.

  Values request handling needs travel on a new `RuntimeConfig`, carried on
  `AppState` and reachable as `state.runtime()`. It is kept separate from
  `Config` deliberately: `Config` holds `database_url`, `smtp_password` and
  `jwt_secret`, and `AppState` is handed to every request handler. Startup-only
  settings — `templates_dirs`, `trusted_proxies`, `jwt_secret`,
  `shutdown_timeout` — became `Config` fields consumed where they already were.
  Each group of settings resolves through a `from_lookup` constructor owned by
  the module it configures (`SecurityHeaders`, `TenantResolution`,
  `GatherAccessConfig`), so every setting name and documented default is
  assertable from an explicit map.

  Two fixes fell out of the consolidation. An unusable `CSP_REPORT_URI` — one
  with a newline or a control character — used to leave the response with **no
  CSP at all**, because the `HeaderValue::from_str` failure skipped the insert
  entirely; it now logs and serves the policy without the report endpoint. And
  `QUERY_SLOW_THRESHOLD_MS` multiplied by five without a guard, so a large
  configured threshold could overflow; the comparison saturates.

  Behaviour is otherwise unchanged, including every default and the precedence
  of an explicitly set variable. `PATH`, the per-provider AI key variable, and
  `site_config`'s `env:` secret references still read the environment on use, and
  correctly so: in each the *variable name* is runtime data, not configuration.

- Tests no longer mutate the process environment without a guard. Seven places
  called `std::env::set_var` / `remove_var` from code running on a live libtest
  thread pool: the environment is process-global, `cargo test` runs a binary's
  tests in parallel, and `setenv` is not thread-safe against the `getenv` that
  dependency C code performs for timezone lookups and name resolution. None of
  the mutating tests restored what they found unless every assert passed, so one
  failure leaked state into every test that ran afterwards, and the SAFETY
  comments justifying the `unsafe` blocks asserted a "before any threads are
  spawned" guarantee that libtest does not provide.

  The fix is mostly to remove the need. `PluginConfig::from_env`,
  `audit::retention_days_from_env` and `config::split_search_path` are now
  one-line edges over `from_lookup`, `retention_days_from` and
  `split_search_path_value`, which take their input as a parameter; their tests
  drive those cores with explicit values and touch nothing global, and they
  cover the defaults and the unparseable-value fallbacks that the old
  environment-reading tests could not state (asserting a default by *assuming*
  a variable was unset failed spuriously on any machine that exported it). The
  integration fixtures set `Config` fields — `plugins_dirs`,
  `database_max_connections` — instead of the variables those fields are loaded
  from. What legitimately remains goes through one mechanism for the whole
  workspace, `trovato_test_utils::env`: a single lock serializing every
  mutation, an `EnvGuard` that restores what it found when it drops (including
  while a failing assert unwinds), and `load_dotenv` so that `dotenvy`'s own
  `set_var` calls take the same lock. Every SAFETY comment now states only what
  is actually guaranteed, and names the part that no test-side lock can close.

  No runtime behaviour changes. The variables still read lazily and repeatedly
  deep inside runtime call paths — the cron key, CSP headers, tenant
  resolution, the query profiler threshold, `TEMPLATES_DIR`, `STATIC_DIR`,
  `TRUSTED_PROXIES` — are why a fixture has to reach for the environment at
  all; consolidating them into the startup config is the follow-up.

- `STATIC_DIR` is a search path, like `PLUGINS_DIR` and `TEMPLATES_DIR`.
  0.99.0 generalized two of the three asset roots and left the third a single
  directory, so an application could overlay its templates but not the CSS
  those templates reference: shipping a stylesheet meant writing into the one
  `./static`, which is the kernel tree the overlay exists to leave alone.
  Several directories separated by the platform separator are now read in
  order, a later one overrides an asset of the same path, and the asset
  manifest hashes the file that is actually served. A single-directory value
  is unchanged in behaviour. The generated Pagefind index needs one
  destination and is written to the first directory.

  ```
  STATIC_DIR=./static:/path/to/app/static trovato serve
  ```

- The site front page can be any path the site serves. `site_front_page` was
  parsed as `/item/{uuid}` and nothing else, so every other value — a gather
  alias, a plugin route, an aliased path — silently fell back to the promoted
  listing, and the home page was the one route on the site that could not be
  any route. An item path still renders inline at `/`; any other path
  redirects (307) to itself, carrying the query string, so the handler that
  owns the route serves it and the front page needs no knowledge of route
  types. The path must be local (absolute, no scheme, no host); the admin form
  rejects a non-local value with the same rule that serves it.
- The front page listed only promoted items that happened to be among the ten
  most recent published items: it fetched a fixed page of published items and
  filtered it in memory. Promotion now decides the query
  (`Item::list_promoted`, with paging), so a promoted item appears however many
  newer published items precede it.
- An item front page was access-checked against a hard-coded permission list
  rather than the viewer's real permissions, so anonymous visitors were handed
  none at all and a configured item front page fell through to the promoted
  listing on a default install (whose anonymous role does have "access
  content"). The front page now builds the same user context as the item
  route.
- Navigation hid every permission-gated menu item from everyone. The menu was
  built as `root_menus().filter(|m| m.permission.is_empty())`, which reads as a
  permission check and is not one: it kept only the entries needing no
  permission, so an entry declaring one was dropped for every viewer, including
  a viewer who held it and an admin. A plugin could not get a gated navigation
  entry into the menu at all — Argus's `/stories` and `/articles`, both gated on
  `access content`, were unreachable from the navigation for everyone.
  `MenuRegistry::root_menus_for` now filters per viewer: an entry appears when
  it requires no permission, when the viewer holds the one it requires, or when
  the viewer is an admin, matching `require_permission`.
- Admin contexts are loaded rather than fabricated. `admin_user_context`
  returned `authenticated(user.id, vec!["administer site"])`, dropping the
  admin's real permissions across eleven admin handlers, and the admin AJAX
  callback hard-coded the same list. Both worked only because `is_admin()`
  short-circuits every permission check — the same fabricate-instead-of-load
  shape that broke the item front page, one short-circuit from the same
  failure. Every request-scoped context now comes from one loader:
  `PermissionService::user_context` assembles it, `routes::helpers`
  `get_user_context` (from a session) and `user_context_for` (from an
  already-loaded `User`) are the two sanctioned ways to reach it, and the
  hand-rolled copies in `routes::comment` and the MCP server were folded into
  it.
- Self-service updates pass the owner's real permissions as the acting
  principal. Profile update, password change, email-change verification and
  password reset built `authenticated(id, vec![])`. Those service methods
  authorize nothing — the route gates on identity or a verified token — but the
  context is dispatched to plugins as the `tap_user_update` principal, so an
  empty list told every listener that a permission-less user acted. Documented
  on `UserService::update` and `update_password`: `acting_user` is the tap
  principal, not an authorization subject.
- A failed permission load no longer degrades a request silently. Both loaders
  ended in `unwrap_or_default()`, so a transient database error produced an
  empty permission set — safe, but indistinguishable from a permission model
  that is working, which is exactly how the front-page bug presented. The
  policy is now explicit per caller: paths that can propagate fail closed
  (`PermissionService::user_context`, the MCP server), and the web loader
  degrades to the deny-all set while logging at ERROR and incrementing
  `trovato_permission_load_failures_total`, which should alert on any non-zero
  value.
- wasmtime 43 → 47.0.3. Clears every outstanding wasmtime and cranelift
  advisory (RUSTSEC-2026-0085 through -0096, -0114, -0222); 14 suppressions
  removed from `.cargo/audit.toml`. No source change was required and the
  plugin ABI is unaffected: a plugin binary built against the pre-freeze SDK
  still loads and runs.

## v0.99.0 — 2026-08-16

First public release. Pre-1.0: the plugin contract is frozen, the CMS is not
finished. See `KNOWN-ISSUES.md` and `ROADMAP.md`.

### Versioning

- One version for the whole project. Every crate inherits `version.workspace`;
  every plugin manifest carries the project version; the plugin API is that
  version without the patch component (`0.99.0` → `(0, 99)`,
  `api_version = "0.99"`). Replaces four independent version tracks. See
  `docs/design/Versioning.md` and `docs/design/version-map.md`.
- Added `trovato --version`.
- OpenAPI `info.version` now reads the build version instead of a literal.

### Licensing

- Dual licensed MIT OR Apache-2.0. Replaces the `GPL-2.0-or-later` declaration
  used during private development; nothing was published under it.

### External applications

- `PLUGINS_DIR` and `TEMPLATES_DIR` are search paths: several directories
  separated by the platform separator, read in order, later wins a name
  collision. A single-directory value is unchanged in behaviour.
- Templates from all roots load into one Tera instance, so an application
  template can extend a kernel template.
- Plugin name collisions across the search path are logged.
- Additive; no plugin ABI change.

An application now installs from its own repository, adding no file to the
kernel tree:

```
PLUGINS_DIR=./plugins:/path/to/app/plugins \
TEMPLATES_DIR=./templates:/path/to/app/templates \
trovato serve
trovato config import /path/to/app/config
```

---

Entries below predate the first public release and use private development
version numbers. No release was published under them.

## v0.2.0-beta.2 — 2026-04-09

### Infrastructure Hardening

- **Structured Error Handling**: 12-variant `AppError` with JSON `ErrorResponse` (machine-readable codes, request IDs, per-field validation details). PostgreSQL errors classified by code (23505 → 409 Conflict, 23502 → 422). All 160+ route handlers migrated from ad-hoc tuples.
- **Circuit Breakers**: AI provider (3 failures/60s recovery), email SMTP (5/30s), S3 storage (3/30s). States visible in `/health` and `/metrics` (Prometheus gauges).
- **Graceful Shutdown**: SIGINT/SIGTERM handling with configurable drain timeout (`SHUTDOWN_TIMEOUT_SECS`). Background tasks use `CancellationToken` for coordinated exit.
- **DB Pool Monitoring**: 4 Prometheus gauges (size/idle/active/max). `/health` includes pool utilization. Background task warns at 80%+ utilization.
- **Concurrent Plugin Loading**: WASM compilation via `tokio::task::spawn_blocking` per plugin. Failed plugins logged and stored for admin visibility instead of aborting startup. Mtime tracking for reload optimization.
- **Async Plugin Discovery**: Directory scanning uses `tokio::fs`. All callers updated.

### CMS Product Features

- **Admin Site Configuration UI**: `/admin/config/site` with site name, slogan, email, front page, items per page, registration mode, SMTP settings, and notification preferences. Test email button for SMTP verification.
- **Email Template System**: 4 template pairs (HTML + plain text) for registration verification, password reset, comment notification, and admin new-user alerts. Multipart sending via `send_templated()`.
- **Email Notifications Wired**: Comment creation notifies content author (background task, skips self-notifications). User registration notifies admin when enabled in site config.
- **Media Browser**: `/admin/media` grid view with type/search filters, pagination. `/api/v1/media/browse` JSON API. JavaScript media picker modal with browse/upload tabs, drag-and-drop, integrated into content edit form file fields.
- **SEO Plugin** (`trovato_seo`): Meta description, Open Graph tags, JSON-LD structured data (Article, Event, FAQPage schema types with speakable property), sitemap.xml with URL alias resolution, robots.txt with AI crawler management (GPTBot, ChatGPT-User, ClaudeBot, Google-Extended, Bytespider, CCBot, PerplexityBot, Amazonbot).

### Versioning & Release

- **Plugin API Versioning**: `api_version` field in all 25 plugin manifests. `KERNEL_API_VERSION` constant. Compatibility check at install and enable time (major must match, minor must be <=).
- **Workspace Version**: All core crates inherit version from `[workspace.package]` (single-line bumps).
- **Nightly Auto-Increment**: Docker versions increment per commit (counts commits since tag).
- **Versioning Documentation**: `docs/design/Versioning.md` with full strategy.

### Search & AI Improvements

- **Rich Search Results**: Pagefind index includes description, location, event dates, content type metadata. Friendly URL aliases. Type badges, dot-separated metadata chips in result cards.
- **Chatbot RAG Enrichment**: Loads actual items from DB after search — context includes all JSONB field values (dates, locations, descriptions, URLs) instead of just title + snippet.
- **Chat Formatting**: `formatChat()` renders markdown (bold, numbered lists, bullet points) in chat widget.
- **Auto-Search**: Scolta.js reads `?q=` URL parameter and searches on page load.

### Rate Limiting

- **Per-IP Rate Limiting**: Middleware wired into request chain (before session resolution).
- **Per-User Rate Limiting**: Second middleware layer after authentication, keyed on user ID.
- **Bug Fix**: `/user/register` now correctly maps to the `register` category (3/hr) instead of `login` (5/min).

### Test Coverage

- **924 total tests** (up from 754 in beta.1): 829 kernel, 14 SEO plugin, 81 MCP server.
- New coverage: middleware (rate limit categories, client ID extraction, security headers), error system, circuit breakers, route helpers, email templates, content filters, MCP tools/resources/server.

### Tutorial & Documentation

- Fixed plugin names in tutorial Parts 5-7 (block_editor → trovato_block_editor, .wasm path → machine name in install commands).
- Pre-commit hook (`.githooks/pre-commit`) runs fmt + clippy automatically.
- `scripts/pre-commit-check.sh` with `--quick`, standard, and `--full` modes.

---

## v0.2.0-beta.1 — 2026-04-04

First public beta release.

### Core CMS

- **Content Types**: Dynamic content types with custom fields (Text, TextLong, Integer, Float, Boolean, Date, Email, File, RecordReference), JSONB field storage, revision history
- **Gather Query Engine**: 18 filter operators (Equals, Contains, HasTag, HasTagOrDescendants, FullTextSearch, SemanticSimilarity, etc.), configurable pagination, exposed filters, NULLS FIRST/LAST ordering
- **Categories**: DAG hierarchy with multiple parents per tag, recursive ancestor/descendant queries, slug-based routing
- **Full-Text Search**: PostgreSQL tsvector with configurable field weights, GIN indexes, integrated as gather filter
- **URL Aliases**: Clean URLs with middleware-based resolution, automatic pathauto generation from title patterns
- **Redirects**: URL redirect management with automatic alias-change tracking and loop detection
- **Content Staging**: Stage hierarchy with parent/child chains, upstream publishing, content overlay inheritance
- **Block Editor**: Editor.js with 8 block types (paragraph, heading, image, list, quote, code, delimiter, embed), server-side rendering with ammonia sanitization, syntect syntax highlighting
- **Forms**: Declarative Form API with validation, AJAX multi-step support, CSRF protection
- **Config Import/Export**: YAML-based with 13 entity types (item_type, item, role, stage, tile, menu_link, category, tag, gather_query, url_alias, language, variable, search_field_config)
- **File Uploads**: Magic byte validation, filename sanitization, MIME type checking, image style derivatives

### Plugin System

- **WASM Sandboxing**: 25 plugins compiled to WebAssembly, running in per-request Wasmtime sandboxes with pooled allocation (~5us instantiation)
- **Tap System**: 40+ named extension points for content, forms, access control, menus, permissions, cron, search, AI, and chat
- **Host Functions**: All fully implemented — database (parameterized queries, DDL guards), item CRUD, cache (Moka L1 + Redis L2), variables (persistent key-value via site_config), AI requests, HTTP, crypto (SHA-256, HMAC-SHA256, random bytes, constant-time comparison), user context, logging, queues
- **Plugin Namespace**: All core plugins use `trovato_` prefix convention. Standalone projects (argus, netgrasp, goose) and ritrovo_* plugins retain their own namespaces
- **Plugin CLI**: `trovato plugin list|install|migrate|enable|disable|new`

### Standard Plugins (25)

**Content Types**: trovato_blog, trovato_media, argus (7 types), netgrasp (6 types), goose (5 types)

**Features**: trovato_categories, trovato_comments, trovato_audit_log, trovato_content_locking, trovato_scheduled_publishing, trovato_webhooks, trovato_image_styles, trovato_oauth2, trovato_redirects, trovato_block_editor

**i18n**: trovato_locale, trovato_content_translation, trovato_config_translation

**AI**: trovato_ai (field rules, form assist, chat actions), trovato_search

**Reference App**: ritrovo_importer, ritrovo_cfp, ritrovo_access, ritrovo_notify, ritrovo_translate

### AI Integration

- **Provider Registry**: OpenAI-compatible and Anthropic protocols, secure key store (env vars, never in DB or WASM)
- **`ai_request()` Host Function**: Single function for Chat, Embedding, ImageGeneration, SpeechToText, TextToSpeech, Moderation
- **Token Budgets**: Per-user, per-role tracking with configurable daily/weekly/monthly periods
- **Field Rules**: Automatic content enrichment on save via `tap_item_presave` (fill_if_empty, always_update behaviors)
- **Form AI Assist**: Inline rewrite/expand/shorten/translate/tone buttons on text fields via `tap_form_alter`
- **Chatbot**: SSE streaming chat with RAG context from search, session history, configurable system prompt
- **MCP Server**: Content CRUD, search, Gather, categories, content_types tools + resources for external AI tools
- **VectorStore**: Trait with PgVectorStore implementation, SemanticSimilarity gather operator, graceful degradation without pgvector

### Security

- **Authentication**: Argon2id (RFC 9106 params), Redis sessions, account lockout, session fixation protection
- **OAuth2**: Authorization code (PKCE), client credentials, refresh token grants with JWT
- **Security Headers**: CSP, X-Frame-Options: DENY, HSTS, X-Content-Type-Options, Referrer-Policy, Permissions-Policy
- **Crypto Host Functions**: SHA-256, HMAC-SHA256, secure random bytes, constant-time comparison for WASM plugins
- **SSRF Prevention**: DNS rebinding mitigation, private IP blocking, port restrictions
- **File Upload**: Magic byte validation, filename sanitization, MIME allowlist, executable rejection

### Accessibility

- Skip links, main landmark, focus-visible CSS, visually-hidden utility
- 8 ARIA helpers on ElementBuilder (SDK): aria_label, aria_describedby, aria_hidden, aria_current, aria_live, role, aria_expanded, aria_controls
- Form elements with aria-describedby, aria-invalid, role="alert" on error summaries
- Flash messages with role="status" and aria-live auto-announcement
- Admin tab navigation with full WAI-ARIA (role="tablist/tab", aria-selected, arrow key navigation)
- Correct heading hierarchy (h1 > h2) across all templates

### Internationalization

- Language negotiation: URL prefix, Accept-Language, session, site default
- RTL support: 15 language codes, text_direction_for_language(), dir attribute on html
- Locale-aware date formatting: 14 locale patterns (en, de, fr, es, it, ja, zh, ko, ar, he, pt, nl, ru, pl)
- Interface translation: Gettext .po import, in-memory cache, Tera `t()` function
- Content translation: Field-level overlay with language fallback
- Config translation: Translatable configuration entities

### Infrastructure

- **Two-Tier Cache**: Moka L1 (in-process) + Redis L2 with tag-based invalidation
- **Multi-Tenancy**: Tenant schema, middleware, tenant_id on all content tables, zero-overhead single-tenant default
- **API**: Versioned router (/api/v1/), X-API-Version header, paginated ListEnvelope, route metadata registry
- **Docker**: Nightly images published to ghcr.io on every push to main. Three workflows: native dev, dev container, pre-built runtime
- **CI**: 8-job pipeline — Security Audit, Build, Clippy, Doc Check, Test, Format, Terminology Check, Coverage

### Known Limitations

- **Search**: Full-text search works; semantic search (query expansion via AI, faceted search) not yet implemented
- **Performance**: No formal benchmark suite or profiler integration
- **Nightly Images**: amd64 only; arm64 images published with release tags. ARM users can build locally
- **RecordReference Autocomplete**: Works but searches published items only (no draft item search)
