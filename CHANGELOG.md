# Changelog

## Unreleased

- Language-aware navigation, and the metadata a translated page needs.

  A site serving `/it/why` rendered its menus with default-language addresses,
  offered nothing a language switcher could be built from, and emitted no
  `hreflang` alternates. Every fact needed for all three was already in the
  kernel; nothing assembled them, so sites worked around the gap with a metadata
  field and a hardcoded path map in their templates.

  **`requested_path`** joins `current_path` in the render context: the address as
  asked for, before a language prefix was stripped and before an alias was
  resolved. `current_path` is what the request was rewritten *to* — on an aliased
  item that is `/item/{uuid}`, which no menu link can ever equal, so the active
  trail was dead on every aliased page. `current_path` is unchanged; the stock
  navigation now matches on `requested_path`, which falls back to it.

  **`available_translations`** on an item page: `{language, path}` for every
  language the page exists in, default included, the default language's canonical
  alias verbatim and every other language that same address behind a `/{lang}`
  prefix. One query for existence.

  **Menus follow the language.** When the active language is not the default, a
  menu link whose target has a translation gets a `/{lang}` prefix on its path,
  and its title replaced by the translated title when the link's title is the
  target's default-language title — a label somebody wrote by hand is left as
  written. A link whose target has no translation is untouched: a translated label
  on an untranslated page is a promise the click breaks. Two queries for a whole
  menu, not two per link.

  **`hreflang_links`** is populated at last. `build_hreflang_links` existed and
  was reachable only from its own tests; it now takes the languages a page
  actually exists in rather than every language the site knows, because an
  `hreflang` naming an address that 404s is worse than no tag. `base.html` already
  emitted the tags.

  Nothing here touches the plugin SDK.

- Fix: the language a page is served in reaches the page.

  `inject_site_context` inserted `active_language` and `text_direction`
  unconditionally, while its own comment said route handlers could override them.
  That made the *order* of a route's inserts decide the outcome, which is not a
  contract: `routes/gather.rs` inserted after the call and was right,
  `routes/item.rs` inserted before it and was silently overwritten with the site
  default. Every translated item page therefore rendered `<html lang="en">`
  whatever language it was actually served in, so a screen reader read Italian
  prose in an English voice (WCAG 3.1.1) and no template could branch on the
  language.

  The helper now fills these in only when the context does not already carry
  them, and its doc comment states the contract: the route's value is
  authoritative, the site default is a fallback. Both insert orders are correct
  from here, and both are exercised by tests.

  The front page had the same symptom from a different cause: `routes/front.rs`
  rendered the configured front-page item without ever calling
  `apply_translation_overlay`, so a translation aimed at the front page was
  configuration nothing read. It applies the overlay now, with the same
  field-access re-filter the item route does after one, and records the
  negotiated language on the page.

## v0.102.0 — 2026-08-25

An assistant a plugin can be configured through, and the tool calling the kernel
needed before one was possible.

Trovato could talk to a model and could never let one *do* anything. The visitor
chatbot streams text; a plugin's `ai_request` returns text; neither built a
`tools` array and neither parsed a tool call back out of a response.
`tap_chat_actions` was declared for exactly this and was never dispatched. So a
plugin that owned real configuration — a network monitor, a mail server, a
firewall — had no way to offer it to a person in words.

The plugin API moves to `(0, 102)`. Everything here is additive: no existing type,
tap, route or host function changes shape, and a plugin declaring
`api_version = "0.101"` still installs and runs. A plugin that wants the assistant
taps must declare `"0.102"`, since a 0.101 kernel does not dispatch them.

- The project version is 0.102.0 and the plugin API is `(0, 102)`.

  The manifest count in `docs/design/version-map.md` moves from 36 to 37 with
  `test_assistant_scope`.

- The AI Assistant: configuring one thing by conversation.

  A person opens `/ai/assistant/{scope}/{scope_id}`, and talks to a model that can
  read the plugin's domain through tools the plugin declared. **Every change the
  model wants to make is a proposal a person has to apply.** That is structural
  rather than a matter of prompting: a write tool is dispatched in `Describe` mode,
  which changes nothing, and the single `Execute` dispatch in the whole kernel is in
  the apply route, reached by somebody clicking Apply on a card they have read.

  A conversation belongs to one person. Every route answers 404 to anyone else,
  administrators included, because a conversation is working notes rather than site
  content.

- Three taps, and a registry that drops rather than fails.

  `tap_assistant_scopes` declares what can be configured, dispatched once at boot
  without services. `tap_assistant_context` describes the thing being configured,
  with services and the caller's real permissions. `tap_assistant_tool` answers one
  call, in `Describe` or `Execute` mode.

  `AssistantRegistry` validates every scope: names match `[a-z0-9_]+`, a scope name
  is unique across all plugins, `parameters` is a JSON Schema object, and there are
  caps on tools, suggestions and prompt size. An invalid scope is dropped with a
  warning and listed on the admin page. One plugin's malformed declaration must not
  stop a site booting.

  `tap_chat_actions` stays declared and is marked superseded in the WIT. It was the
  sketch of a plugin telling the visitor chatbot what it could do; what a plugin
  actually needed was a conversation of its own.

- Tool calling in the provider layer: `services/ai_tools.rs`.

  A third provider path beside the two text-only ones, so neither the chatbot nor
  `ai_request` changes shape. It speaks OpenAI's `tools`/`tool_calls` and
  Anthropic's `tools`/`tool_use`, carries a prior call and its result into a
  follow-up request, and honours the Anthropic rule that every `tool_use` in an
  assistant turn is answered in the very next user turn. Auth, rate-limit, other
  statuses and timeouts map to distinct errors, so a caller can say something honest
  without repeating the provider's body to a person.

- The permission `use ai assistant`, the config key `ai_assistant_config`, and two
  tables.

  `ai_conversation` holds the transcript, with a partial unique index that makes
  "open the assistant for this thing" idempotent. `ai_proposal` holds a write the
  model asked for and nobody has applied, because a proposal has a lifecycle a
  transcript entry does not.

- Four API routes and a page that works without JavaScript.

  `GET /api/v1/assistant/{id}`, `POST .../message` (SSE), `POST
  .../proposals/{id}/apply` and `/discard`, `POST .../reset`. The proposal cards and
  Start over are plain forms carrying a `_token` field, which is what the 0.101 form
  surface was for; `static/js/assistant.js` adds the one thing a form cannot do,
  which is consume the stream a turn produces.

- An admin screen at `/admin/system/ai-assistant`, and a launcher.

  The screen carries the model settings, the limits that bound what a conversation
  can cost, a switch and a prompt override per scope, and the scopes that were
  **dropped at startup and why** — otherwise visible only in a log nobody reads. The
  launcher appears automatically on an item whose type a scope names, and a plugin
  includes the same partial with a literal scope of its own.

## v0.101.0 — 2026-08-19

Three plugin surfaces, and the feature they were for. A visitor can reach a site
owner through `/contact`, which is a plugin because kernel minimality puts a
feature in one, and which could not have been written before this release.

The plugin API moves to `(0, 101)`. A plugin declaring `api_version = "0.100"`
still installs and runs, because the rule is same major and minor at or below the
kernel's; a plugin that wants the `mail` interface must declare `"0.101"`, since a
0.100 kernel does not export it.

- The project version is 0.101.0 and the plugin API is `(0, 101)`.

  `docs/design/version-map.md` item 10 earned its place a second time. A bulk
  substitution of `0.100` to `0.101` set **both** API-compat tests to `"0.101"`,
  which turns "a future minor is rejected" into a test asserting that the kernel's
  own API version requires a newer kernel. The rejected case has to name one minor
  *above* the current one, so it is `"0.102"`. That item exists because this is the
  one place a version bump fails the suite rather than merely reading wrong, and it
  caught the mistake as designed.

  The manifest count in the table moves from 35 to 36 with `trovato_contact`.

- A contact form: `trovato_contact`, a standard plugin serving `/contact`.

  A visitor could not reach a site owner except by a `mailto:` link. Drupal 6
  shipped contact in core, and kernel minimality puts it in a plugin — which could
  not be written until this release, because the three things it needs were all
  missing kernel seams. This is the feature the other three entries were for, and it
  uses all three at once.

  `GET /contact` serves a form with no JavaScript in it, carrying the kernel-minted
  token in a hidden `_token`, rendered into the site's page template. `POST /contact`
  validates, sends through the `mail` interface to the site's configured address, and
  renders a themed confirmation.

  **It stores nothing**, which is why it declares no `db` capability and ships no
  migration. A contact message is delivered, not kept: keeping it would mean a
  moderation queue, a retention policy and a data-export obligation, and a contact
  form should not have opinions about any of those.

  Two details that are decisions rather than details. The plugin **collapses a
  subject containing a newline** instead of letting the kernel refuse it: the kernel
  is right to refuse control characters in a header, and a visitor who pastes a
  subject with a line break has done nothing wrong and should get their message
  delivered. And an invalid submission comes back as **422 with the values still in
  the fields and a fresh token**, because a token is single-use and the one that
  arrived was spent verifying the request that failed validation; without the fresh
  one, a typo would be a dead end.

  It cannot set `Reply-To` — the `mail` interface takes no headers, which is how a
  relay would be smuggled back in — so the visitor's address goes in the body where
  the owner can read it. And a visitor is told a message could not be sent without
  being told why: "SMTP is not configured on this site" is the owner's problem and
  goes to the log.

  `contact_form_test.rs` drives the real wasm end to end: an anonymous visitor loads
  the form, posts it with no header anywhere, and the message arrives at the site's
  address over a real SMTP conversation. A post with no token and a post with a
  forged one are both refused and send nothing. An invalid submission is corrected
  and resubmitted with the fresh token, and lands. Markup a visitor types comes back
  escaped.

  `plugin_surfaces_test.rs` replaces the pinned-gap test from the write-up that
  described these three absences. The assertions are inverted rather than deleted:
  each surface is asserted to exist, and the two theme taps that are **still** not
  dispatched keep a test of their own, so that claim in KNOWN-ISSUES.md cannot go
  stale either.

- A plugin-served page can ask to be rendered into the site's theme.

  The third of the three plugin surfaces KNOWN-ISSUES.md described. `tap_api`
  output was served verbatim, which is right for an admin screen or a JSON
  endpoint and wrong for a page a visitor reaches: it arrived with no site header,
  no navigation and no styling, and a plugin could not supply them. `page.html`
  belongs to the theme, a site may override it, and a plugin cannot know what it
  says.

  An `ApiResponse` now carries `theme` and `title`. With `theme` set the kernel
  treats the body as page *content* and renders it the way an item page is
  rendered: `inject_site_context` for the site context, then
  `ThemeEngine::render_page` for template resolution, so a site that overrides
  `page--contact.html` gets its override on a plugin page too. `ApiResponse::themed`
  and `themed_with_status` are the constructors.

  **Off by default**, which is the whole design: every existing plugin response is
  byte identical, and a test pins that a JSON endpoint keeps its content type and
  its unwrapped body. The kernel still does not sanitize a plugin's HTML, the
  contract every view tap has — theming changes what surrounds a plugin's markup,
  not what it is. A template failure serves the body in a minimal document rather
  than a 500, the same fallback `routes::item` takes: the page is the plugin's work
  and it is better served plain than not at all.

  **The two theme taps are still not dispatched, deliberately.** `tap_theme` and
  `tap_preprocess_item` are a different feature from this one and neither is what a
  plugin page needs to reach the theme. `tap_theme` is `() -> string` with nothing
  consuming what it would return: the Drupal equivalent registers templates, which
  needs template discovery and override resolution the kernel does not have.
  `tap_preprocess_item` would let a plugin alter an item's render context, which
  first needs a decision about which keys it may overwrite — `csrf_token` and
  `user_is_admin` are in that context. Both are recorded in KNOWN-ISSUES.md with
  the reason rather than dispatched with invented semantics.

- A plugin can send email, to the site's own address and nowhere else.

  The first of the three plugin surfaces KNOWN-ISSUES.md described. The kernel has
  had SMTP, templates and a circuit breaker in `services/email.rs` since before the
  freeze, with no seam onto it: a plugin that needed to notify somebody posted to a
  webhook over `http`, which is not email.

  **The recipient is not a parameter.** A host function that sends to an address
  the caller supplies is a spam relay wearing a CMS, and what it puts at risk is
  the site's mail reputation and its SMTP credentials rather than the plugin's own
  data. `send-to-site-contacts(subject, body, attachments)` sends to the address
  the site configured (`site_mail`) and nowhere else: enough for a visitor to reach
  the site owner, which is the case a CMS needs a plugin to cover, and useless for
  reaching strangers. That is also why the interface is not called `send`.

  Delivery goes through the site's own `EmailService`, so a plugin's mail shares
  the kernel's SMTP transport, `from` address and — the part that matters
  operationally — its **circuit breaker**. A plugin cannot configure its own
  delivery and cannot keep hammering a host the kernel has already given up on.
  `RequestServices` gained an `email` handle for this, attached to the one
  `AppState` template, so every plugin shares that breaker rather than getting one
  each.

  Refused, each with its own error code so a plugin can tell the cases apart: no
  SMTP host configured (`ERR_MAIL_NOT_CONFIGURED`), no `site_mail` configured
  (`ERR_MAIL_NO_RECIPIENT` — the `from` address is a transport identity, not a
  contact address, so it is not a fallback), an empty subject or body, **a control
  character in the subject or an attachment's content type**, which is header
  injection and would be the way to smuggle a `Bcc` past the missing recipient
  parameter, an attachment filename carrying a quote or a path separator, more than
  5 attachments, and more than 1 MB of attachment bytes totalled across them rather
  than checked one at a time. An oversized attachment is refused on its encoded
  length, before the kernel allocates its decoded size.

  `EmailService::send_with_attachments` builds a `multipart/mixed` around the text
  part, and delegates to the existing `send` when there are none, so a caller does
  not branch on whether it has any. Attachment bytes cross the WASM boundary
  base64-encoded, because the payload is JSON; the SDK encodes them from plain
  `Vec<u8>` with a hand-written encoder pinned to RFC 4648's own test vectors,
  rather than adding a fifth dependency to the crate compiled into every plugin.

  **The test sends real mail over a real socket.** `plugin_mail_test.rs` runs a
  throwaway SMTP server on a loopback port and reads the conversation: the
  recipient the kernel chose, the `From` header, the body, and the MIME parts
  lettre built. The alternative was a capture mode on `EmailService`, which means
  test-only behaviour inside production code on the path that sends mail to real
  people. Nothing changed shape to be testable.

  What this does **not** do: bound how often a plugin calls it. A plugin-served
  POST already falls into the `forms` rate-limit bucket per IP, which covers the
  web-facing case; a plugin calling from a cron tap is unbounded, and is recorded
  in KNOWN-ISSUES.md rather than half-gated.

- A plugin can serve a form that works with JavaScript switched off.

  This was the second of the three plugin surfaces KNOWN-ISSUES.md described, and
  it needed two halves rather than the one the write-up predicted.

  **Accepting the token.** A state-changing plugin-served request took its CSRF
  token from an `X-CSRF-Token` header only, and a plain HTML `<form>` cannot set a
  header, so a plugin's own form was refused with 403 whatever it rendered.
  `routes/plugin_api.rs` now calls `require_csrf_header_or_field`, which tries the
  header and then a `_token` field from a form-urlencoded body. It is the same
  check either way: both paths end in `form::csrf::verify_csrf_token`, so the token
  stays single-use, session-bound and an hour long. `_token` is the field forty
  templates and every hand-written kernel form already use.

  Two deliberate narrownesses. The body is read **only** when the content type is
  `application/x-www-form-urlencoded`, so a JSON caller that omits the header gets
  the 403 it always got rather than having its body scanned. And a body carrying
  the field **twice** is refused rather than resolved: a `HashMap` of the pairs
  keeps the last, this parser could keep either, and a security check should not
  turn on that choice.

  **Handing over a token to embed.** Accepting the field is useless on its own,
  which the write-up missed: a plugin serving a GET form had no valid token to put
  in it. `tap_api` is one call with no way to ask for one, and `request-context` is
  a plugin-namespaced key/value store. The kernel now mints a token per request and
  passes it in a new `ApiRequest::csrf_token`, additive on a `#[non_exhaustive]`
  struct. Minted for a POST too, because a token is single-use and a submission
  that fails the plugin's own validation needs a fresh one to re-render with. Not
  minted for a bearer-authenticated caller: CSRF does not apply to it, it is not
  being served a form, and minting writes the session store on every request.

  **A form posts back to its own URL, and the registry could not hold that.** Found
  by the test, not by reading: `GET /contact` and `POST /contact` are one path and
  two methods, `MenuRegistry` was a `HashMap` keyed by path, and the second
  registration overwrote the first. A plugin declaring both silently lost one,
  which one depending on declaration order, and the surviving 405 looked like a
  routing bug rather than a lost registration. The registry now keeps every
  registration for the routers to build from (`all`) alongside the path-keyed index
  navigation and page lookup need (`by_path`), and the path-keyed one prefers
  `GET`, so a submit handler cannot displace the page it submits to.

  `plugins/test_plugin_api` grew a public no-JS form, and `plugin_api_test.rs`
  drives the real wasm the way a browser with scripting disabled would: an
  anonymous visitor GETs the form, posts it back with the token in the body and no
  header anywhere, and lands. The refusals are pinned too — a forged token, an
  absent one, an empty one, a spent one, two of them, one minted for another
  session, and a JSON body carrying the field.

## v0.100.0 — 2026-08-19

Thirty-eight pull requests since the first public release. New in this one:
two plugins (`trovato_book` for book-style page trees, `trovato_spam` for AI
comment moderation), administration screens for menus and editorial stages,
account self-deletion, self-service data export, RSS feeds served from gather
queries, comments rendered on item pages, and a notice when a release or a
security release exists.

The plugin API moves to `(0, 100)` because this release adds features rather
than only fixing things. The contract itself is unchanged, so a manifest still
declaring `api_version = "0.99"` installs and runs: the compatibility rule is
same major, minor at or below the kernel's.

- The project version is 0.100.0 and the plugin API is `(0, 100)`, and
  `docs/design/version-map.md` lists three more places that move with it.

  Two of the three are prose: the "Trovato is at" opening line in `README.md`,
  `ROADMAP.md`, `CONTRIBUTING.md` and `KNOWN-ISSUES.md`, and the doc comments that
  use the current API tuple as their worked example. The third is not prose and
  would have failed the suite. `api_compat_same_version_ok` and
  `api_compat_newer_minor_rejected` in `info_parser.rs` name minors relative to the
  kernel, so `"0.100"` had to move from the rejected case to the accepted one, with
  `"0.101"` taking its place. Bumping the tuple without touching them would have
  left a test asserting that the kernel's own API version requires a newer kernel.

  The freeze language that was keyed to "the 0.99 series" now says "before 1.0",
  which is what it always meant: the plugin boundary does not move before 1.0.

- Clippy lints the plugin crates, which it had never done.

  `cargo clippy --all-targets` runs over the workspace's *default* members, and no
  `plugins/*` crate is one: the CI Clippy job, `scripts/pre-commit-check.sh`,
  CONTRIBUTING.md and docs/coding-standards.md all named that command, so every line
  of plugin code was unlinted while the documentation said the toolchain had been run
  over it. Contributors write plugins, which makes that the wrong half of the tree to
  skip. All five invocations now pass `--workspace`.

  Turning it on found five errors, all mechanical: three `manual_let_else` in
  `plugins/argus/src/reader_api.rs` and two `single_char_add_str` in
  `plugins/trovato_seo/src/lib.rs`. Fixed here, so the gate goes green the first time
  rather than landing a job that fails on arrival.

  Found while gating the 0.99 parity work: a local `cargo clippy --workspace` failed
  on crates CI had reported clean, which is the kind of disagreement between two gates
  that is worth chasing rather than working around.

- Book-style page trees: `trovato_book`, a standard plugin giving ordered hierarchy
  with previous/next/up navigation, the Drupal 6 book model.

  Nothing provided it, and menu hierarchy does not: a menu answers "what is under
  this" and a book answers "what comes next", which needs a total order over the whole
  tree rather than an ordering among siblings. The docs site wants this immediately —
  nine tutorial parts and nineteen design documents are two books.

  **Storage** is one plugin-owned table, `book_page`, declared in
  `[capabilities] db_tables`, with a migration the plugin ships. No kernel table is
  touched: a `book_id` column on `item` would make every site carry a column for a
  feature most of them do not use. A **book is identified by its own root page**
  (`book_id = item_id` for the root) rather than by a separate entity, which keeps the
  model to one table and makes "which book is this in" a column read.

  **Reading order** is depth-first, siblings by `(weight, title)`. The title tiebreak
  is not cosmetic: without it two siblings at the same weight have no defined "next",
  and prev/next is exactly the question a book has to answer. A test walks a ten-page,
  three-level book by following the rendered `next` link and asserts it visits every
  page exactly once.

  **Two things this plugin cannot do on the 0.100 contract**, reported rather than
  worked around, because both are missing kernel seams and not plugin bugs:

  - **A fieldset on the item form.** The item form does not go through `FormService`,
    so `tap_form_alter`, `tap_form_validate` and `tap_form_submit` are never
    dispatched for it — `FormService` is constructed and exposed on `AppState`, and no
    route calls `build` or `process`. Authoring therefore lives on the plugin's own
    screens under `/admin/structure/books`, which is a complete path and not the one a
    Drupal user would expect.
  - **A sidebar tile rendering the tree.** `services/tile.rs` dispatches on
    `tile_type` in a closed `match` in the kernel, so a plugin cannot register a tile
    type. The tree renders into the item view instead, which puts it in the content
    region rather than the sidebar.

  Both would be additive kernel changes rather than contract breaks, and both are
  larger than this plugin. The module docs name the file and the reason for each, so
  whoever wants them next does not have to rediscover why they are absent.

  A third limitation, smaller and worth stating because a test asserts it honestly
  rather than aspirationally: the plugin renders the tree from its own rows and cannot
  apply the kernel's access filter, so an unpublished page still appears in the tree
  and 404s when followed. Filtering it would need the plugin to be able to ask "may
  this viewer see this item", which `tap_item_access` answers in the other direction.

  Cycle rejection, orphan promotion (a removed page's children move to its parent) and
  the one-book-per-item rule are enforced by the plugin, because a self-referential
  foreign key permits a cycle and a cyclic book is one that cannot be read.

- Two tests that depended on something outside themselves now carry their own
  preconditions.

  `db_probe_state` in `crates/kernel/tests/plugin_test.rs` derived its fixture path
  from the plugin name, and both WASM-2 probes pass the same name, so the two shared
  one directory: each writes a migration file, has `DbPolicy::derive` read it, and
  then removes the directory. When the removal landed between the other probe's write
  and its read the allowlist came back empty, and
  `db_select_inside_allowlist_passes_gate` failed with `left: -16, right: -16`, for a
  reason with nothing to do with the gate under test. Each call now gets a directory
  named for the process and a counter, so no two probes can collide however many are
  added.

  `plugin_view_render_test` seeded items of type `blog` while loading only
  `trovato_series` into its dispatcher. `blog` is declared by `trovato_blog`'s
  `tap_item_info`, and the content-type registry syncs rows only for the types the
  loaded plugins declare, so that type existed only when some other test binary had
  already run against the same database. On a virgin database three of its four tests
  fail with `item_type_fkey`, `Key (type)=(blog) is not present in table "item_type"`.
  The file now loads the plugin that declares the type and asserts the row arrived, so
  a failure names the cause instead of surfacing as a foreign-key violation three
  functions away. `trovato_blog` contributes no `tap_item_view`, so nothing this file
  asserts on changes.

  Both were caught by CI and not by the local gate, which is the interesting part.
  Sharding changes which test binaries share a database, so it exposes a test that
  quietly depends on another one while the single-database local run hides it.
  CONTRIBUTING.md calls the local run the stronger gate; it is stronger against
  cross-file interference and weaker against cross-file dependence.

- The "what is configuration-import only" list is a test, not prose.

  `ROADMAP.md` sets the 1.0 bar as "a site can be built, configured and operated
  through the interface", and the list of what fell short of that lived in a
  paragraph. Paragraphs drift: menus were listed among the types *with* admin screens
  for a while and did not have one, which is how that defect was found.

  `crates/kernel/tests/config_admin_coverage_test.rs` holds the audit as a table with
  one row per config entity type, and each row is one of two things:

  - a path that must **actually serve** for an administrator — a real request, since a
    route that 500s is not a screen — and must **not** serve for an anonymous visitor;
  - a sentence that must appear in `KNOWN-ISSUES.md`, so an import-only type is a
    written decision rather than an omission.

  The table is asserted against `ENTITY_TYPE_ORDER`, in both directions, so adding a
  config entity type fails this test until somebody decides which it is, and removing
  one fails it until the row goes. That is the whole mechanism and the only part of
  the file that will still be earning its keep in a year.

  The audit found the outcome, so here it is: **twelve of the thirteen types have a
  screen.** The thirteenth is `language`, now import-only by a decision with a reason
  written down — a site's language set belongs in its config set, and a form that adds
  a language row would not do the interface strings (`trovato_locale`, by `.po`
  import) or the content translations (`trovato_content_translation`, per item) that
  are what actually make a language work.

  `variable` is a key/value store rather than one thing, so KNOWN-ISSUES.md now lists
  it setting by setting: which have a screen, which do not (`robots_txt_custom`, and
  anything a plugin defines), and why there is deliberately **no generic variable
  editor** — a form writing arbitrary JSON into arbitrary `site_config` keys can break
  a site in ways the kernel parses at startup, with no validation possible because the
  schema is per key.

  Two smaller corrections the audit forced. Tags are managed per category
  (`/admin/structure/categories/{id}/tags`), not at `/admin/structure/tags`, which is
  only the edit and delete path — so the audit names the real screen. And
  `ENTITY_TYPE_ORDER` is now public, because it is not only an import ordering: it is
  the list of config entity types, and the audit walks it.

- The committed `ritrovo_importer.wasm` is reproducible from public sources, and its
  provenance header says so with a recipe that works.

  This closes the gap the version sweep opened honestly. That sweep found the header
  asserting a byte-for-byte reproducible build and citing "the contract-freeze commit
  `9791c24`" as the pinned SDK revision — a commit in no published repository — and
  replaced the claim with a statement of the gap. Ritrovo now builds against this
  repository's SDK, so the artifact is rebuilt from that: Ritrovo `8e72d37`, SDK pinned
  at this repository's `50c46ee`, sha256 `e3361482…`. The recipe in the header was run
  from a fresh clone and produced the artifact byte for byte.

  **What the refreshed artifact no longer demonstrates**, said plainly because the old
  one demonstrated it by accident: it is built against the *current* SDK, so it is no
  longer evidence that a plugin compiled against an *older* SDK still loads. The old
  artifact was, and could not be rebuilt or audited by anyone — a freeze guarantee whose
  subject is a black box is weak evidence, and this one becomes the older artifact
  naturally as the kernel moves on, with provenance a reader can check. The
  version-compatibility rule itself is covered by `info_parser.rs`'s own tests.

  Two corrections to KNOWN-ISSUES.md while there, both found by running the real
  install rather than by reading:

  - It said the artifact "is checked in so the tutorial works without a second
    repository". The directory holds the `.wasm` and **no manifest and no migrations**,
    so the plugin loader skips it with `no .info.toml file found, skipping`. It cannot
    make the tutorial work; it is the paired-consumer test's fixture.
  - `docs/tutorial/part-02-ritrovo-importer.md` has been stale since Ritrovo moved to
    its own repository: it tells the reader to read the plugin's source (not here), to
    run `cargo build -p ritrovo_importer` (not a workspace member), and that its
    manifest declares `default_enabled = true` (there is no manifest here, and the real
    one says false). Recorded rather than fixed — rewriting that part is its own piece
    of work, and guessing at it in a provenance change would be worse than naming it.

- A person can delete their own account, and **deleting any account that ever
  wrote anything now works at all**.

  The second half is the bug. `item.author_id`, `item_revision.author_id`,
  `comment.author_id` and `file_managed.owner_id` are `NOT NULL REFERENCES users(id)`
  with no `ON DELETE` action, so a delete failed on a foreign key for any account
  with content. The admin screen offered the button and reported "Failed to delete
  user"; there was no self-service path to fail. `User::delete` now reattributes in
  one transaction before deleting, so both paths work.

  **What deletion means**, stated on the confirmation screen in the same terms:

  - The account row is deleted, with its password, passkeys, API tokens and every
    session on every device.
  - Authored items and comments are **reattributed to the anonymous author**, not
    destroyed. Content integrity wins: a thread with holes in it damages every other
    participant's record of a conversation they took part in. The screen says so, and
    says that deleting a specific item first is the way to have it gone.
  - Two nullable references that record *who did an administrative act*
    (`config_revision.author_id`, `stage_deletion.deleted_by`) are **cleared**, not
    reattributed. "The anonymous author published this revision" would be a claim,
    and a false one.
  - `tap_user_delete` fires before the row goes, so a plugin can clean up its own
    user-keyed rows while the user still exists. That tap already existed and was
    already dispatched; nothing was added to the plugin contract for this.

  **The invariant this had to narrow, and why.** `item_revision` rows are immutable
  by trigger — a revision is a snapshot, and a snapshot that can be edited is not a
  history. That made the deletion impossible rather than merely awkward. The three
  ways out were: delete the author's revisions (wrong: they are the history of items
  that remain visible), refuse deletion to anyone who ever saved an item (wrong: that
  is most accounts, and a right of erasure that applies only to people who never
  wrote anything is not one), or narrow the invariant. It is narrowed, deliberately
  and in a migration that explains itself:

  > Before: a revision never changes.
  > After: a revision's *content* never changes; its authorship may be anonymized
  > when the author's account is deleted.

  Enforced by comparing whole rows (`to_jsonb(NEW) - 'author_id'`) rather than a list
  of columns, so a future column is covered rather than silently exempted, and the
  new author must be the anonymous sentinel so this cannot reassign a revision from
  one real person to another.

  **Re-authentication.** There was no re-authentication machinery anywhere to reuse:
  no admin flow has one, and the WebAuthn routes authenticate from scratch into a
  session rather than stepping an existing one up. So this adds a step-up scoped to
  deletion — a password post, or a fresh WebAuthn assertion — good for five minutes,
  stored under its **own** session key so a login ceremony in flight cannot be
  completed as a deletion step-up or the reverse. An account with both a password and
  a passkey is offered both, because making somebody type a password they may not
  remember when the authenticator is right there is a worse screen, and the reverse
  when the authenticator is at home. The password path needs no JavaScript; the
  passkey path does, because a WebAuthn ceremony does.

  **The audit row names no account.** `security_audit_log.user_id` references `users`
  with `ON DELETE SET NULL`, so a row written after the deletion cannot carry the id
  — and a row that did would undercut the erasure it records. The event carries the
  hashed subject and the counts (items and comments reattributed, sessions revoked)
  and no raw identifier, which is what the audit module's hashing rule is for.

  **The last active administrator cannot leave**, because a site with no
  administrator cannot be administered back into having one. Refused, audited, and
  said on the screen before the button rather than after it.

- A person can download their own data. `/user/data-export` serves one JSON
  document holding the profile, the roles held, every authored item with its fields
  and timestamps, every comment, and the metadata of active sessions.

  Trovato had no way to produce a copy of someone's own data. For a site operated
  from the EU with open registration that is GDPR article 15, and the practical
  reading is the same: a person who cannot get their own writing back out of a site
  does not really have it.

  What is deliberately **not** in it, said on the page and again inside the file:

  - **Anything the person only looked at.** Trovato keeps no reading history, so
    there is nothing to export. Saying so is better than leaving an absence to be
    guessed at.
  - **Session tokens and credential material.** A session's metadata answers "which
    of my devices are signed in"; the token would let whoever holds the file *be*
    those devices, which is the opposite of a privacy feature. The IP address is
    excluded too: it is the rate limiter's business, not the export's. There are
    tests for each of those absences rather than a comment claiming them.

  A comment is exported at every status, including unpublished and spam. It is the
  author's own writing, and an export that quietly omitted what a moderator hid
  would not be a copy of their data.

  Once an hour per account, through a new `data_export` rate-limit category, keyed
  on the account rather than the IP: this is a per-account cost, and two people
  behind one address should not be able to starve each other of their own data. It
  is also the one authenticated read on the site whose cost scales with what the
  caller wrote, since the document is built in memory. An account with hundreds of
  thousands of items would want a queued export written to a file; that is not this,
  and the limit is named in the module docs rather than left to be discovered.

  `Comment::list_by_author` is new — the model could count an author's approved
  comments and not list their comments — and the download filename is derived from
  the username rather than trusting it, since a username is user input and this one
  ends up in a header.

- A site can learn that a release exists, including a security release, and no
  server was built to make that true.

  Sites had no update channel at all: a security fix could ship and nobody running
  Trovato would hear about it. The usual answer is an update server, which is a
  service to operate for the life of the project. GitHub already is one. Tagging a
  release produces `https://api.github.com/repos/jeremyandrews/trovato/releases/latest`
  and `https://github.com/jeremyandrews/trovato/releases.atom`, both free and both
  stable, so the work is a client and a convention rather than infrastructure.

  **The convention, now in CONTRIBUTING.md's release section: a security release's
  title starts with `[security]`.** The latest-release JSON says what the newest
  version is and has no field for urgency, and "a newer version exists" and "act
  now" are different things to put in front of an administrator. The signal
  therefore lives in the one field a human writes deliberately. The prefix has to
  lead: a title merely mentioning security is not a security release, or every
  release note that says the word becomes an emergency.

  **The client** is a cron task, not a plugin. It concerns the kernel's own version,
  which a plugin cannot know, and it needs an outbound request, which a plugin would
  need a network capability for — making every site grant a plugin network access to
  learn its own version is the worse trade. It compares the release tag against the
  compiled-in workspace version and stores the answer in `site_config`
  (`update_status`: latest version, title, is_security, checked-at). Failures are
  logged at debug and change nothing, on a five-second timeout, at most once per
  interval (default daily). **No page render ever makes the request**; the banner
  reads what was stored.

  The comparison is deliberately not semver. Trovato's versions are dotted integers
  and nothing else, so it parses components and **refuses what it cannot read**
  rather than guessing — `0.99.0-rc1` is not `0.99.0`, and `nightly` is not a
  version. A tag it cannot read produces no banner, which is the safe direction: a
  false "you are behind" is worse than a missing one, because an administrator who
  cannot find the release it means stops trusting the banner. String comparison
  would also have got `1.0.0` versus `0.99.0` backwards, which is precisely the
  comparison this project is about to need.

  **The banner** appears on `/admin`, past `require_admin`, with ordinary styling
  for an ordinary release and alarm styling (plus `role="alert"`) for a security
  one. A visitor and a logged-in non-administrator are never told the site's
  version, which is a fingerprinting detail with no upside for them, and there are
  tests for exactly that rather than a comment claiming it.

  **Privacy and control**, stated the same way in INSTALL.md: one HTTPS GET carrying
  no site data — no URL, no version, no identifier, nothing but the request and a
  `Trovato/<version>` User-Agent. GitHub learns that an IP address asked about a
  public repository. Two switches, environment wins: `UPDATE_CHECK=0` for a
  deployment that must make no outbound requests, and a **Check for Trovato
  releases** checkbox at `/admin/config/site` for a site that simply does not want
  it. On by default, deliberately: a site with no way to learn a security fix exists
  is a site that does not get it, which is the posture Drupal core has shipped for
  two decades.

  `UPDATE_CHECK_ENDPOINT` is configurable, and that is what makes this testable: the
  integration tests serve a release payload from an axum app on `127.0.0.1:0` and
  drive the real `CronService` against it. **No test touches the network.** They use
  a plain HTTP client rather than the kernel's SSRF-hardened one, because the
  hardened resolver refuses loopback — which is correct, and is what production
  gets.

- Stages have an administration screen. `/admin/structure/stages` lists them and
  creates and edits machine name, label, description, visibility, default and
  weight.

  That list is what the schema models, and the form does not widen it. In
  particular there is **no workflow-membership field**, because there is nothing to
  edit: the tutorial ships `variable.workflow.editorial.yml` describing stage
  transitions, and a repository-wide search for `workflow.editorial` finds the file
  and no consumer. A field for a relationship the kernel does not model would be a
  field that does nothing.

  The guard rails live on the model rather than in the handlers, so `config import`
  and any future caller are held to the same ones and the form's contribution is a
  sentence instead of a constraint violation:

  - **Exactly one default stage.** `Stage::update` clears the flag everywhere else
    in the same transaction when it sets it, which is what the partial unique index
    on `is_default = true` requires. Clearing the last one is refused, because new
    content has to land somewhere.
  - **The Live stage stays public and stays.** Published content is resolved
    through the one public stage, so demoting it would take a site's published
    content off the site. `Stage::update` refuses that, and delete already refused
    Live.
  - **Only one public stage**, which the partial unique index on
    `visibility = 'public'` also says. The form refuses the second rather than
    letting Postgres do it.

  Three things this needed that the model did not have. `Stage` could update its
  label and its visibility and nothing else, so machine name, description, weight
  and the default flag had no write path at all — `Stage::update` is that path, in
  one transaction, because a stage is two rows and a half-applied edit is a stage
  whose tables disagree about which stage it is.

  And a defect: `Stage::delete`'s doc comment said it "checks for content
  referencing this stage (items, aliases, menu links, tiles)" and it counted
  **items only**. All four columns are `RESTRICT` foreign keys, so a stage holding a
  menu link, a tile or an alias was refused by Postgres with a message naming a
  constraint rather than by Trovato with a count. `Stage::reference_counts` now
  counts all four and the refusal reads "cannot delete stage: 3 items and 1 menu
  link still reference it", which is what an operator needs in order to go and move
  them. The listing shows the same count per stage, so the refusal is predictable
  before the click rather than discovered after it.

- The roles screen says what deleting a role does. `/admin/people/roles` now
  shows, per role, how many people hold it and how many permissions it grants, and
  the delete confirmation names the number of members who will lose it.

  The roles CRUD already existed — list, create, rename, delete, with the
  anonymous and authenticated roles refused — so this is not a new screen. What was
  missing was the consequence. `user_roles.role_id` is `ON DELETE CASCADE`, so
  deleting a role silently takes it away from everyone who holds it. That is the
  right behaviour; discovering it afterwards is not. The confirmation now reads
  "3 user(s) hold it and will lose it. Their accounts are not deleted, and no other
  role is affected", and the page states the same thing outside the dialog for
  anyone who has JavaScript off and never sees a confirm.

  The two screens also now link to each other. Roles linked to permissions and
  permissions linked nowhere, which made the grid a dead end from a screen that
  cannot grant anything itself.

  And the grid says what it cannot do. It renders the kernel's permission list, so
  a plugin's permissions are absent from it — the same `tap_perm`-is-not-dispatched
  limitation `config import` hits from the other side. A grid that silently omits
  half the permissions on a site with plugins is worse than one that says it does.

- A role config file carries its permissions, and `config import` grants them.

  Root cause: the `role` config entity was the `roles` row, which has three
  columns — id, name, created. Permissions are `role_permissions` rows, and nothing
  in the config layer knew about them, so `config import` created roles that could
  do nothing. The workaround had spread into the documentation: the tutorial's
  three role files listed their intended permissions **in comments**, and
  `docs/tutorial/recipes/recipe-part-04.md` had the reader paste SQL to apply what
  those comments said. A config set that describes a site and cannot configure its
  roles is not a config set.

  A role file now declares `permissions:` and the import applies exactly that set.
  Three decisions worth stating, because each has a wrong answer that looks right:

  - **Replace, not merge.** A permission the file no longer names is revoked. The
    file is the description of the role; a merge would make it impossible to take
    a permission away through the file that granted it.
  - **An absent key means "leave the grants alone", not "revoke everything".**
    Every role file written before this omits the key, and reading omission as an
    empty list would mean an import silently stripping a site's permissions. An
    explicitly empty list (`permissions: []`) does revoke everything, so the
    intent is still expressible. Export always writes the key, so an exported role
    is authoritative on re-import.
  - **An unknown permission fails validation, naming the file and the string.** A
    permission is a bare string, so a typo is not a constraint violation: it is a
    grant that never matches anything the code checks. Validation happens with the
    rest of phase 2, so nothing is written.

  "Unknown" needs care, because the kernel does not know every valid permission. A
  plugin declares its own through `tap_perm`, which is declared in the WIT and not
  dispatched, so plugin permissions are in no list the kernel can consult. Valid
  therefore means *either* a permission the kernel defines *or* one some role in
  this database already holds — and the second half is not a convenience, it is
  what lets an export of a site that uses plugin permissions re-import at all. The
  seeded `authenticated user` role proves the two sets differ: it holds `view own
  profile`, which the kernel's own list does not contain. The error message says
  which of the two likely causes applies, because "unknown permission" alone does
  not tell an operator whether to fix a typo or enable a plugin.

  The tutorial's role files now declare real lists. The `ritrovo_access`
  permissions they also want (`view incoming conferences` and three more) stay in
  comments, because listing a permission the kernel has no evidence for would make
  the tutorial's own config set fail to import; each file says so and says where
  they come from. `KNOWN-ISSUES.md` carries what remains of the limitation, which
  is now about plugin permissions specifically rather than about roles.

  Two pieces of tidying that fell out, both in service of one implementation
  rather than two:

  - The list of kernel permissions moved from `routes/admin_user.rs`, where it was
    private to the permission grid, to `models::role::KERNEL_PERMISSIONS`. Two
    consumers reading two lists is how they drift.
  - The set arithmetic behind "make this role hold exactly this set" moved to
    `Role::set_permissions`, and `RoleService::save_permissions` now wraps it to
    invalidate the permission cache. The two unit tests that covered that
    arithmetic reimplemented it in their own bodies, so they asserted that two
    `HashSet` differences agree with each other and would have passed with the real
    function deleted; they now call it.

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
