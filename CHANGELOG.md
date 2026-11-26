# Changelog

## v0.105.0 — 2026-11-25

Trovato is at 0.105.0 and the plugin API is `(0, 105)`. A plugin that declares
`api_version = "0.104"` loads unchanged: the kernel accepts any plugin API with
the same major and a minor no newer than its own, and refuses only a newer minor
or a different major. A plugin that uses anything added in this release, such as
the `query` and `text-format` interfaces, `load-users`, `send-to-user`, or the
`[config] tables` and `secret_columns` manifest keys, declares `"0.105"`, which a
0.104 kernel refuses cleanly instead of failing to link.

A large release: everything merged since v0.104.0, about a hundred and sixty
entries. Read as a whole it is one long pass over a single question, whether a
decision the kernel makes is made in one place and made as the person asking.

Each entry was written when its change merged and says what was true then, and
the entries are not in date order, so a few describe a state that another entry
in this section moved past. Two matter to an operator. The lock file refresh
says two advisories remain, in rmcp 0.16.0 and libsqlite3-sys 0.30.1; the rmcp 3
and sqlx 0.9 entries close both, and this release locks rmcp 3.10.0 and
libsqlite3-sys 0.40.0. The incremental search index design says the `pagefind`
CLI still rebuilds the index after every change; the entries that build the
index in the kernel and then update it item by item replace that, and no server
or image of this release needs a `pagefind` binary.

Access control comes first. Item reads on every path, item writes from forms and
from plugins, field edit access, files, gathers, the ad hoc gather endpoint,
outbound HTTP and plugin SQL now decide through the same seams, as the viewer.
API tokens are stateless and die when revoked, an account change ends the other
sessions, a delegated user administrator acts only within their own authority,
and a refusal answers the same way from every handler: `401` or `403` for a JSON
caller, a sign in or a themed error page for a browser, one JSON error shape.

Editors get a working day's tools. Every kernel form goes through one forms
pipeline, so the form taps fire and a save no longer erases the fields a form
did not render. Items move between stages by workflow, editors read from their
stage, and content moderation, revisions with preview, promote and delete,
publishing options and bulk actions all work from the admin screens.

The site around the content filled in: files served to every viewer of the item
that holds them, mail that survives a down server and reaches a member by id,
password reset in a browser, search that indexes everything and is built and
updated item by item by the kernel itself, gather feeds in Atom and RSS per
author and per tag, tiles and menus that know the viewer and the language, an
administration organised by task, text formats and themes chosen from the admin,
and cache, request and trace metrics that leave the process.

Plugins get seams in place of shortcuts. A plugin reads content through a
`query` interface compiled by Gather for its viewer, writes items through the
item service as the user it acts for, does the ordinary writes on its own tables
without `raw_sql`, and wherever `raw_sql` is still granted the admin says so.
Config export carries plugin configuration and never a secret. Several servers
can share one site, and migrations run as a step of their own.

Gather queries in `table` format render a table. 23 of the 29 queries in a stock
install use that format, including nine core administrative listings, and every
one of them failed to render, logged the failure, and fell through to a fallback
that dumped whatever keys the rows happened to carry. Each page still answered
200, which is why nothing upstream noticed.

`templates/gather/query--table.html` iterates `columns`, and no kernel code ever
put `columns` in the render context. It is supplied now: a query that names its
fields gets those, in declared order and under their labels, while a query that
names none is a `SELECT base.*`, which is what every core listing is, and takes
its columns from the rows. The same template also rendered `row.id` bare, and a
query that names its fields has the injected access columns stripped back out of
its rows, so `row.id` was absent for exactly those queries and failed the whole
render; it is now conditional.

Underneath was a third copy of the same disagreement, and that one was losing
data rather than markup. The query builder projects a plain column unaliased, so
its key in a result row is its field name, but the row-rebuild in the field
access pass listed its key as the column's label. Any gather that gave a label
to a plain column had that column dropped from every row it returned, silently
and regardless of format. The rule now lives once, as `QueryField::result_key`,
and the projection, the access pass and the template columns all read it.

AI calls are priced on the model that was requested, so a currency cap enforces
something again. A run of 132 calls logged every one of them unpriced and left
the daily spend reading `0` while real money was being spent, with the requested
model sitting in the `ai_pricing` table the whole time.

The root cause was the string the pricing lookup was keyed on. `host/ai.rs`
priced from `ai_response.model`, parsed out of the provider's own response.
Anthropic resolves an alias server side, so a request for `claude-haiku-4-5`
comes back as `claude-haiku-4-5-20251001` and matches no row in a table an
operator keys by the name they configured. `services/ai_assistant.rs` had the
same defect through `completion.model`. Both now price on the requested model,
and the resolved string stays in `ai_usage_log.model`, where it says what
actually ran.

An unpriced call is no longer silent. It is logged with the two model strings,
so an operator can see which one to add to the pricing table. And because a NULL
`cost_estimate` sums as zero, a period of unpriced calls and a period of no
calls produced the same spend figure: a currency cap checked against it was
enforcing nothing and saying nothing. The cap check now counts the rows it
cannot see and reports them alongside the spend, so a cap is never quietly
exceeded. The kernel still refuses to invent a cost for a model it has no price
for; what changed is that it admits the gap instead of presenting a zero as an
answer.

Plugin-declared content types get their fields back. `GET /admin/content/add/<type>`
rendered a title and a Published checkbox and none of the type's declared fields
for every type registered through `tap_item_info`, and because the same empty
field list reached `validate_required_fields`, an item saved cleanly with a
required field left empty.

The root cause was a writer and a reader disagreeing about one column. Two
shapes were being written to `item_type.settings`: the core seed migration and
every admin-side writer stored the object `{"fields": [...]}`, while plugin
registration stored the bare array `[...]`. The reader only understood the
object shape and returned an empty `Vec` for anything else without a word, so
`page` rendered its body field while `blog` and every plugin-declared type
rendered none. The object shape is now the one canonical layout: plugin
registration writes it, a migration lifts the rows already written as bare
arrays, and a reader that cannot parse what it finds says so in the log instead
of passing it off as a type with no fields. The same disagreement also made
`persist_fields` silently write nothing when it found an array, so adding,
editing or deleting a field on a plugin-declared type reported success and
changed nothing; it now normalizes the row instead of skipping the write.

Argus leaves the tree, and two security documents arrive. No kernel code changed
and no kernel behaviour moved.

Argus, the news-intelligence plugin, now lives in its own repository,
`jeremyandrews/argus`, pinned to the `v0.104.0` tag and installed as an overlay
on the released image the way Netgrasp and Ritrovo are. `plugins/argus`,
`crates/argus-core` and the four `argus_*_test.rs` suites are gone from here; the
suites went with the code, along with the Argus half of `plugin_api_test.rs`. The
workspace members, the Dockerfile plugin build, the CI and pre-commit plugin
builds, the `argus` and `argus-cron` compose services and the Argus section of
`.env.example` go with them, and so does the `roxmltree` workspace dependency,
which nothing else used. `plugin_queue_v2_test.rs` used Argus as its second
queue worker and now uses `trovato_spam`, which has the same property the test
needs: it answers an unrecognised job with an error value rather than a trap.
The version map's manifest count drops from 37 to 36. An image built from this
tree no longer carries `argus`, so a deployment that ran the old compose profile
installs it from the new repository instead.

`SECURITY.md` at the repository root, which the project did not have. It names
GitHub's private vulnerability reporting as the channel, sets response time
targets, and says that security fixes land on the current minor only while
Trovato is pre-1.0. A project that ships a security release convention
(`[security]` in the title, read by `is_security_title`) and a dependency
advisory policy but no way to report a vulnerability privately was telling
people to open a public issue by omission.

`docs/security/REVIEW-BRIEF.md`, written to be handed to an external reviewer
without the maintainer present. It maps ten surfaces to their code, the
invariant each is supposed to hold and the tests that pin it; collects the
security relevant changelog entries and every advisory taken, with commit ids;
states the threat model as five attackers and what each must not be able to do;
and says what "done" means, which is a findings table where every row's status
reads `fixed <commit>` or `accepted: <reason>`. `KNOWN-ISSUES.md` records that
the security posture is "reviewed once, by the author"; an independent review of
those findings is welcome, and this is the document written for one. It does not
gate 1.0 (BL-66).

Writing it turned up twelve places where the repository's own documentation
claims more than the code does, collected in section 6 of the brief rather than
fixed here. Two are worth naming: `docs/admin-permissions.md` says exactly two
routes still gate on `require_admin` and the code has nine plus one inline
check, and `docs/BACKLOG.md` maps the rustls advisory bump to the wrong commit.

A plugin can add page builder components without touching the kernel. A
component is a declaration, `components/<name>.component.toml`, a Tera template
in the plugin's `templates/`, and optionally a stylesheet in its `static/`. At
boot the kernel builds one registry from its standard components and those of
every enabled plugin, and the editor, the server-side validation and the
renderer all read from it. The Puck editor no longer carries a hard-coded list:
it builds its component list and every field form from
`GET /api/v1/page-builder/components`, and shows a component it has no React
preview for (any plugin's) by asking the kernel to render it, through the new
`POST /api/v1/page-builder/preview`, with the page's own template, sanitizer and
checks. The kernel's thirteen standard components are declared in the same
format, so they are the examples. See "Page builder components" in
`docs/plugin-development.md`.

The format is TOML, like `.info.toml`, read with unknown keys refused, so a
misspelt limit is an error at boot rather than a rule that never applies. A
file that does not parse, a missing template or stylesheet, or a type name two
plugins both declare stops the server, naming the file and both plugins. Field
types are `text`, `rich_text` (Markdown, rendered and sanitized by the kernel),
`number`, `heading_level`, `url`, `image` (alt text required unless marked
decorative), `link`, `select`, `boolean`, `list` and `reference` (an item or a
gather query, resolved as the viewer when the page is viewed). Every rule is
checked on the server on every create and update that goes through the item
service; a page that breaks one is refused with an error naming the component
and the field (`field_layout.content[2].cards[1].title: is required`), shown on
the item form and returned as a 422 by the JSON API. Before, nothing checked a
saved page on the server: the registry had a `validate_page` (component type,
heading level, one image's alt text) that no write path called, and only the
editor knew the rest. A component's template receives its declared fields only, each
normalized to its type, so a value the declaration does not name, an unsafe URL
or a select value outside its options never reaches it.

The page builder's sanitizer no longer allows the `style` attribute, and no
component writes one from a prop. The four standard components that did now
take a declared variant instead: Hero, Call to Action and Section have a
`background` select (none, light, dark, brand, adding a `pb-bg--*` class), and
Columns has `gapSize` (default, none, small, large). The free-text
`backgroundColor` and `gap` fields are hidden in the editor and no longer
rendered; a stored page keeps the values, still validates, and renders without
them. The standard Hero's background image, which was a `background-image`
style, is an `<img>` behind the content. A page saved before this release that does not use
those three props renders byte for byte as it did, and a test holds that. See
UPGRADING.md.

`/static/css/page-builder.css` is now linked on a page that has page builder
content; nothing linked it before, so the standard components rendered
unstyled outside the editor. A plugin component's stylesheets are linked the
same way, each once and only when the component is on the page, from
`/static/plugins/<plugin>/`, where the kernel now serves an enabled plugin's
`static/` directory. An enabled plugin's `templates/` directory is on the
template search path at the lowest precedence.

A Hero saved without a `variant` failed to render ("Variable `variant` not
found"): the template defaulted the class but compared the raw value further
down. It now defaults it once.

The release image ships only the plugins it builds (BL-122). The Dockerfile
made a directory under `/app/plugins` for every directory in `plugins/`, so the
in-tree test and reference plugins were discovered with a manifest and no
module behind them. It now assembles a directory only for each built module,
and copies its `components/`, `templates/` and `static/` along with its
manifest and migrations.

The editor's source moves from a single file to a small TypeScript project in
`plugins/trovato_page_builder/editor/`, built with esbuild into the committed
`static/js/page-builder-editor.js` and `static/css/page-builder-editor.css`,
with Vitest tests of the stored-page conversion and the registry-built
configuration.

A plugin can declare page level fields for page builder pages, beside the
built in title, and a page can use them to keep, hide or replace a sitewide
part such as a footer call to action. Before, a page's `root` was read into a
struct with a `title` and nothing else: any other key the editor stored there
was dropped when the page rendered, never validated when it was saved, and
seen by no template, and the editor's page settings form could only offer the
title because nothing declared anything more.

Root fields are declared in `components/<name>.root.toml`, next to a plugin's
components, as a list of `[[fields]]` in the component field format with the
same eleven types, limits and checks, and optionally the `classes` a root
select may add. Two enabled plugins declaring one root field name stop the
server at boot naming both, as a duplicate component does; `title`, `props`
and `variant_classes` are reserved, and a default the field's own rules refuse
is an error at boot. `GET /api/v1/page-builder/components` serves them as
`root_fields`, and the editor builds its page settings form from them with the
same control per type as a component field.

Every save checks the root like a component's props, with the error at
`field_layout.root.<field>` (a 422 from the JSON API). A root key no enabled
plugin declares is refused on save; a stored page is never validated to
render, so one carrying such a key renders as it did, without passing it on.
The kernel reads the root in either shape Puck writes, flat or under
`root.props`. The editor stores it flat and leaves out keys no enabled plugin
declares.

Page templates, the item template and every tile template on the same request
receive `page_root`: the title, each declared field normalized to its type
(with `<name>_html` for rich text) or its declared default when the page has
no value, and `variant_classes`. A page with no page builder content gets the
defaults. A `custom_html` tile is now rendered through
`elements/tile--custom_html--<machine name>` or `elements/tile--custom_html`
when a template of that name exists, with `tile`, `body` and `page_root`, and a
tile whose template renders only whitespace is left off the page. A page
saved before this change validates and renders byte for byte as it did. See
"Page root fields" in `docs/plugin-development.md`.

A search result for a page builder item no longer shows its page builder
document as its snippet. Every such result on `/search` read
`{"root": {"props": {}}, "content": [{"type": "TextBlock", …` under its title,
on every screen, and on a phone the unbroken JSON ran the results page wider
than the screen.

The snippet source (`search::snippet::SOURCE_SQL`) fell back from
`field_body->>'value'` to `fields->>'field_body'`, and `->>` on a JSON object
returns the object's JSON text. A page builder field is exactly that object,
with no `value` key. The bare field is now read as text only when it is a JSON
string, the same rule `item_search_vector` already indexed by, so the snippet
shows what the index holds: the title, and a long text body where there is one.

A holder of `administer users` who is not a superuser can now act only on
accounts, roles and permissions within their own authority, and the last active
superuser can no longer be demoted, blocked or deleted from the admin screens. The
delegation rule for `administer users`, that a non-superuser may hand out only
what they already hold, was enforced on the role checkboxes of the user form and
nowhere else. The user edit and delete routes, the permission grid and role
deletion trusted any holder of the permission with accounts and permissions
beyond their own, superusers' accounts included, and nothing kept the last
superuser in place. The rule now lives in one place in `routes/admin_user.rs` and
all four paths use it: an account is within reach only when the actor holds every
permission it holds, and never when it is a superuser's; a role can be deleted
only when the actor holds every permission it carries; and the grid neither
grants nor revokes a permission the actor lacks. The grid also ignores, for every
actor, any permission name it did not render. The edit and delete routes refuse
to take the last active superuser out of that role, using the same
`blocks_last_admin` rule self service account deletion already applies.

A pull request's security audit depends only on that pull request now. The
`Security Audit` job ran bare `cargo audit`, which fetches the advisory database
before it scans, so what it actually asked was not "does this branch have a
vulnerable dependency" but "does it have one as of whenever this job happened to
start". Every open branch went red the morning an advisory landed against a crate
none of them had touched, and the only cure was to rebase and burn another full
CI cycle. A red audit had stopped saying anything about the branch it was attached
to.

The job now audits against one commit of the advisory database, recorded in
`.cargo/advisory-db-pin`, with fetching disabled. A branch fails only for an
advisory that already existed when the pin was set, which is a real finding about
its own dependencies, and the same commit audited twice gives the same answer.

The live check did not go away and was not weakened. `Live Security Audit` fetches
live against `main` every morning and on demand, fails loudly, and opens an issue
naming the advisory and the crate, or comments on the one already open rather than
filing a duplicate; a clean run closes it, so no open issue means the last run was
clean rather than that nobody looked. That job is allowed to go red for a reason no
branch caused, because no merge is waiting on it.

A pin that never moves would freeze the floor at the day it was set and hide every
later advisory from every pull request, which is the one real risk here, so it is
gated rather than trusted: `scripts/security-audit.sh` fails, in CI and locally, on
a pin more than 90 days old. The pin moves in its own reviewed pull request, and a
real advisory is answered by upgrading the dependency, never by moving the pin past
it.

Every page template now gets `user_name`, the signed-in viewer's account name,
and the stock theme's header reads "Logged in as <name>". `inject_site_context`
built the viewer from the user record but kept only the id, the roles and the
permissions, so only `/user/profile` and the admin dashboard, which put the name
in their own context, could say who was signed in; a theme could print "Logged
in" and never whose session it was. The name now rides on the `UserContext`
built from that same record, so it costs no query, and it is set only for a
signed-in viewer: an anonymous visitor's context has no `user_name` key. It is a
key of its own beside `user_authenticated`, not a field of `viewer`, and it
carries the account name and nothing else.

`POST /user/logout` redirects to `/user/login` again instead of answering 500.
Signing out a blocked account, and ending a session revoked from another device,
no longer answer 500 either. All three paths ended the session with
`Session::delete()`. That call removes the record from Redis but leaves the
in-memory session as it was: it still holds its data, keeps its id, and is
marked modified. On logout, the CSRF check had just spent the form's token,
which is what marked it modified. After the handler returned, the session layer
saved that session with `SET <id> <record> EXAT <t> XX`. Redis answers nil to
`SET .. XX` on a key that no longer exists. The store,
`tower-sessions-redis-store` 0.16, reads that reply as a `bool` through fred,
which cannot convert nil ("Could not convert to bool"), and the layer replaced
the response with a 500. The record had already been deleted, which is why the
session ended anyway. These paths now call `Session::flush()`, which also
clears the data and the id, so the layer has nothing to save and removes the
cookie. The session record is gone from Redis after logout, and a test now
asserts that against the real store.

Site search now finds every published item by its title, any of its text fields, or any of its published comments, in both backends, without anyone configuring a field first.

The Pagefind export read only the fields named in `search_field_config`, falling back to the body when a type had none, so every other text field (a summary, a location, a plugin's text field) was invisible until someone set a weight for it at `/admin/structure/types/{type}/search`. It now indexes every `Text` and `TextLong` field the type declares, and the configured weights stay weights: a text field with none counts as D. The Postgres vector behind `/search` and the API had the same rule and the same gap, and now builds from one SQL function, `item_search_vector`, with the same field list. A migration recomputes every existing item's vector.

Comments were never indexed by either backend: the export selected from `item` only, and the trigger read `item.fields` only. The export now loads every comment an anonymous visitor sees on the exported items in one query (published, under a published parent, the rule the item page's thread follows) and indexes them inside the item's fragment, so a comment match is a result for the item and links to it. A trigger on `comment` recomputes the item's vector when a comment is written, edited, published, unpublished, moved or deleted.

The body was indexed as escaped HTML (`<p>` became the text `&lt;p&gt;`) and the description meta kept safe tags. Every indexed value, title, fields, comments and description alike, now goes through the kernel's one plain text function (`page_meta::plain_text`: tags stripped, entities decoded, whitespace collapsed).

The index went stale on delete, unpublish and comment: `trovato_search`'s `tap_cron` asked for a rebuild only when `MAX(changed)` over published items rose, which none of those raise, so a deleted post stayed findable and a new comment never became findable. The kernel's item service now requests a rebuild after any write to an item that was or is published on the live stage, and the comment service after any write to a comment that was or is published; the rebuild runs at the next cron run. `tap_cron` keeps polling for writes that bypass the services.

The search configuration page's Reindex button did nothing to the Postgres vector: it touched `changed`, and the vector trigger fires only on `title`, `fields` and `type`. It now recomputes the vector. The page no longer labels an unweighted text field "Not indexed".

Each content type has two search switches at `/admin/structure/types/{type}/search`: "Include this content type in search" and "Include comments in search". Both are on unless an administrator turns one off, so a fresh install and every existing site keep searching everything.

Until now every published live item of every type the anonymous role can view was indexed, with no way to keep a type (an internal notice board, say) or a type's comments out of site search. The switches are stored in the type's `item_type.settings` under `search`, which config export and import already carry, and the form goes through the kernel's form pipeline with its CSRF and form build checks. Both backends honour them: the Pagefind export leaves the type's items, or their comments, out of the export, and `item_search_vector` (a migration replaces it) returns an empty vector for a switched off type and leaves comments out when they are switched off. Saving recomputes the type's vectors at once and requests a Pagefind rebuild for the next cron run.

The search architecture document now says what is indexed by default, what the two switches do, and that only content an anonymous visitor may see is in the Pagefind index.

The admin sidebar shows Comments and Categories again when those plugins are
enabled. Its link table tested the plugin names `comments` and `categories`,
but enabled plugins are held as `trovato_comments` and `trovato_categories`.
The comparison is exact, so both screens were hidden from everyone, and the
sidebar's own tests passed the same wrong names and passed. The names are now
the real ones, and a test holds every plugin the sidebar names to the list the
route gate is registered from.

The dashboard has one card per administration area: content, comments, people,
roles and permissions, structure, categories, menus, URL aliases, gathers,
tiles, plugins, site settings and reports. It used to link to content types,
the add forms and the health check, so most screens could be reached only by
typing their address. Each card's links pass the same gate and plugin check as
the sidebar, so a delegated role is shown the screens it may open and no card
for the rest.

The admin styles are no longer inline. `page--admin.html` links
`/static/css/admin.css` and `base.html` links `/static/css/base.css`, the
component sheet (variables, buttons, tables, messages) that used to sit in a
`<style>` block there. Both links carry `?v=` and the kernel version. A site
theme that replaces `base.html` used to lose every admin style with it: on one
site the sidebar's `--gray-900` was undefined and it drew white on white.
`admin.css` now imports `base.css` into a cascade layer, and layered rules
lose to every unlayered rule, so on the stock theme the copy changes nothing.
On a theme that never links `base.css`, the admin screens still get the
variables and components they are drawn with, and the theme's own CSS still
wins wherever it sets something. Nothing changes on the stock theme. A theme
that overrides `page--admin.html` itself should keep the `admin.css` link in
its `head` block.

`GET /admin/content/comments/settings` serves the comment moderation screen,
which holds the settings form, under `administer comments`. The route took only
POST, so a GET on the settings address answered 405.

The item add and edit forms, both the admin ones and `/item/add/{type}` and
`/item/{id}/edit`, have a "Publishing options" section: Promoted to front page,
Sticky at top of lists, Authored by (a username, checked against the users) and
Authored on (a date and time, read as UTC). Both forms used to send
`promote: None, sticky: None`, and `UpdateItem` had no author or date. So an
editor could not put an item on the front page or pin it, the author was always
whoever created the item, and the date was always when they saved it.

`UpdateItem` gains `author_id` and `created`, and `ItemService::create_authored`
takes an authored on time for a new item. `ItemService` decides all four on
every write path, as it decides the published state: `gate_promotion` admits a
holder of `publish content` or the new `administer content`, and
`gate_authoring` admits only `administer content`. A refused write is
`WriteDenied::Promotion` or `WriteDenied::Authoring`, a 403 on `/item` and an
error on the admin form. Asking for the value already stored is no change, so
an ordinary save by anyone still goes through. The form shows only the options
the user may set. `administer content` is granted by no migration; an
administrator holds it implicitly. Every save records a revision as before.

Gathers already accepted `promote` and `sticky` as filter and sort fields, and
the gather builder now suggests them with the other item columns, so a front
page gather can list promoted items with the sticky ones first.

`/admin/reports/cron` shows the last cron run: when it ran, on which server,
its result, how long it took and which tasks did work. A Run now button runs
cron in the server's own process, through the forms pipeline with its CSRF
token, and the report then shows that run. It takes `administer site`, as
`/cron/status` does. The last-run record gains `status`, `tasks` and
`duration_ms`; a record written by an older server still shows its result.

`/admin/reports/log` is the site log. A `tracing` layer writes warnings and
errors to a new `system_log` table, along with three kinds of `info` event:
sign-ins (success, failure and lockout, with the username), plugins enabled or
disabled (from the admin screen or the CLI), and config imports. Other `info`
events stay in the server log only. A field whose name looks like a secret
(passwords, tokens, keys, cookies, session ids, CSRF tokens, request bodies) is
never stored. Writes go through a bounded channel and are batched, so logging
never waits on the database. Cron prunes the table to whichever is smaller:
`SYSTEM_LOG_RETENTION_DAYS` (default 30) days or `SYSTEM_LOG_MAX_ROWS`
(default 100,000) rows. The screen pages 50 entries at a time and filters by
level and type. It takes `administer site`.

Maintenance mode is a setting on `/admin/config/site`, with an optional message.
While it is on, every request gets a 503 with the themed `maintenance.html`
page, `Retry-After: 300` and `Cache-Control: no-store`. The exceptions are a
signed-in user holding the new `bypass maintenance mode` permission
(administrators hold it), and `/user/login`, `/static/*`, `/health` and
`/cron/*`, so an administrator can still sign in and cron still runs. The
setting is read on each request, so every server in a cluster turns away
visitors as soon as it is saved. A site theme can override `maintenance.html`.
It extends `base.html` and receives `site_name` and `maintenance_message`.

Page views have a rate-limit bucket of their own. A `GET` or `HEAD` outside
`/api/` that no more specific bucket claims is now a `page` request, 600 a
minute by default, and is configured like every other bucket:
`TROVATO_RATE_LIMIT_PAGE`, `rate_limit.page`, and per role
`TROVATO_RATE_LIMIT_PAGE__<ROLE>`. Such requests used to fall through to the
`api` bucket at 100 a minute, shared with every API call from the same address,
so visitors browsing from a shared address (an office, a school, a carrier NAT)
were served a 429 for reading pages. `/api/*` keeps the `api` bucket. Sign in,
registration, password reset, search, uploads, comment writes and static files
keep their own. A request that is neither a read nor a POST (`PUT`, `DELETE`
outside `/api/`) is still counted as `api`. A site that raised
`TROVATO_RATE_LIMIT_API` to let readers browse can lower it again and set
`TROVATO_RATE_LIMIT_PAGE` instead.

Access rules turn away registrations and sign ins by username, email address or
client address, the way Drupal sites have long stopped spam registrations. A
rule is a type, a mask in which `%` matches any run of characters (`spam%`,
`%@example.com`, `203.0.113.%`), and allow or deny; masks match the whole value
and ignore case, and an allow rule overrides any deny rule of the same type, so
a site can deny a domain and still admit one address in it. Rules live in a new
`access_rule` table, which starts empty: an existing site blocks nothing until
it adds one.

Registration, from the form and from `/user/register/json`, checks the
username, the email address and the client address, and says which one is not
allowed. Sign in checks the username and the client address before the password
is verified, after the lockout check, and answers a refused attempt with a 403.
An administrator creating an account on `/admin/people/add` that a rule would
block is warned on the user list rather than refused.

`/admin/people/rules` lists, adds, edits and deletes rules, and has a box that
tests a value against them without registering or signing in. Every form on it
goes through the forms pipeline with its CSRF token. It takes the new
`administer access rules` permission, which no migration grants; an
administrator holds it implicitly. It sits under People in the admin sidebar and
on the dashboard.

Text formats are stored configuration with an admin screen. The three formats
(`plain_text`, `filtered_html`, `full_html`) were defined in code, so a site
could not change which tags `filtered_html` keeps or which roles may write in
it. They are rows of a new `text_format` table now, seeded by a migration with
exactly the code's definitions; a test renders the same inputs through the code
pipelines and through the stored rows and compares them, so an existing site
renders the same after upgrading as before.

`/admin/config/formats` lists the formats and edits each one's name, order,
allowed tags and attributes, line break and link conversion, and which roles may
use it; it adds formats and deletes any but the three built-in ones, whose
filter is also fixed. It takes the new `administer text formats` permission,
which no migration grants. A format's roles are its `use <format>` permission,
the same grant the permission grid shows (a site's own formats appear there
too), and a non-superuser may change it only for a format they may use
themselves. `<script>`, `<style>`, event handler attributes and `style` can
never be allowed.

Rendering reads the stored formats: the kernel's field rendering, the
`text-format.filter-text` host function and the item forms' format selector all
go through one in-process registry, loaded at start, reloaded when a format is
saved, and reloaded on every other server through a new `text_formats` cache
generation. A field offers only the formats the editor's roles may use, and a
save with any other is refused, as before. A format that does no filtering is
still never rendered from stored content: it shows as plain text, as `full_html`
always has.

Formats are a config entity, `text_format.<name>.yml`, exported and imported
before roles so a role file may grant a format the same set creates. An imported
format is checked by the same rules as the form.

A site can switch its theme from `/admin/appearance`. Until now the only way to
change how a site looked was to replace template files and restart.

A theme is a directory under the new `THEMES_DIR` search path (default
`./themes`, split like `TEMPLATES_DIR`, a later entry's theme replacing an
earlier one of the same name) holding a `theme.toml` with a name, a description
and an optional screenshot, a `templates/` directory, and a `static/` directory
served at `/static/themes/<name>/`. The stock theme is the template search path
the kernel has always used. Each theme is loaded at start as the stock search
path with the theme's templates laid over it, so a template the theme does not
provide is the stock one; a theme whose templates fail to load, or whose
manifest is missing, is skipped with a warning rather than stopping the server.

`/admin/appearance` lists the themes with their screenshots and sets the default
theme, which draws every page outside `/admin`, and the administration theme,
which draws `/admin`. Both default to the stock theme, so an existing site looks
exactly as before. The choice is stored in `site_config` (`theme_default` and
`theme_admin`), read once at start, applied by the template loader as soon as
it is saved, and on every other server through a new `themes` cache generation:
no restart. A stored theme that is no longer installed is the stock theme. The
screen goes through the forms pipeline and takes the new `administer themes`
permission, which no migration grants.

A site plugin can give its groups, clubs or projects vocabularies of their own,
administered by that owner's managers with the kernel's term list and term form.

A migration adds two nullable columns to `category`, `owner_type` and
`owner_id`, set together or not at all. Every existing vocabulary has neither
and behaves exactly as before. A vocabulary with an owner is listed at
`/admin/structure/categories` with its owner, and keeps that owner through
config export and import; an import naming only half an owner is refused.

The plugin surface (additive; no existing signature changed):

- `[[vocabulary_owners]]` in a manifest declares an owner type, the route its
  term administration is served under (`/<prefix>/{owner}/<name>`, outside the
  kernel's own paths), and optionally a permission whose holders administer
  every owner's terms.
- The new `vocabulary` host interface: `create-owned` creates a vocabulary for
  one owner of a declared type, and `list-owned` lists an owner's vocabularies.
  The SDK has `host::create_owned_vocabulary`, `host::list_owned_vocabularies`,
  `OwnedVocabularyRequest` and `Vocabulary`, with new error codes `-90`
  (owner type not declared), `-91` (invalid) and `-92` (id taken).
- The new `tap_vocabulary_owner_access` resolves the route's `{owner}` segment
  to an owner id and grants, denies or leaves the decision to the declared
  permission (`VocabularyOwnerAccessRequest`, `VocabularyOwnerAccess`).

Under the declared route the kernel serves the owner's vocabulary list, the term
list, and the add, edit and delete term forms. An anonymous user is sent to sign
in, a refused user gets 403, and a vocabulary or term of another owner is 404.

A term reference field can be limited to the vocabularies owned by the item's
owner with the field setting
`{"vocabulary_owner": {"owner_type": …, "owner_field": …}}`. The owner is the
value of `owner_field` on the same item. Every save then refuses a term from
any other vocabulary: on a form at the field, and through the API with a 422.

The term form, at `/admin/structure/categories` as under an owner route, no
longer accepts a parent term from another vocabulary. Its add form no longer
fails with a 500 on a hierarchical vocabulary that already has a term.

The plugin runtime runs on wasmtime 49.0.6, up from 49.0.5, which fixes eight
advisories published on 2026-11-16 against every earlier 49.x release.
In RUSTSEC-2026-0568, a stale GC heap base pointer in compiled code let a guest
abort the host process, and RUSTSEC-2026-0573 aborted the process when a GC
heap's reservation was not a multiple of the page size. RUSTSEC-2026-0567 and
RUSTSEC-2026-0570 aborted builds without the `threads` or the `gc` cargo
feature. RUSTSEC-2026-0569 panicked in `Caller::get_module_export` when a host
function was not called from WebAssembly, and RUSTSEC-2026-0572 panicked when a
component cancelled a stream or future copy on the wrong end. The last two,
RUSTSEC-2026-0566 and RUSTSEC-2026-0571, are unbounded host memory growth from
empty writes in `wasmtime-wasi` and `wasmtime-wasi-http`, neither of which the
kernel links.

The ones that matter here are those a plugin can reach: a guest able to abort or
panic the host takes down every site the process serves, not only the plugin.
The upgrade is a lock file change only. The workspace already asked for
wasmtime `49`, the whole release family (`wasmtime-*`, `cranelift-*` and
`pulley-*`) moved together to the 49.0.6 release and its cranelift 0.136.6,
nothing else in `Cargo.lock` moved, and no Trovato code changes. Plugins already
built for `wasm32-wasip1` load exactly as before.

**Security.** Two handlers still took the client address from `X-Forwarded-For` as sent, whoever sent it. The account deletion re-authentication form (`/user/delete`) keyed its password-guess limit on the header's leftmost entry and logged that entry with each failed guess, so whoever held a signed-in session (a borrowed laptop, a stolen cookie) could guess the account's password without limit by sending a new value with each attempt, and choose what the audit log recorded as their address. Its confirmation step, and the passkey rename, revoke and remove-password endpoints, wrote the header's value to the security audit log as the client address. All of them now use the address the kernel resolves once per request, which believes a forwarding header only from a configured trusted proxy. Nothing else in the kernel or the plugins reads a forwarding header: `client_addr::resolve_client_addr` is the one resolver, the `resolve_client_ip` middleware calls it once and stores the result as the `ClientIp` request extension, and rate limiting, sign-in lockout, access rules, the audit log and the session index read that.

`TRUSTED_PROXIES` takes CIDR ranges as well as single addresses (`10.0.0.0/8`, `fd00::/8`), so a proxy tier with changing addresses (a Kubernetes ingress, a load balancer pool) can be listed. An entry that does not parse now stops the server at startup with a message naming it. It used to be dropped without a word, which quietly stopped trusting that proxy and put every visitor behind it in one rate-limit budget. From a trusted proxy, `X-Forwarded-For` is still read from the right, skipping trusted hops; an entry that is not an address ends the walk at the last hop that was vouched for instead of becoming the client's identity, and `X-Real-IP` is used only when it parses and no `X-Forwarded-For` was sent. The RFC 7239 `Forwarded` header is not read.

A new setting, `CLIENT_IP_STORAGE`, decides how much of a client address is kept once the request is over: `full` (the default, and what every site did before), `truncated` (the IPv4 /24 or IPv6 /48) or `none`. It applies to the security audit log (NULL under `none`), the audit log plugin's table, the lockout warning in the site log, the device list's session index and the password reset log lines. Rate limiting and lockout keep using the whole address under every setting, in Redis counters that expire with their window, so a site that stores nothing still tells its visitors apart. A privacy-minded site no longer needs to rewrite `X-Forwarded-For` to a constant at its proxy, which also collapsed every visitor into one rate-limit budget. `INSTALL.md` has a "Behind a reverse proxy" section with the one line a proxied deployment adds, and a "Client addresses and privacy" section listing what each value stores.

The front page renders any configured path at `/`, with no redirect. A gather
listing, a plugin route or an alias set as `site_front_page` used to answer `/`
with a 307 to itself (only an `/item/{uuid}` was rendered inline, by a second
copy of the item route's rendering), so a site whose front page was a listing
showed its internal path in the address bar. `/` is now rewritten to the
configured path before routing, the way Drupal's `system.site:page.front`
works, and served by the same router, middleware and handler as that path:
the same access checks (an anonymous visitor who may not see the target gets
its login redirect or 403), cache headers and query string (`/?page=2` is page
two of a gather front page). `/{lang}` is the front page in that language. The
page carries `<link rel="canonical">` to `/`, and an item front page offers its
translations at `/` and `/{lang}/` as before. A target the site answers 404 for
(a deleted item) falls back to the promoted listing with one log warning. The
path is cached in memory under a new `site_config` cache generation, so `/`
costs no extra database query; the site settings form refuses a path that does
not resolve. A theme's `page--front.html` now applies to the default listing
only: a configured front page uses its own path's layout.

Search result snippets show their text once, with the matched terms
highlighted, whatever the stored body holds. A body written as HTML came out as
`&lt;p&gt;Tom &amp;amp;amp; Jerry …` on the results page, so a site had to undo
the escaping in its own template. The snippet was escaped twice: the search
query HTML-escaped the title and body before `ts_headline` (the XSS-3 fix), and
the route escaped the whole headline again in `sanitize_snippet` before the
template printed it `| safe`. Neither step stripped the markup, so the tags the
first escape had turned into text were escaped a second time and shown. The
route's escape also restored any `<mark>` it found, so a body holding a literal
`<mark>` got a highlight the kernel never made.

There is now one rule, in `crate::search::snippet`. The query strips HTML tags
from the stored title and body before highlighting, not from the highlighted
result, and `ts_headline` marks matches with two fixed private-use characters
that the query also removes from the source, so stored content cannot forge
one. `snippet::render` then splits the headline at those markers, takes the text
of each piece with the kernel's existing extractor (the ammonia-based one the
meta description and the Pagefind export use), escapes it exactly once, and
turns only the markers into `<mark>`. `SearchService::search` is the one place a
snippet is built, `templates/search.html` the one place it is printed unescaped,
and `sanitize_snippet` is gone. The JSON endpoints (`/api/search`,
`/api/v1/search`) carry the same snippet. No query was added.

The blog listing's teaser had the same fault from the other side: it printed the
stored HTML body through `truncate`, so autoescape showed its tags and turned a
stored `&amp;` into a visible `&amp;`. It now goes through a new `plain_text`
Tera filter, the same text extraction, so the teaser is text escaped once.

A site served over plain HTTP at a LAN address can be logged in to.
`create_session_layer` always marked the session cookie `Secure`, and a browser
drops a `Secure` cookie from a plain HTTP page (except on `localhost`), so the
login answered, the cookie was thrown away, and the next page was anonymous
again. The new `COOKIE_SECURE` setting decides the flag: `always`, the default
and what every existing site keeps, or `never`, for a plain HTTP LAN or
development site only. The session cookie is the only cookie the kernel sets
(CSRF tokens and "remember me" live inside the session), so it is the one
place the setting applies. It is read once at startup.

`never` cannot reach a public site quietly. It logs a warning naming
`COOKIE_SECURE` on every start, the site settings page shows it under "Session
cookie" with a warning, and it is refused at startup when `SITE_URL` is
`https://` or `COOKIE_SAME_SITE` is `none` (browsers reject a `SameSite=None`
cookie that is not `Secure`). Any other value is refused too. There is no
`auto`: the flag is fixed when the session layer is built, and choosing it per
request from the scheme would rest on a forwarded header. `INSTALL.md` has a
"Serving over plain HTTP" section saying when to use `never` and why it is
unsafe on the internet. `HttpOnly` and `SameSite` are unchanged.

Saving an item without changing anything no longer changes what it stores. Every field type now survives load, edit and save unchanged through the item edit form, the admin content form and the JSON edit route, so a field's shape no longer depends on which form saved it last.

B71 put both forms on one pipeline, but the field types still had no single owner. The form builder read a stored value tolerantly: a bare scalar or a `{"value": …}` wrapper, a reference as a uuid or as `{"target_id": …}`. The submission builder wrote back only the canonical shape. One save with no edits therefore rewrote `{"value": "Barga"}` as `"Barga"` (how `field_city` is stored on the `conference` items in the local development databases), a reference object as a bare uuid, and a long text without its `summary`. It gave a plain string body a `filtered_html` format it had never rendered with, deleted an integer held as a string, unticked a Boolean held as `"1"`, emptied a date held as a timestamp, and stored a textarea's newlines as the CRLF a browser submits. Fields the item never had were added as `""`, `[]` or `{"sections":[]}`. The publishing options also moved the item's `created` time back to the minute, because the "Authored on" input dropped its seconds.

Both directions now live in `content::field_value`, matched on the field type: what a widget shows for a stored value, and how a posted value goes back into that stored shape. On an edit, a value that comes back as the widget showed it keeps the stored value byte for byte, and an absent field stays absent. A changed value is written into the stored shape: a wrapper stays a wrapper, a reference object keeps its other keys, and a long text keeps its `summary`. The JSON edit route checks every field it changes with the same reading and answers 400, naming the field, for a value of a shape the type cannot read or a text format the writer may not use. A value sent back exactly as stored is accepted whatever its shape. A long text in a format the editor may not use is shown disabled and kept, instead of a selector that could not show its format and refused the save. The "Authored on" input carries seconds when the stored time has them.

No stored data needed repair. A query for the marks the pre-B71 forms left (the literal `[object]` or `[object Object]`, and a long text stored beside a `<name>_format` key) found none in any database this kernel can reach. The wrapped shapes that do exist were written by importers and plugins, and are now kept as they are.

A gather feed now gives a reader what it needs to follow a site: each entry
names its author and its taxonomy terms, keeps one id for life, carries a
plain-text summary, and every gather feed is served as Atom 1.0 as well as RSS
2.0, with `ETag` and `Last-Modified` so a repeat poll is answered `304`.

The feed renderer wrote title, link, description and date, nothing more. The
`guid` was the link, so moving a post to a new URL alias made every reader show
it again as new; the id is now `urn:uuid:<item id>`. The description read a
top-level `summary` or the body off the row, but a gather that selects the whole
item nests its fields under `fields`, so most entries had no description, and
the ones that did carried the stored HTML whole. The summary is now the item's
summary field when it has one, otherwise its body stripped of markup and cut at
a word boundary, extracted and escaped once by the rule B123 gave search
snippets. A new `full_content` option adds the body, rendered through its text
format, as `content:encoded` and Atom `<content type="html">`. Authors (`dc:creator`,
never an email address) and terms (`<category>`) are loaded in one batched
query each; the alias lookup the feed did for itself is gone, since the gather
service already puts the alias on every row.

The Atom document is at the RSS path's `.atom` sibling (`/blog/feed.xml` and
`/blog/feed.atom`), derived so that every existing feed gets it with no config
change, and both are advertised in the page head. Every absolute URL comes
from `SITE_URL`, and the kernel now logs one warning at startup when `SITE_URL`
is unset or a loopback host. The query still runs as the requesting viewer on
every request, so an anonymous feed is `Cache-Control: public, max-age=300` and
one fetched with a signed-in session is `private`.

One gather feed declaration can now serve a feed per author and per tag. A feed
path may carry one argument (`/blog/author/{user}/feed.xml`,
`/tag/{term}/rss.xml`), and `paths` lists more addresses for the same feed, so
the stock blog gather serves `/blog/feed.xml`, `/blog/author/<name>/feed.xml`
and `/blog/tag/<slug>/feed.xml`, each with its `.atom` sibling, from one
declaration.

A feed path was refused if it contained `{`, because the feed handler had no
way to fill an argument, so the only way to get a per-author feed was a second
gather per author, which is what the jibnotes, rudere and KernelTrap ports did.
The argument is now bound by the gather's own `display.routes` mapping with
the same segment name, resolved by the code the gather's page route uses: that
loop moved out of `handle_gather_route` into
`gather_routes::resolve_route_params`, which both call. It gained one mode,
`user: true`, which resolves an account name to the active account's id. An
argument that names nothing answers 404 rather than an empty feed: an unknown
slug or account name fails its lookup, and a passed-through value that matches
nothing the viewer can see is treated the same way. The page reached with an
argument (`/blog/author/<name>`) advertises the feed with the same argument in
its autodiscovery links; argument feeds are not added to every page's head.

`trovato_blog` gets migration `002_author_and_tag_feeds.sql`: two contextual
filters on `blog_listing` (author and tag, each a no-op when absent, so `/blog`
is unchanged), the routes `/blog/author/{user}` and `/blog/tag/{tag}`, and the
feed declaration. Every feed still runs the query as the requesting viewer.

A feed also now carries its own `items` count. It executed page one of the
gather, so a feed declaring `items: 20` over a gather showing ten a page
carried ten; it now asks the gather for exactly `items` rows.

A config export never contains a secret, so the directory can be committed.

PR 201 (B92) exported each plugin config row as Postgres returned it, with no
notion of a secret column, so `webhook.<pk>.yml` carried the webhook signing
secret in the clear and `oauth_client.<pk>.yml` the client secret's Argon2 hash;
the variable `plugin.trovato_captcha.turnstile_secret_key` went out the same way.
A manifest now names its secret columns under `[config] secret_columns`
(additive; plugin API 0.105), and `trovato_webhooks` and `trovato_oauth2`
declare theirs. Export writes `env:TROVATO_SECRET_<TABLE>_<PK>_<COLUMN>` in
place of each secret, or nothing when it is empty; secret `site_config` keys
(`SECRET_VARIABLE_KEYS`) follow the same rule. Import keeps a secret the target
already holds, else reads the named variable (hashing it for a `_hash` column),
else refuses the file and writes nothing; a secret in the clear is refused. A
config table column whose name says it is a credential is treated as secret
even when undeclared. A test exports a site with a webhook and an OAuth client
and fails if any file holds either secret or the hash.

Logging out now removes the device from the account's session list. Logout
ended the session with `Session::flush()`, which deletes the session record from
Redis but not the device's entry in the per-user session index,
`user_sessions:<uid>`. The session tracking middleware, which owns every other
write to that index, could not clean up after the handler either, because a
flushed session no longer names a user. So the logged out device stayed on
`/user/sessions` for every other device of the account until the index expired.
Logout now reads the device id before flushing and removes the entry through
`SessionRegistry::revoke`, the same removal a revocation from another device
uses. A session that never reached the index logs out exactly as before.

The same gap existed on two other paths that end a session. A blocked account's
session, ended on its next request by `session_user` or `get_user_context`, was
flushed the same way and left its entry behind; it now leaves the index too. An
administrator deleting an account through `/admin/people/{id}/delete` ended none
of that account's sessions at all, so its devices stayed listed and its index
went on being refreshed; the deletion now revokes every session of the account,
as self-service deletion already did. The work happens only when a session
ends: ordinary page views make no extra Redis call.

The Logout control in the stock theme's header now looks like the header links
beside it. Logout is a POST form with a CSRF token, so `templates/page.html`
renders it as a `<button>` carrying both `site-nav__link` and
`site-nav__logout`. The `.site-nav__logout` reset in `static/css/theme.css`
came later in the sheet than `.site-nav__link`, at the same specificity, and
declared `font: inherit`, `color: inherit` and `padding: 0`; those won over the
link style, so Logout took the list item's 16px regular text and darker colour
instead of the links' 14px medium grey, and lost their padding and hover pill.
The reset now takes away only what a browser gives a button and not a link
(border, background, margin, appearance, font family and line height) and sets
nothing the link style sets, so the button gets the links' size, weight,
colour, padding, hover and focus ring at every width. Logout is still a POST
with its CSRF token; the template is unchanged.

A category term reference field (`RecordReference("category_term")`) can now
be filled in by hand. Its widget, `static/js/record-ref.js`, asked
`/api/v1/items/autocomplete` for suggestions with the field's target type, and
that endpoint only ever searched the item table for items of that type. Terms
are not items, so for `category_term` it found nothing and the box offered
nothing, on every term field, owned vocabulary or not. Saving was unaffected.

The endpoint now answers a `category_term` target from the terms, in the same
`[{"id", "title"}]` shape and with the same `limit`, by a case-insensitive
prefix match on the term name through `CategoryService::search_tags`, which
reads the cached per-vocabulary term lists. The widget sends the field it is
filling in (`item_type`, `field`), the item when it edits one, and the form's
owner value for a field limited to the item's owner. A caller is offered terms
only if they may edit that field as the item form decides it: the type's
create permission or edit access to the item, plus the field's own view and
edit decisions. Anyone else, anonymous included, gets an empty list. A field
limited by `settings.vocabulary_owner` searches only the vocabularies its save
check accepts; the check and the search share one rule,
`OwnerLimit::admits`. An open term field searches the site-wide vocabularies
and never an owner's. Item autocomplete for every other target is unchanged.

Reopening an item also showed a blank box for a term field, because the edit
form looked titles up in the item table only. It now shows the term's name.

A template with a syntax error no longer turns the whole site into
untemplated output. Every template was loaded into one Tera instance with
`add_template_files`, so a parse error in any single file failed the whole
load; `AppState` then logged one warning ("failed to load templates, using
empty engine") and carried on with an engine that had no templates at all, and
the error itself said only "failed to load templates from the search path".

Each template on the search path is now parsed on its own before Tera loads
them together, so the error names every file that does not parse, by path, with
Tera's message and the line and column it points at. A kernel, plugin or app
template that does not parse stops startup: the error is logged once at
`error` and the process exits with it. The empty-engine fallback is gone;
`ThemeEngine::empty` remains for tests.

A deployment theme (`THEMES_DIR`) whose templates do not parse is refused
instead: logged once at `error`, so it is in the site log, and the site keeps
running on the stock theme. `/admin/appearance` lists it as a theme that
cannot be activated, with the file and Tera's message, and choosing it is
refused with that reason while the active theme stays as it was.

Template names, load order and override rules are unchanged, and the check runs
only when templates are built, never per request.

The `url` crate is upgraded from 2.5.8 to 2.5.9, which fixes RUSTSEC-2026-0592: `Url::origin()` on a deeply nested `blob:` URL overflowed the stack, which aborts the process instead of returning an error. Every build of Trovato from `Cargo.lock` carried 2.5.8, and the live security audit reported it as a vulnerability. Operators building from source should rebuild; the change is a patch release inside `url` 2.x, needs no configuration, and moves nothing else in the lock file except `form_urlencoded` 1.2.2 to 1.2.3, which `url` 2.5.9 requires.

**Security.** `X-Forwarded-Proto` is believed only from a proxy listed in `TRUSTED_PROXIES`, the rule `X-Forwarded-For` already followed. The security headers middleware decided a request had arrived over HTTPS, and so sent `Strict-Transport-Security`, from that header whoever sent it: it was the one forwarding header the kernel still read without the trusted-proxy gate. The effect was small, since a direct client only changed the headers sent back to itself and a browser ignores HSTS received over plain HTTP, but it was the last reader the gate did not cover. The decision now goes through `client_addr::forwarded_https`, beside the client address resolver: the socket peer must be a trusted proxy, and the value that proxy wrote, the rightmost when there are several, must be `https` (in any case). A site behind a TLS-terminating proxy that does not list it in `TRUSTED_PROXIES` stops sending HSTS until it does; `UPGRADING.md` says how to check. `COOKIE_SECURE` is unchanged and still does not look at the header.

`full_html`, the text format with no filter configured, was never rendered
from stored data: every view path downgraded it to `plain_text`, so a body in
`full_html` showed as escaped text instead of the rich HTML the `use full_html`
permission was meant to let an author write. The downgrade existed because the
format's filter was genuinely raw — whatever an author wrote came back
unchanged — and no render path could tell a value an author wrote in `full_html`
from one that reached storage some other way holding that format name.

A format with no filter configured now renders as rich HTML, cleaned by one
fixed, permissive sanitizer (`TrustedHtmlFilter`, built on ammonia) rather than
escaped or passed through raw. It keeps ordinary article markup — `div`,
`span`, `figure`, tables, headings, lists, images, `class` and `id` on every
tag, `target` on links — and always strips `<script>`, `<style>`, every `on*`
event handler attribute and a `javascript:` or `data:` URL, whatever wrote the
value and however it reached storage. `FilterPipeline::for_format_safe`, which
existed only to downgrade an unsafe format, is gone: `FilterPipeline::for_format`
is now safe to call with any format string, including one a user, a plugin, or
stored data supplies, since every pipeline it builds is sanitized before it is
returned. A `custom_html` tile's body goes through the same sanitized path as
every other render, rather than trusting the tile's own `has_full_html = true`.
Theme templates no longer need `| safe` on a `text_format`-filtered value: the
filter now reports its output as already safe, since it is sanitized by
construction.

`plain_text` and `filtered_html` render exactly as before.

The revision history of an item says which revision is current, deletes a
past revision, and opens to a user who holds `view revisions` without being
able to edit the item. The list marked the current row only with a small
"current" badge, offered no delete at all, and asked for edit access before
showing anything, so a role that Drupal lets read the history (`view
revisions`) was refused with 403 when it edited the item through a plugin's
own form or not at all.

Two kernel permissions are new, both with Drupal's names so a migrated role
keeps them, and granted by no migration: `view revisions` reads the history,
a past revision and a comparison of two for anyone who may view the item, and
`delete revisions` deletes a past revision for a user who may also delete the
item (`POST /item/{id}/revisions/{rev}/delete`, answered 409 for the current
revision, which is never deleted). `ItemService::delete_revision` is the one
path, and records an audit row. Revert is unchanged: it is an edit of the item,
so it needs edit access and is offered only to a user who has it.

The content list promotes a selection to the front page, removes it again,
and makes it sticky or not, and a content type can promote (and pin) its new
items by default. Items had the flags and the item forms set them since B107,
but `bulk_content_action` knew only publish, unpublish, delete and move, and a
new item was promoted only if its author ticked the box: the type form had no
default, where Drupal's Article and Blog types promote theirs, so a new post on
a migrated site never reached a front page or feed that lists promoted items.

The four bulk actions (`promote`, `demote`, `sticky`, `unsticky`) go through
`ItemService::update` like the others and are refused, before any write, to a
user holding neither `publish content` nor `administer content`, the rule
`gate_promotion` applies to one item. The type form's publishing options gain
"Promoted to front page by default" and "Sticky at top of lists by default",
stored as `promote_default` and `sticky_default` in the type's settings;
`ItemService::create` applies them when a write names no value, whoever makes
it, and the add form starts from them.

The item forms preview: a Preview button beside Save renders the submission as
the item page would, above the form shown again with what was posted, and
saves nothing. A content type's "Preview before submitting" setting
(`preview_mode`) makes it optional (the default, as in Drupal), disabled, or
required, in which case the form offers Save only under a preview and a save
posted without one is refused. The forms had Save and nothing else, so an
editor could not see an item before it was stored, and a migrated site whose
types required a preview lost the step.

One path for all four forms (`/item/add`, `/item/{id}/edit` and the two admin
forms): `item_form::build` adds the button and withholds Save, the form state
of a form built under a preview records it, `item_form::process` refuses a
required type's save without it, and `item_form::render_preview` renders the
fields through the item page's own renderer, cut to what the user may see,
with the title escaped. A preview runs the same validation as a save and
nothing after it: no write, no tap, no lock change.

Content moderation: a content type can be moderated, and its items then move
between Draft, Published and Archived by transitions each role is granted,
with a draft of a published item waiting beside it while visitors keep the
published version, and a moderated content list at `/admin/content/moderated`.
There was no per-item workflow at all. An item was published or not, the
editorial workflow in `models::workflow` moves items between site-wide stages
rather than through states of one item, an author who could publish published
on save, an edit of a published item went live at once, and there was nothing
for an editor to review.

A type is moderated when its settings say `moderation: true`, a checkbox on
the content type form, off by default. The workflow is Drupal's Editorial one,
fixed: Create New Draft (Draft or Published to Draft), Publish (Draft or
Published to Published), Archive (Published to Archived), Restore to Draft and
Restore (Archived to Draft or Published), each granted by its own permission
with Drupal's name (`use editorial transition publish`), so a migrated role
keeps its grants; `view latest version` is new too. `content::moderation` is
the pure rule; `ItemService::create_moderated` and `update_moderated` apply it
on every write path, the item forms, `/item` JSON, the admin forms, bulk
actions, revert and plugins alike, and the state decides the published status,
so on a moderated type the transition permission replaces `publish content`.
A refused transition is `WriteDenied::Transition`, a 403.

Each revision records its state in a new nullable column,
`item_revision.moderation_state`; nothing is backfilled, and a revision saved
before its type was moderated reads as Published or Draft from its status. The
item row stays the revision visitors see: a Draft saved over a published item
writes only a revision, so the read path is unchanged and asks nothing more.
The edit form works on the latest revision and offers a "Change to" select of
the states the user may reach in place of the Published checkbox;
`/item/{id}/latest` shows a pending draft to a user with edit access, `view
revisions` or `view latest version`; the revision list shows each revision's
state; and the moderated content list, for `view latest version` or `edit any
content`, lists every moderated item whose latest revision is a Draft or
Archived.

A draft over a published item does not yet move the item's file references,
alias, promote or sticky flags or stage: those follow when the draft is
published.

The revision history listed revisions by `created` alone, which holds whole
seconds, so a draft saved in the same second as the published revision before
it could be listed below it while `/item/{id}/latest` (ordered by `created`
then id) showed it as the latest. Both now break the tie by the later id.

Comments have author pages: `/comment/reply/{item}` and
`/comment/reply/{item}/{comment}` are the reply form (the second with the
comment being answered shown above it), `/comment/{id}/edit` edits a comment's
text, and `/comment/{id}/delete` is a confirmation page whose form POSTs with
the CSRF token and deletes the comment and its replies. Each comment on the
item page links Reply, and Edit and Delete where the viewer may use them, so
replying no longer needs JavaScript and a member can correct or withdraw what
they wrote. Before, the only edit and delete controls were on the
administrator's moderation screen. Editing your own comment is `edit own
comments`; deleting it is the new `delete own comments`, which no role holds
until it is granted, so a site's members still cannot delete unless it says
so. A visitor who is not signed in is sent to the login form and brought back
to the reply page; a member without `post comments` gets 403.

The comment API's PUT and DELETE refused a moderator who held `administer
comments` but was not the site administrator: the access rule asked for `edit
any comment` and `delete any comment`, two permissions nothing declares and so
no role could be given from the permissions page. Editing or deleting someone
else's comment is now `administer comments`, the permission the moderation
screens already require; the two old names are still honoured where a site
granted them by hand. A comment also counts as the viewer's own only when the
viewer is signed in, so an anonymous visitor does not own an imported
anonymous comment that shares the nil author id.

Site search works for the query a visitor types, shows its results with
JavaScript on or off, and writes them once.

A query holding `&`, markup or a stray quote answered 500. The search built its
`to_tsquery` text by hand, appending `:*` to each whitespace-separated word, so
punctuation in the query was tsquery syntax. The words now go to
`websearch_to_tsquery`, which never fails to parse: every word must match,
"a quoted phrase" matches as a phrase, `or` and a leading `-` work as they did
on Drupal. Markup in the query is reduced to its text first, the way the
indexed text is, so `<b>wscci` searches for `wscci`. A query left with no word
to search for (`a`, `it's`, `&`) says so on the page instead of only "no
results". The count query reports that, so no query was added.

With JavaScript on, a visitor never saw a result: `search-init.js` hid the
server-rendered results as soon as `scolta.js` loaded, before knowing whether
the Pagefind index existed, and on a site with none built
`/pagefind/pagefind.js` answered 404 and left an empty box. The server results
are now hidden only once Pagefind has loaded; until then, and for good if it
cannot, they are the page. `templates/search.html` wrote those results twice,
in a `<noscript>` and again in the fallback block, both visible with JavaScript
off; they are written once.

On the Pagefind path every excerpt was escaped twice. Pagefind's `excerpt` is
HTML, its text already escaped with `<mark>` around the matches, and
`scolta.js` ran `escapeHtml(stripHtml(…))` over it, so "Tom &amp; Jerry"
showed its entity. `scolta.js` now reads every Pagefind value as HTML into
text once, and escapes once when it writes, highlighting on the text rather
than on escaped HTML; its `escapeHtml` escapes quotes too, since it fills
attribute values. Tests for it run on Node's own runner in a new
`Static JavaScript` CI job.

Searching asks for a permission now, `search content`, Drupal's name for it, so
a site can withhold search from a role. A migration grants it to every role
holding `access content`, so an existing site searches as before.

A gather can count `?page=` from 0, the way Drupal's pager did, so every paged
link a migrated site published lands on the same items; a page past the end of
a listing is a 404; and a gather reached at an alias pages on that alias.

Drupal numbered its pager from 0 (`?page=1` was the second page) and Trovato's
gathers from 1, so every old paged link landed one page off. Which count a
listing uses cannot be guessed from the request, because `?page=1` means a
different page under each, so it is the gather's: `display.pager.zero_based`,
set by the gather that replaces a Drupal listing and offered in the gather
builder. A zero-based gather reads `?page=N` as page N+1 and writes its own
pager the same way, with no `?page=` on the first page, so its links and the old
site's agree. Every other gather is unchanged: `?page=N` is still page N, and
every link it has published still works. The pager template now links with
URLs the kernel computes (`pager.links`, `prev_url`, `next_url`, `first_url`,
`last_url`); `pager.pages` and `page_url_prefix` stay for themes written
against them, and pager links keep their filter parameters in a fixed order.

A page past the last answered 200 with an empty listing, which a search engine
reads as a soft 404. It is now a 404, except on the front page, where a 404
falls back to the stock front page.

A gather reached at a URL alias with no `canonical_url` paged to its internal
address: at `/blog`, the pager linked to `/gather/blog_listing?page=2`.
`render_query_html` built its link base from `canonical_url` or the gather id
and never looked at the address the visitor used. It now uses that address
when there is no canonical URL. No query was added: the pager settings come
from the in-memory gather registry.

Term pages are titled by their term, the site's items per page reaches the
listings that do not set their own, and an item's views can be counted and
shown as "N reads".

A gather route whose path names a tag (`/recipe-category/{slug}`) titled its
page with the gather's label, so every term page was called "Recipe category".
The route already looked the tag up to filter by it, then dropped it. It now
keeps the tag it found: the page title, the breadcrumb and the listing heading
are the term's name, and the term reaches the templates as `term`. The shipped
blog listing template heads `/blog/tag/{tag}` with the tag's name instead of
"Blog". No query was added.

The "Items per page" on `/admin/config/site` was stored and read by nothing:
every gather had its own `items_per_page`, defaulting to 10 when its definition
left the key out. `display.items_per_page` is now optional. A gather that names
no size pages by the site's setting, so a migrated listing that Drupal paged by
"posts per page" follows it. The gather service caches the setting and drops it
on a `SiteConfig` cache generation bump, so a listing pays no query for it while
the cache is warm. A gather that names its own size is unchanged. The gather
builder leaves the field blank to mean the site's setting.

Items had no view counter (Drupal's statistics "N reads"). "Count content
views" on `/admin/config/site`, off by default, counts each item view in a new
`item_counter` table: one upsert per view, which also returns the total. The
total is shown under the item's title to roles holding the new
`view post access counter` permission. While counting is off, an item view
costs no query for it, because the setting is cached the same way.

Old links that carry a query string reach their page, and a feed's item count
and full text are set in the gather builder without the save losing the feed.

A Drupal site without clean URLs published every link as `/?q=node/96`. On
Trovato that was the front page, because `check_redirect` matched the path
alone and skipped the site root. A request for the root, or for `/index.php`,
whose `q` names a site path now follows that path's redirect in one hop when
there is one, and otherwise goes to the path itself, keeping its other
parameters. A `q` that could leave the site (`//host`, a scheme, a backslash, a
control character) is ignored. And a redirect dropped the request's query
string, so `/old?page=2` landed on `/new`. It now lands on `/new?page=2`. Only
requests with `?q=` at the root take the new branch, so no query was added
anywhere else.

The gather builder edits a feed's number of entries and whether each entry
carries the full text (`display.feed.items` and `full_content`, from B127),
which are the RSS publishing settings a Drupal site had. Before this, a save
through the builder erased every display key the builder does not show: it
posted only its own keys, and the save handler stored exactly what was posted,
so saving a gather there dropped its feed, its routes, header and footer. The
save now lays the submitted display over the stored one: a key the form does
not send keeps its value, and a key sent as `null` is cleared.

A plugin's own `/admin/...` page now renders with the kernel's admin sidebar,
and a plugin can add its own sidebar link and dashboard card to it.

`routes::plugin_api::themed_response`, the path a plugin's themed page takes,
built the page context without ever setting `admin_nav`: only
`helpers::render_admin_template` and the themed error page did. An admin page
served by a plugin therefore rendered `page--admin.html` with an empty
sidebar, and a site worked around the gap by hand-copying the kernel's link
list into its own template, shown to anyone admitted to `/admin` whatever
permissions they actually held. All three paths now build the sidebar through
one shared helper (`routes::admin_nav::insert_context`), so a plugin's admin
page and a kernel one cannot drift apart, and the workaround is no longer
needed. The page also gets the same header a kernel admin page draws, from the
title the plugin returns with `ApiResponse::themed`.

The sidebar and dashboard were also fixed Rust arrays a plugin could not add
to. A plugin now declares its own links and cards from a new tap,
`tap_admin_nav`, mirroring `tap_perm` and `tap_menu`: a declaration collected
once at boot and merged into the sidebar and dashboard under the same
permission filter the kernel's own links use, so a viewer never sees a link
or a card for a screen they cannot open, and a disabled plugin contributes
neither.

A comment can now have an optional plain text subject, shown as a heading above its body and editable wherever the body is.

The `comment` table had no column to hold one, so a site importing comments with titles (Drupal's comment entity has a subject field) had nowhere to put the title but inside the body — which changed the body's text and left the title unable to be edited on its own. `comment.subject` is nullable, trimmed on save, stored as `NULL` rather than an empty string, and capped at 255 characters (checked in Rust before it ever reaches the column's own `CHECK` constraint). It is plain text: rendered with autoescape on, never through a text format, the same rule a comment's author name already follows.

The subject is searchable at the same weight as the body, in both the Postgres vector and the Pagefind export, and a comment with none is indexed exactly as it was before this. The notification mail uses a comment's subject as its subject line when it has one, falling back to the text it has always used otherwise.

A redirect's source can now carry a query string. `/article.php?sid=1` and
`/article.php?sid=2` were the same page to `check_redirect`: it matched
`request.uri().path()` alone, so two different legacy pages that only differed
by query could not redirect to two different destinations — only ever one
redirect for `/article.php`, or none, answered every query. A source with a
query now matches only a request carrying that exact query; a request whose
query matches no stored source falls back to a path-only redirect exactly as
before, so an existing site's redirects behave byte for byte as they did.

The query is normalized at write time — its parameters sorted and its
percent-encoding made canonical — and stored in the same `source` column, so
`?a=1&b=2` and `?b=2&a=1` are the same redirect, `q=a+b` and `q=a%20b` are the
same redirect, and matching stays one indexed lookup (two when the request
carries a query: the exact source first, the bare path second). The admin
form and the JSON API (`/admin/config/redirects`, `/api/v1/redirect`, both new
— `trovato_redirects` declared a menu entry and a config table but no route
ever served them) show a source back as it was stored, not as it was typed,
and refuse one with a fragment.

Two operator commands. `trovato user create <name> --mail <address>
[--role <role>]... [--blocked] [--no-password]` creates an account with the
admin "Add user" form's checks and through the same user service, so plugins
see `tap_user_register` and the audit trail records the account and each role
grant. The password is read from stdin, with echo off at a terminal, and
`--no-password` leaves an account that cannot sign in until a reset. A taken or
empty name or address exits 1 and creates nothing; a role that cannot be
granted once the account exists leaves the others granted, exits 1 and prints
the `role-add` commands that finish it. `trovato cache clear [--kind <kind>]...`
tells every running server to drop its in-memory caches and returns once they
have read the bump, so a change written straight to the database is served on
the next request rather than after the cache's time limit or a restart. Sites
importing accounts had been inserting into `users` by hand, and sites writing
by hand had been restarting their servers.

`cache clear` is the one invalidation path of the command line: `config
import`, `generate` and the `plugin` and `user` commands that write now end with
the same function rather than their own bump, and so also return only once the
servers have cleared. Clearing `site_config` also drops the search settings'
copy in Redis, the one value read from `site_config` that no cache generation
reached, so a search settings change made by import or by hand is no longer
served stale for up to five minutes. A new account created with an empty
password now stores no password hash instead of the hash of the empty string;
no existing path created one. `user reset-password` now reads a password typed
at a terminal with echo off too, where it used to show it as it was typed.

An item page and a gather listing row now carry their author's display name,
and a profile link when the viewer may see one. `view_item` put `item`
(carrying only `author_id`), `viewer` and the rest in the template context but
never a name, so a theme had to hardcode "by Jeremy" or print nothing at all;
`/api/item`'s `include=author` was the only read path that resolved one. The
item page and a gather row now resolve it through one function,
`UserService::author_views`, which reads the same user cache `/api/item`
uses: an author the cache holds costs no query, a cold item page pays the one
`find_by_id` any author lookup would, and a cold gather loads all of its
uncached authors in one query, never one per row. Warm pages run no more
queries than before; cold ones run one more. The shipped `item.html`,
`item--blog.html`, `gather/row.html` and blog listing templates print
`by {{ author.name }}`, linked to `author.url` only when it is set. A deleted
account's content is reattributed to the kernel's anonymous account, which
shows its own name and is never linked; an id matching no user row at all
shows the kernel's anonymous display name. Only the display name and an
access-checked URL reach a template, never the mail address or anything else
a `User` record carries. A gather that defines its own include named `author` keeps
that include's result; the byline never replaces it.

Deleting a past revision of an item (B158's `delete_revision`) is now reachable
from a confirm page, not just a one-click form on the revision list: the
revision list's Delete link opens `/item/{id}/revisions/{rev}/delete` (GET),
which asks the same `delete revisions` and delete-access questions the POST
does and answers them the same way, before it offers the button. An operator
reaches the same path from the command line, `trovato item revision-delete
<item> <revision>`, with the administrator bypass every other `trovato`
command already runs with.

The current revision was already refused by `ItemService::delete_revision`
(409); it turns out the database already refused it too, independently, since
the item table's original migration: `item.current_revision_id` is a foreign
key into `item_revision` with no `ON DELETE` action, so a `DELETE` reaching
Postgres by any other path — a script, a future bypass of the service — fails
with a foreign key violation rather than orphaning the item.

A delete also bumps the item's `Items` cache generation, which it did not
before: nothing served a stale revision list from it (revisions are not
cached), but `item_revision` is part of what that generation covers, and every
other write to an item or its revisions already bumps it.

A gather feed's RSS `<link>` and Atom `<link rel="alternate">` now point at the
URL alias of `/gather/{query_id}` when one exists and the gather has no
`canonical_url`, instead of `/gather/{query_id}` itself. The stock blog's
channel link is `/blog`, the address every reader and aggregator already uses,
not `/gather/blog_listing`.

The cause: the feed's channel link was built from the argument's own route,
else `canonical_url`, else `/gather/{query_id}` — it never consulted the
`url_alias` table at all. `trovato_blog`'s `blog_listing` gather has no
`canonical_url` (B127 left it unset: the gather's `/gather/blog_listing`
address already has a URL alias to `/blog`, and giving it a `canonical_url` of
`/blog` too would add a 301 redirect from `/gather/blog_listing` to `/blog`
beside the alias that already rewrites `/blog` to `/gather/blog_listing`), so
the whole blog feed linked the ugly address.

Resolving the alias is per request, not per restart, since a `url_alias` row
can be written at runtime. It is cached in `GatherService` and cleared the
moment an alias write bumps `CacheKind::Menus` — the same invalidation an alias
edit already triggers for navigation links — so a feed need not cost a query
on every request, and an alias written after startup reaches it without one.
The resolved link is now part of the feed's `ETag`, since it is part of the
document: previously an alias change that moved the channel link left the
`ETag` unchanged.

Argument feeds (`/blog/author/{user}/feed.xml`, `/blog/tag/{tag}/feed.xml`)
and any gather with a `canonical_url` are unaffected — both already named a
literal address and still take priority over the alias.

A gather feed's `ETag` now changes whenever anything the document prints
changes, not only when an entry itself is edited.

B127 hashed each entry's id, `changed` timestamp and link, so a repeat poll
could be answered `304` cheaply. What it left out was the entry's author name
and its taxonomy term labels: both are printed beside those hashed fields
(`dc:creator` / `<author><name>`, `<category>`), but both live on rows an
entry's own `changed` knows nothing about — `users.name`, `category_tag.label`.
Renaming an account or relabelling a tag left the old `ETag` matching, so a
conditional poll kept getting `304` for a document that now read differently,
and readers carried the stale name until the post's next unrelated edit.

The author's name is now hashed too — it was already fetched on the `304`
path for `dc:creator` itself, so this costs nothing new. Each term the entry
prints is hashed by id and label, and the labels are read in one batched query
for the whole response before the `304` check (never one per entry): that one
indexed lookup is the added cost of a poll that matches. The `Categories`
cache generation counter would have saved it, but it moves on any tag write
anywhere on the install, so while anyone edited tags no feed would ever answer
`304`; the labels move only the feeds that print them.

Also fixed: the `lakeside` fixture theme's `base.html` carried only the RSS
`<link rel="alternate">` for feed autodiscovery, missing the Atom sibling the
stock theme has advertised since B127.

A tile can now be shown to anonymous visitors only, or to signed-in users
only, without a site having to hide it in a template. The login tile is the
case this closes: a tile's `visibility` JSON takes a new `audience` key
(`"anonymous"` or `"authenticated"`), decided from whether the request is
actually signed in rather than from the role list `roles` matches against.

`roles` could not express "anonymous only" because every viewer, signed in or
not, carries the `anonymous user` role in the list `routes::helpers` builds
for tile matching — a list built only for tiles, separate from (and
inconsistent with) the role names `PermissionService` actually grants
permissions by, which never put `anonymous user` on a signed-in user.
`{"roles": ["anonymous user"]}` therefore matched everyone, and a site
(rudere) worked around it with a per-theme template check the kernel should
have offered instead. `audience` is decided straight from the viewer's
signed-in state rather than from that list, so it needs no change to it and
carries no risk to anything the list already does.

The tile admin form gains a "Show to: Everyone / Anonymous visitors only /
Signed-in users only" field, saved into `visibility` alongside any `paths` or
`roles` rule a config import already set, and shown again on edit. Config
import and export round-trip `audience` the same way they already did every
other `visibility` key.

A "Language switcher" tile: an administrator places it in any region from the
tile admin, like any other tile, and it links each enabled language to this
same page in that language, marks the current one, and renders nothing at all
on a site with only one language — there being nothing to switch to.

An item page already knew its own translations, through
`available_translations`, but that fact reached the page's `<head>` as
`hreflang` alternates and nowhere else: nothing on the page itself offered a
reader the click. Tiles render earlier in the request than the item route
computed that list, so the item route now computes it first and the tile
layer reads it back out of the page context, rather than duplicating the
lookup. A page type with no translation list of its own — a term page, a
gather listing, anything else — offers each other language's front page
instead, the same fallback Drupal's own switcher block uses for a page with
no translation of itself.

The tile's own markup is a `nav` labelled by the tile's title, one link per
language carrying `hreflang` and `lang`, and `aria-current="true"` on the
language being read. The template is a stock one,
`elements/tile--language_switcher.html`, so a theme overrides it the way any
tile template is overridden.

On a page served in a language other than the site default, every menu link to
a page that exists in that language now keeps the language prefix, and the link
for the page being read is marked active, as it already was in the default
language. `localize_menu_links` prefixed a link only when it resolved to an item
with a translation, so a link to a gather page (`/recipes`), a term, the front
page or a plugin route kept its default-language address on `/es/recipes`: the
click left the language. And the active check compared each link with the
address as asked for, `/es/recipes`, so a link either side of the prefix never
matched, and an unprefixed link never could. The rule for a link existing in a
language is now the one the router already serves by: an item link exists when
the item has a translation (unchanged, and judged on the link's page without
its query or fragment, so `/why#faq` is the item at `/why`), and any other
local path exists in every language unless language negotiation passes it
through untouched (`/static`, `/api`, the health probes, `/install`), which both
now ask through one function, `is_served_per_language`. Entries plugins register
in the menu registry, such as the account links under `/user`, follow the same
rule. External URLs are left alone. Active is decided with the language prefix
removed from both the link and the requested address, so an untranslated item's
unprefixed link is active on its page read under `/es`; a link that names a
different language explicitly (`/es/recipes` on English `/recipes`) is a
different page and is never active. The existing rule that a section link is
active on the pages under it is kept.

Deleting an item now removes its queued embed jobs. Saving an item enqueues a
`plugin_queue` row under the kernel's reserved `__kernel_embed` identity, with
the item's id only inside the JSON payload, so no foreign key cascaded and the
delete path never touched the queue: the row stayed until the next drain found
the item gone and discarded it. `Item::delete_in` now deletes that item's
`__kernel_embed` rows in the same transaction as the item, and touches no other
plugin's rows.

An enqueue that ran alongside a delete could still leave a job behind, since
it read the item and inserted its job without any lock the delete would see.
`enqueue_embed_job` now holds `FOR KEY SHARE` on the item row while it writes
the job and the item's pending state, and queues nothing (returning `false`)
for an item that is gone; `Item::delete_in` deletes the item row before its
embed jobs. Whichever starts first, the other waits, and no job outlives the
item. A job left over from before this release is still dropped by the drain,
which treats a job whose item no longer exists as done and logs nothing for it.

A vocabulary owned by a group, club or project, and its terms, can now be read
only by someone who may see that owner. Owned vocabularies (B108) stored their
owner on the `category` row, but every category read path ignored it: the
category and tag API (`/api/v1/categories`, `/api/v1/category/{id}` with its
`tags` and `roots`, `/api/v1/tag/{id}` with its `parents`, `children`,
`ancestors`, `descendants` and `breadcrumb`, and the same routes under the
unversioned `/api` alias) needed no login and listed every owner's
vocabularies and terms to anyone, and so did the MCP `list_categories` and
`list_tags` tools, a gather term page or feed argument bound to an owned
vocabulary, a gather's exposed category filter and `/topics`. The term
autocomplete already refused owned terms to anyone who could not edit the
field, so the read rule and the edit rule disagreed.

The rule is decided once, in `content::owned_vocabulary`, and every one of
those paths uses it. A site-wide vocabulary reads exactly as before, for every
caller. An owned one is visible to a site administrator or a holder of
`administer categories`, to a holder of the permission the owner's
declaration names, and to a user the owner's plugin grants a new read
operation, `view`, of `tap_vocabulary_owner_access`, asked with the stored
owner id as the key and once per owner rather than once per row. A plugin that
does not answer `view`, or answers anything but `grant` for that very owner,
leaves the vocabulary hidden. To anyone else an owned vocabulary or term is a
404, the same 404 a vocabulary or term that does not exist now gives on every
read route (the tag list, roots and hierarchy routes used to answer an unknown
id with an empty 200), so the answer does not reveal that it exists. The
category list filters in SQL, so its pager counts what the caller can see.
Writes, the save check and the term autocomplete are unchanged.

Three leftovers from the `category_term` `RecordReference` work (B137, #236).

The term box's placeholder read "Search category_term..." — and, for an item
reference field, "Search blog_post..." and the like — because the template
built it straight from the field's target type machine name. The placeholder
is now a human label the route resolves when the form is built: "terms" for
a category term field, the target content type's own label for an item
field, run through the same locale lookup a translated interface string
would use, and falling back to a plain "Search..." when no label is known.

A field limited by `settings.vocabulary_owner` offered the item's *stored*
owner's terms even after the editor changed or cleared the owner field on the
open form, because the widget only sent the owner value when it was
non-empty: an empty owner field looked, to the endpoint, exactly like a
request that said nothing about an owner at all, so it fell back to what was
stored. Saving then refused the item (no owner) or the term (`TermsNotOwned`,
wrong owner) with a term the box had just offered. The widget now sends the
owner field's current value whenever the field has one, blank or not, and the
endpoint tells "empty" from "absent" apart: an owner changed on the form
offers the new owner's terms, one cleared offers none and shows a short
"Choose the owner first" hint instead of the stale list. The endpoint still
resolves the owner with the one rule the save path already enforced,
`OwnerLimit::admits` — nothing about what a write accepts changed.

A form rendered before this field information existed (or before B137 at
all) could only have gotten there as an already open browser tab; nothing the
kernel stores server side — the form state table, the AJAX "add another"
rebuild — carries pre-rendered widget markup forward across a deploy, so
there was no stale state to migrate. Reloading the page is enough.

A category term box limited to the item's owner now searches when the owner
field is one this editor may not see. The edit form leaves such a field out,
and the widget read the missing control as an empty owner field: it showed
"Choose the owner first" and never asked the server, though the server would
have offered the stored owner's terms, the ones the save accepts. The form now
names the owner field to the widget only when the field is on the form.

A feed's channel link no longer sticks to the raw `/gather/{query_id}` path
for five minutes after one failed `url_alias` lookup: a lookup that fails is
answered for that request only and is not cached. A lookup that read
`url_alias` before an alias write cleared the cache can no longer put the old
alias back into it afterwards; it drops its answer when the cache was cleared
while it ran.

The `cookie` crate is upgraded from 0.18.2 to 0.18.3, which fixes
RUSTSEC-2026-0594 (severity 6.5, medium): a process that forks after seeding
its random number generator could hand the same nonce to more than one
private (encrypted) cookie, letting two different cookies reuse the same
keystream. `cookie` arrives transitively through tower-sessions 0.14.0 and
tower-cookies 0.11.0, used by trovato-kernel and trovato-mcp for session and
private cookie handling, so every build of Trovato from `Cargo.lock` carried
the vulnerable version. The change is a lock file refresh only: the workspace
already asked for `cookie = "0.18"`, nothing else in `Cargo.lock` moved, and
no Trovato code or configuration changed. Operators building from source
should rebuild.

A pull request's CI run finishes in about 20 minutes of wall clock instead of
42, every test still runs with the same assertions, and a budget in CI keeps it
there. Three causes set the old number, none of them a slow test.

Coverage ran on every pull request, instrumented and one test at a time, for
42 minutes, ten longer than any other job, and nothing gated on the number it
produced. It now runs on pushes to `main` and by hand.

Shard 1 of the Test job carried every unit and doc test on top of its third of
the integration targets, which put it ten minutes behind the other two. The
unit and doc tests are now one part per shard, and the integration targets are
partitioned longest first by their measured time (`.config/ci-test-weights.txt`)
instead of by index, so the three shards finish together. That partition is
free to move a target only because every target now passes alone on a fresh
database: `language_navigation_test` did not, because it relied on an earlier
target having migrated the translation plugin's table.

And every test ran serialised, `--test-threads=1`, in every job. The unit tests
now run under cargo-nextest, one process each and all at once, which isolates
the environment variables the single thread was protecting. The integration
targets stay under libtest, because each nextest process would rebuild the
whole test application (thirteen times the CPU for `front_page_test`), but run
their tests in parallel inside each target, four at a time, as a local
`cargo test` always has. Parallel tests found the races the single thread hid,
and each is fixed at its cause in test code: a lock released before the
test's deferred cleanup restored what it guarded (`common::hold_until_cleanup`
now holds it through cleanup), raw writes that skipped a cache a concurrent
request refilled or a concurrent render filled stale, site-wide settings one
test switched while another relied on them, plugin tables created concurrently
without a lock, and a tracing capture that missed events because another
thread registered the callsite first
(`common::scoped_subscribers_see_every_callsite`).

argon2 and Cranelift now build optimized in the dev profile (`Cargo.toml`).
Most of a test's CPU was spent in them unoptimized; optimized, the same targets
use about a third of it. The workspace's own crates build as before. The
dependency cache of the Test, Coverage and Cluster jobs is now keyed on the dev
profile too; it was not, so a profile change was rebuilt in every run and
never saved.

The Cluster job, 26 minutes of which 0.6 were tests, now builds its image on a
cached dependency layer (the Dockerfile builds the dependencies in a layer of
their own, with cargo-chef, written to the cache on pushes to `main`), builds
the test binary at the same time, and waits until the proxy balances to both
nodes before the first test: 13 minutes.

The budget: a test over 30 s fails CI unless `.config/slow-tests.txt` names it
with its measured time and a reason, and an allowlisted one fails at twice its
recorded time. It applies to each test, integration tests included: libtest's
`--report-time` gives every test its own time (switched on for the test process
alone, through a runner), where the budget first timed a whole integration
target, which failed a target of many quick tests and let a slow test through
inside an allowlisted target. Doc tests are timed crate by crate, and every
shard's budget step requires every report it should have, so a test cannot
drop out of the budget because its report went missing; the parser fails on a
target that reports no time, and the shard script refuses a target name two
packages share. `scripts/ci-scripts-test.sh` tests all of it.

One more race of the parallel tests: `appearance_test`'s restore of the stock
theme released its lock before the themes cache bump from the test's own save
had been seen, and the reload that bump triggers read the restored, empty
setting in the next test's turn and put the stock theme back over the one that
test had just made active. The restore now waits for the bump to land. Each Test shard and the Cluster job has a timeout set at its
expected time plus half. `docs/testing-ci.md` describes the split, where new
tests go, and how to remeasure.

The design for a native, incremental, Pagefind compatible search indexer is
written down in `docs/design/incremental-search-index.md`, together with the
evidence it rests on. Nothing about search changes in this release: the index is
still rebuilt whole by the `pagefind` CLI after every change, exactly as before.

The full rebuild was measured on the largest content set available locally
(1,237 searchable items, 223 visible comments). The design covers a page
ledger in Postgres, which gives each item a page number for its whole life and
reuses a deleted item's slot. It also covers a per item term cache keyed by
content hash, word chunk ranges frozen at the last full build, a write order
under which a reader never sees a meta file pointing at a missing chunk,
compaction once tombstones pass 5% of page slots, and a change queue to replace
the rebuild flag. It sets out three compatibility gates that the production
indexer has to pass.

A new dev tool, `crates/pagefind-harness` (`trovato-pagefind-harness`, not
published, not a kernel dependency), depends on the `pagefind` crate pinned to
`=1.3.0`, the release the Dockerfile downloads. It decodes a built index
(gunzip, the `pagefind_dcd` signature, CBOR into structs that mirror Pagefind's
`MetaIndex` and `WordIndex`), and it proves that the crate, given one page at a
time, produces the same words, positions, weights, word count, meta and
fragment for every page as a full CLI build. Its `tokenizer_parity` test runs
that proof over 60 exported items it carries in `corpus/`. The comparison
against the crate's own full build always runs. The comparison against the CLI
runs when `PAGEFIND_BIN` or `pagefind` on `PATH` is present, and skips with a
message otherwise.

The tool is its own Cargo workspace with its own lockfile, excluded from the
root one. A `Pagefind Harness` CI job builds it from its own manifest and runs
all its tests, the CLI comparisons included, with the pinned 1.3.0 release
downloaded. The `pagefind` crate depends on
`actix-web` for `pagefind --serve`, and through it on two advisories that have
no fix inside pagefind 1.3.0's version ranges: RUSTSEC-2026-0443
(`actix-files` 0.6) and RUSTSEC-2026-0258 (`h2` 0.3). Kept apart, neither
reaches the root lockfile the product ships and audits from, and the audit
ignore list stays empty.

Its `export` and `measure` subcommands start the server state with start-up
migrations switched off, so running them never changes a schema. `export`
refuses a directory that already holds files, because a page left over from an
earlier export would be compared as if it were current. `--sample` always
returns the number of items asked for when the corpus has that many. `measure`
checks that the CLI wrote an index before it moves the old one aside.

`appearance_test`'s `a_theme_that_does_not_parse_cannot_be_activated` failed
intermittently in CI, on `main` as well, with the active theme `stock` where it
expected `lakeside`. An earlier theme test's switch bumps the themes cache
generation from a background task, and this server's watcher answers the bump
by reloading the stored choice, which cleanup had already deleted. When the
reload landed after the test had set Lakeside, it replaced it. The test now
stores Lakeside as well as applying it, so a reload, whenever it lands,
restores the theme the test expects. It checks that by reloading the stored
choice itself after the refused save.

The kernel now builds the Pagefind search index itself, and no `pagefind`
binary is needed anywhere: not on a server, not in the image (the Dockerfile no
longer downloads one), not on a developer's machine. This is part 2 of the
incremental index in `docs/design/incremental-search-index.md`. The new
workspace crate `trovato-search-index` writes Pagefind 1.3.0's format (CBOR
through minicbor, gzip with Pagefind's signature, SHA-1 file names) and carries
Pagefind 1.3.0's own tokenizer, the `fossick` module, vendored under its MIT
licence, together with the 1.3.0 browser bundle. It does not depend on the
`pagefind` crate, so neither `pagefind` nor `actix-web` enters the root
lockfile. On the largest local content set (1,237 items) a rebuild takes
0.32 to 0.34 seconds with every page tokenized and 0.26 to 0.27 seconds when
nothing changed, where the CLI took 1.6 to 2.5 seconds.

Every searchable item now has a page number for life in a new ledger table,
`search_page`; a deleted item's number becomes a tombstone that the next new
item in its language takes. Each item's tokenized terms are cached in
`search_page_terms`, keyed by a hash of the HTML it was indexed from, so a
rebuild re-tokenizes only what changed. `search_index_state` records the
published generation and the Pagefind version, and `search_index_file` when
each index file stopped being used. A rebuild reads the published generation before it
exports the content, holds a Postgres advisory lock, stages the new index beside
`PAGEFIND_OUTPUT_DIR`, and publishes it only if no other build published since
that read: data files first, then the meta file, then `pagefind-entry.json`
last, by rename. A build that lost to another one changes nothing and asks for
another rebuild, so an older export never replaces a newer one; so does a
build whose entry went live but whose database transaction then failed. File
names are seven hash characters, so the build names its files against the live
directory, and a name already used there by a different file is lengthened
rather than reused. The old index is no longer moved
aside: a browser that loaded it keeps finding its files, which are deleted 24
hours after the published index stops using them. A build that wrote no entry
file, or an entry naming a missing meta file, is refused before the published
index is touched; before, a CLI that exited cleanly without writing an index
left the site with none. The `rebuild_requested` flag works as before.

Search results change in one way, on purpose: each item is indexed in its own
language (`<html lang>` from the item's language, or the site's default), so
Pagefind stems words and the browser loads that language's search code. On
the local content set, "commented" now finds 83 pages where it found 7, and
"lines", "titled", "updated" and "revisioned" likewise find their other forms.
A query that is the start of a longer token and ends in a letter the stemmer
changes can find fewer: "zqbody" stems to "zqbodi", which no longer begins
`zqbody01…`. Items in different languages are now in separate indexes, so an
English search no longer returns German pages. A single language index was
called `unknown` before and is now `en` (or the site default).

Pagefind's fixed-name files (`pagefind.js`, `pagefind-entry.json`, the
`wasm.*.pagefind` files) are now served with `Cache-Control: no-cache`; the
content-addressed index files keep their day.

Three compatibility gates hold the format to Pagefind's own, over a 60 item
English and German fixture (`search_index_gates_test`): two builds are byte
identical, the index decodes equal to the pinned CLI's, and the stock
`pagefind.js` in Node returns the same results from both. The Test job in CI
installs the pinned CLI and Node for the last two and fails if either skips.
The pagefind harness now compares the vendored tokenizer and the native build
with the CLI as well, and gains `measure-native` and `bundle` commands.

Bulk publish, unpublish and delete on `/admin/content` now work. The bulk form
declared its selection as a sequence of item ids read by `axum::Form`, whose
`serde_urlencoded` deserializer cannot build a sequence from the repeated
`ids[]` keys the list screen posts, so every request that selected anything was
refused with a 422 before the handler ran; only an empty selection ever got
through. The handler now reads the body with `url::form_urlencoded`: `_token`
and `action` must each appear exactly once (a 400 otherwise), every selected id
must be a valid item id or nothing is written, and a repeated id is acted on
once. A selected id that names no item is now reported as failed by bulk
publish and unpublish, as bulk delete already reported it, instead of being
counted as done. The permission checks for each action and the CSRF check are unchanged,
and every selected item is still decided by the item service as the acting
user, so an item that user may not edit, publish or delete is counted as failed
rather than changed.

Every outbound AI provider request now goes through the kernel's SSRF-hardened
HTTP client. This covers chat, streaming chat, tool calling chat, embeddings,
the connection test and the plugin `ai-request` host call. The AI provider
service built its own plain `reqwest` client instead of using the shared
`hardened_outbound_builder()`. So AI requests skipped two checks that every
other outbound path has: the resolve-time address check and the per-hop
redirect revalidation. A provider host name that resolved to a private address
was connected to, and a provider redirect to one was followed. The client build
also fell back to an unhardened default client if it failed. The service now
builds its client from the shared builder, keeps its default request timeout,
and refuses to start if the client cannot be built.

Upgrade note: a provider whose host name resolves to a private, loopback,
link-local or other reserved address is now refused when the request connects.
For example, `ollama.local` or `ollama.internal` pointing at a LAN or
local machine was reachable before and is refused now. Local and LAN model
servers are not a supported provider address. The admin form's checks on the
configured base URL are unchanged.

The AI assistant, AI chat, the writing assistant and AI search now work with
every current Anthropic model. Anthropic models newer than `claude-sonnet-4-6`
answer 400 to any request that carries `temperature`, and the kernel put one on
every request: the assistant and chat configs defaulted it to 0.2 and 0.7, so
it was never absent, and the writing assistant and search hard coded 0.7 and
0.3. Temperature is now optional end to end. The assistant and chat configs
hold it as unset by default, the admin forms take a blank field as unset, and a
request carries the key only when a site has set one. The writing assistant and
search have no setting for it and send none. A config stored with a number in
it still loads and still sends that number; nothing rewrites stored settings,
so a site that wants a current Claude model clears the field.

A provider call that fails now says why in the log. The tool calling path
turned a non-2xx answer into "the AI provider returned HTTP 400" and dropped
the body, which is where Anthropic names the parameter it refused, and search
logged nothing at all. Every outbound AI path (assistant and chat, streaming
chat, the writing assistant, search, embeddings and the plugin `ai-request`
call) now logs the status and the provider's response body, cut to 1,000
characters and with the request's key removed should a provider echo it. What
the person sees is unchanged.

The connection test fails on any non-2xx answer. The Anthropic test sent a GET
to the bare base URL and counted any HTTP answer as success, so a 404 read
"Connected successfully". Both protocols now ask `{base_url}/models`, which
needs a valid key and is not billed, and a failure shows the status and the
provider's explanation.

The assistant's `done` event reports the tokens the message spent. It was
built with zeros while the conversation added the real counts to its total; it
now carries the sum over every model call in the turn, the same calls and
totals the conversation records.

Apply after sending a message no longer fails with 403. CSRF tokens are single
use, and `assistant.js` sent the page's one token with the message and then the
same spent token with Apply. Every response to an assistant post (message,
apply, discard, start over) now returns a fresh token in an `X-CSRF-Token`
header, and the page script uses it for the next post and puts it into the
page's plain forms for their no-JavaScript fallback.

Unreleased pull requests raised the version past the release `main` was heading
for. Two of them added to the plugin API after `main` already declared 0.105.0,
and `docs/design/Versioning.md` said such a change is a MINOR without saying it
belongs to the version `main` already heads for, so each raised the minor again.
Nothing said a version is raised once per release, and 0.105.0 shipped from a
tree that said 0.107.0 until this release set it back. The rule is now written
in `docs/design/Versioning.md`, `docs/design/version-map.md` and
`CONTRIBUTING.md`, and `scripts/version-check.sh`, run in CI as `Version Step`,
fails a pull request whose version is neither the newest release tag nor one
release step above it.

The changelog gate could not tell a release fold from a change that forgot its
entry. The fold deletes every fragment and the version sweep rewrites every
plugin manifest, so the one branch that ships the entries was the one
`scripts/changelog-check.sh` failed. It now recognises a release fold by what
the fold leaves behind, fragments deleted, `changelog.d/` holding only its
README and a new `## vX.Y.Z` section for the declared version, and says which
rule passed; every other branch is checked as before.

The nightly image version counted every commit in the repository. Before a
minor's first tag, `.github/workflows/docker-publish.yml` published `main` as
`X.Y.<total commit count>`, which sorts above the `X.Y.0` it was heading for;
that is how `0.104.154` and the `0.105.<N>` images came to exist. A build
before the tag is now a SemVer prerelease, `X.Y.0-nightly.<commits since the
previous release tag>`, which sorts below the release whichever is pushed
first.

Saving a content type's edit form no longer deletes the type's fields. The
type metadata writer, `ContentTypeRegistry::update`, wrote the form's three
settings keys (`title_label`, `published_default`, `revision_default`) over the
whole `item_type.settings` column, and the field list lives in that column
under `fields`, so every save removed every field declaration along with any
other settings key the type carried. The cache hid it: `update` rebuilt its
cache entry from the previous cache entry rather than from the row, so the
admin kept showing the fields until the next reload from the database, or the
next restart, and then the type had none. Item data was never touched, but
nothing declared its keys any more. `update` now merges the keys it is given
into the row's current settings in one SQL statement, never writes `fields`,
and rebuilds the cache from the row the statement returns; the field writers
merge by the same rule, so neither writer can drop what the other stored.

The add form could overwrite an existing type the same way. It asked the cache
whether a machine name was taken, and `ContentTypeRegistry::create` upserted,
so a row this process had not cached yet (written by a second server, or by a
config import before the next reload) had its label, description and whole
settings, fields included, replaced by the form's. `create` now inserts only
when no row has the name, decided by the database, and refuses otherwise with
`CreateContentTypeError::AlreadyExists`, which the form shows as the existing
"already exists" message. Overwriting a whole definition on purpose is
`ContentTypeRegistry::replace`, a fixture and import tool the test suite uses
to reset its shared types.

Types whose fields were already lost are not repaired by this change: the
declarations cannot be reconstructed from the type row.

Restarting the kernel no longer resets a plugin-declared content type such as
`blog`. Plugin registration, which runs on every boot, wrote the plugin's
declared field list over the whole `item_type.settings` column through a
whole-row upsert, so each boot dropped every settings key an admin had saved
(`published_default`, `revision_default` and any other), every field an admin
had added, and the admin's title label. Registration now merges instead, in one
statement: the plugin's declared fields win by name, replacing the stored field
of the same name whole and keeping its position, and declared fields the type
lacks are appended; every other field and every other settings key stays as it
was, and the title label is kept unless the plugin declares one. A field the
plugin stops declaring also stays, since without a provenance marker it cannot
be told apart from one an admin added, and keeping it leaves stored item data
with a definition. Registering the same declaration again writes identical
settings, and the registry now caches the row the database holds rather than
the bare declaration. `ItemType::upsert`, used by config import and
`ContentTypeRegistry::replace`, still replaces the whole column.

The lock file is refreshed to the newest release every declared dependency
requirement allows, which clears 41 of the 43 vulnerabilities the daily live
security audit had been reporting against `main`. The root cause was that the
lock was never refreshed after the 2026-10-06 wasmtime upgrade: fixed patch
releases were published and sat unused, while the pull request audit, which
runs against a pinned copy of the advisory database, stayed green because none
of the advisories existed at its pin.

The crates the advisories stood against, old to new: wasmtime 49.0.2 to 49.0.5
(seventeen advisories against the plugin runtime; still the 49 line, with
`KERNEL_API_VERSION` unchanged and plugins already built for `wasm32-wasip1`
loading as before), openssl 0.10.76 to 0.10.82, ammonia 4.1.4 to 4.2.5 (four
advisories, two of them sanitizer bypasses), rustls 0.23.45 to 0.23.46,
rustls-webpki 0.103.15 to 0.103.17, webpki-roots 1.0.6 to 1.0.12,
pulldown-cmark 0.13.3 to 0.13.6, and zstd 0.13.3 to 0.13.5 with zstd-safe 7.2.4
to 7.4.1 and zstd-sys 2.0.16 to 2.1.3, which wasmtime's module cache pulls in.
The same refresh clears the unsound-code warnings against tokio 1.50.0 (now
1.54.1), rustix 1.1.4 (1.1.6), event-listener 5.4.1 (5.4.2) and rand 0.8.5 and
0.9.2 (0.8.8 and 0.9.5). No `Cargo.toml` and no source file changed.

Two advisories remain, and neither has a fix a lock refresh can reach: rmcp
0.16.0 (RUSTSEC-2026-0462, in Streamable HTTP client transports the STDIO-only
MCP server does not compile), whose fix is the rmcp 3.x major version, and
libsqlite3-sys 0.30.1 (RUSTSEC-2026-0459), which sqlx's lock entry for
`sqlx-sqlite` carries and every released sqlx caps below the fixed 0.39.

The MCP server, `trovato-mcp`, is on rmcp 3.10, up from 0.16. Neither advisory
against 0.16 was reachable here: RUSTSEC-2026-0189 is in the Streamable HTTP
server transport and RUSTSEC-2026-0462 in the Streamable HTTP clients, and
`trovato-mcp` builds neither. It serves over STDIO with rmcp's default server
features plus `transport-io`, and it still does. The major version was overdue
anyway, and staying on 0.16 meant keeping a suppression for an advisory with a
fix available. The ten tools keep their names, input schemas and result shapes,
and the three resources and one resource template keep their URIs. rmcp 3
generates tool input schemas in a different form: an optional parameter
becomes a `["integer", "null"]` union instead of a nullable `integer`, and the
schema root loses its title and description. The server therefore generates
the schemas itself, exactly as 0.16 did, so a client sees the same schemas
byte for byte. A new test
serves the server over an in-process byte pipe the way `main.rs` serves it on
STDIO, sends `tools/list` as raw JSON-RPC, and checks the tool names, so a
future rmcp upgrade that drops, renames or adds a tool fails a test rather than
a client. The RUSTSEC-2026-0189 suppression is removed from
`.cargo/audit.toml`.

The advisory database that pull requests audit against moves from the
2026-10-06 commit to advisory-db commit 4505c66d of 2026-11-09. The rule that
picked it: the newest commit at which the tree audits with zero
vulnerabilities and no new suppression. The next commit in advisory-db
publishes RUSTSEC-2026-0459 against libsqlite3-sys 0.30.1, and no dependency
upgrade can fix that one. The crate is in `Cargo.lock` only because the lock
lists sqlx's SQLite driver; Trovato builds sqlx for PostgreSQL alone, so the
driver is never compiled. Every released sqlx, 0.9.0 included, caps
libsqlite3-sys below the fixed 0.39. The pin stops just short of that advisory
so it is not hidden from pull requests, and it stays open on the live audit
for a decision.

A gather can now require a permission, and no path serves its rows to a viewer
without it. Gathers had no access gate of their own: the gather page, the
`display.routes` aliases, the feed and the JSON API ran the query for anyone, and
item access, which filters rows one by one, does nothing for a gather that reads
a lightweight-record table. So a plugin gather over its own tables, such as a
list of network devices, was readable by an anonymous visitor at its route alias.
A definition may now carry `"access": { "permission": "..." }`, stored with the
definition and carried through config export and import. Every entry point
checks it with one function, `gather::authorize_gather`, before the query runs:
an anonymous visitor on an HTML path is sent to log in and a signed-in one gets
403, a JSON path answers 403 with a JSON error, a feed answers 403,
`/api/queries` leaves the gather out, and the MCP `run_gather` tool refuses it.
Administrators pass. Rows of a gather the viewer may run are still filtered by
item access.

A lightweight-record gather that declares no permission now requires
`access content`, the permission the published path of item access already asks
for, so it is no longer public by accident and a site that withdraws
`access content` from a role withdraws both kinds of listing. Item gathers are
unchanged. With the default role grants, where the anonymous role holds
`access content`, no page that answered before answers differently; on a site
whose anonymous role lacks it, a record gather now sends anonymous visitors to
log in.

`menu_link` rows are no longer drawn for a viewer who could not follow them. A
link whose address serves a gather the viewer may not run (directly, through a
URL alias, a canonical URL or a route alias) or a plugin route whose `tap_menu`
permission they lack is dropped, with every link under it. A site whose main menu
comes from `menu_link` keeps drawing that menu, even when a viewer may follow
none of its links, instead of falling back to the plugin menu. The gather builder
keeps a definition's `access` when the gather is edited and saved.

Plain string, number and boolean fields on an item page show a label again,
and it is the one the item type declares. The item view wrote them as
`<strong class="field__label">label</strong>: value`, and the theme hid every
`.field__label`, although its comment said the rule was for body fields. The
label went and the `: ` stayed, so a Netgrasp person Item read `: true` and
`: false` with nothing to say what either was. The separator now sits inside
the label element, so a hidden label takes it along; the theme hides labels
only on `field_body` and `field_content`; and the label is read from the type's
field definition, falling back to the machine name (`field_notify_arrive` as
"notify arrive") only when the type declares none.

The Cancel button on a comment reply works, and the comment block raises no
Content Security Policy error. It called `cancelReply()` from an inline
`onclick`, which the kernel's `script-src` (no `'unsafe-inline'`) blocks, so the
click logged a violation and did nothing. `comment-reply.js` binds the button
itself now, and the `window.cancelReply` global that only served the inline
handler is gone.

Outgoing mail goes through lettre 0.11.24, up from 0.11.23, which fixes six
advisories published on 2026-11-17 against every earlier 0.11 release.
RUSTSEC-2026-0560, scored 4.8, let a man in the middle inject SMTP replies ahead
of the STARTTLS handshake so that they were read as if they came over the
encrypted channel. RUSTSEC-2026-0561 left a bare LF or CR in a message body
without dot-stuffing, so a body could end the DATA section early and inject SMTP
commands of its own. RUSTSEC-2026-0563 sent a message addressed to a quoted
local part to a different address than the one written. The other three are
denial of service: RUSTSEC-2026-0562, a panic on an `EHLO` reply line of only
white space; RUSTSEC-2026-0564, a panic on a mailbox display name containing CR
or LF; and RUSTSEC-2026-0565, unbounded buffering of an SMTP reply, which let a
malicious server exhaust memory.

Every site that sends mail through SMTP is affected, whether the mail comes from
the kernel (registration, password reset, notifications) or from a plugin
through the `mail` host interface. The upgrade is a lock file change only: the
workspace already asked for lettre `0.11`, nothing else in `Cargo.lock` moved,
and no Trovato code or configuration changes.

`docs/design/plugin-query-surface.md` records decision D-60, which supersedes
D-19: plugin reads of content go through a new `query` host interface compiled
by Gather for the request's viewer, stage and language; a plugin's own tables
gain conditions, upsert and bounded batch insert on `db`; translations are
written through `ItemService` from `item-api`; and `raw_sql` stays one flag for
what Gather will not compile, shown in the admin listing, the boot log and a
capability reference. Design only: no contract change in this entry.

A real-database test now pins the read gate `query-raw` relies on: PostgreSQL
runs the `DELETE` inside a `WITH ... AS (DELETE ...) SELECT` in an ordinary
transaction, so a first-keyword check could never hold, and refuses it with
SQLSTATE 25006 in the `BEGIN READ ONLY` transaction `query-raw` opens. BL-61 is
marked fixed by #125, which made that so.

A plugin can run a Gather definition now, through a new `query` host interface
(`run`, bound in the SDK as `trovato_sdk::host::query`). Until now a plugin
that listed content across types, sorted and paged with each row's URL, had
to reach for `raw_sql`, which reads every unprotected table with no viewer,
stage or language. The kernel's own reference site did exactly that. The definition
runs through the same pipeline as a site's gather page, for the user the plugin
is acting for, on the request's read stages and in its language. The plugin
cannot set any of the three: `stage_aware = false` on an item query is refused.
Item rows carry `url`, and a filter on `url` finds an item by the address it is
served at, resolved for any gather. A plugin's own tables are queried with no
viewer semantics, an item query may join them, and they may not join `item`.
Tables that are not content must be in the plugin's allowlist. Includes and
`semantic_similarity` are refused. A page is at most 100 rows, a definition at
most 32 filters, 8 sorts and 3 relationships, and the statement timeout is 5
seconds. Two new codes: `ERR_QUERY_INVALID` (-80) and `ERR_QUERY_BOUND` (-81).

`query-items` listed items on every stage, so a live request could return a
published item from an `accessible` stage, which is never listed, and an editor's
plugin saw an internal stage's items in a live request. It now lists the
request's read stages only (Live outside a plugin route), newest change first
with the id breaking ties (BL-64). A plugin route now dispatches with the
viewer's read stages and negotiated language.

Contract: the `query` interface, its SDK binding, `QueryResult` and the two
error codes are **additive** (a new interface under the version map). What
`query-items` returns changes, so this is a **behavior change** to an existing
host function, which is frozen surface, taken in a minor release while the
project is 0.x.

Gather can express `or` now, and it can summarise rows. `filter_groups`
holds parenthesised groups, ANDed with `filters`. Each group joins its filters
and nested groups with `and` or `or`, up to three deep. Grouped filters get
the same resolution as top-level ones: category hierarchies, custom operators,
contextual values and `url`. Exposed and `semantic_similarity` filters stay
top-level.

`group_by`, `aggregates` (`count`, `sum`, `min`, `max`, `avg`, each labelled),
`having` (a label compared with a number) and `distinct` summarise the rows of
a base that is not content, which in practice is a plugin's own table through
the `query` interface. The total counts groups. On `item` and on record types
they are refused. There, rows and fields are decided one at a time after the
query, so an aggregate would count or sum what the viewer may not see;
counting content per group stays open as BL-30.

Through `query`, grouped filters and `having` count toward the 32-filter bound.
Nesting past three, or more than 8 `group_by` columns or aggregates, returns
`ERR_QUERY_BOUND` (-81).

The admin gather builder has no control for these shapes. It used to rebuild
a definition from its controls on save, so it would have dropped them. It now
refuses to save a gather that has them, and its edit form says so and turns
saving off.

Contract: the definition keys are **additive**. A definition without them
emits the same SQL, which the golden tests check. `QueryDefinition` gains two
public fields, so a Rust struct literal of it in kernel-linked code needs them
or `..Default::default()` (see `UPGRADING.md`). No WIT, SDK or host function
signature changes.

A plugin can do the ordinary writes on its own tables without `raw_sql`, and
it can write translations through the item service.

On the plugin's own tables (`db`):

- `update` and `delete` take a `where` that is an array of
  `{"field", "op", "value"}` conditions, ANDed. `op` is one of `=`, `!=`, `<`,
  `<=`, `>`, `>=`, `in`, `not_in`, `is_null` or `is_not_null`. The plugin
  guide documented this shape, and the host refused it with
  `ERR_PARAM_DESERIALIZE`. A map still means equality. An empty `where`, an
  unknown op or an empty `in` list is refused, so none of them can mean every
  row.
- `upsert(table, data, conflict)` inserts a row or, on a conflict over
  `{"target": [cols]}`, updates every other column, or the ones `"update"`
  names. It returns the row.
- `insert-many(table, rows)` writes up to 500 rows that name the same
  columns, in one statement, so it writes all of them or none. More than 500 is
  `ERR_DB_BATCH_TOO_LARGE` (-82), refused before anything is written.

The SDK now binds the structured calls as `trovato_sdk::host::db::{select,
insert, update, delete, upsert, insert_many}`; it bound none of them before.
They go through the plugin's allowlist and the protected floor, as before.

Translations (`item-api`):

- `list-translations` returns up to 100 items' translations, for the items
  the user may view, with the field tier applied to `fields`.
- `save-translation` and `delete-translation` are `ItemService` writes. They
  need `edit` access, or `translate content` and `view`. The field write gate
  applies, `tap_item_update` fires on the other plugins, and the item cache
  drops the item.
- A plugin no longer needs `item_translation` in `db_tables`.
- The admin translation screen now saves and removes through the same
  methods. A translation saved or removed there now fires `tap_item_update`,
  and drops the item from the cache, which it did not do before.

Contract: the new functions (`db.upsert`, `db.insert-many`,
`item-api.list-translations`, `save-translation`, `delete-translation`), their
SDK bindings, `ItemTranslation` and `ERR_DB_BATCH_TOO_LARGE` are **additive**.
No existing WIT signature changed. An array `where` was refused before, so
accepting it changes no call that worked. That an admin translation write
now fires `tap_item_update` is a **behavior change** (see `UPGRADING.md`).

`raw_sql` is visible wherever an administrator looks, and the site work it was
held for no longer needs it.

- The admin plugin listing (`/admin/plugins`) marks every plugin whose
  manifest grants `raw_sql`, and says above the table what the grant means.
- Boot logs one warning naming every loaded plugin that holds `raw_sql`.
- `docs/plugin-capabilities.md` (new) says what each `[capabilities]` key
  grants, and what `raw_sql` costs.
- `plugins/test_site_queries`, a test fixture, does the six statements a real
  site plugin held `raw_sql` for (a listing, a sitemap, a docs index, a doc by
  its alias, and a translation sync that lists, writes and removes) through
  `query` and `item-api`, with no `db`, no `db_tables` and no `raw_sql`.
- The error-handling example in `docs/plugin-error-codes.md` used `query_raw`
  on `item`; it uses `host::query`. `docs/design/Design-Plugin-SDK.md` named a
  `[permissions] requires = ["raw_sql"]` key that never existed; it names
  `[capabilities] raw_sql = true`.

Contract: **additive**. `PluginInfo::holds_raw_sql` and
`PluginRuntime::raw_sql_holders` are new public kernel functions; no WIT,
SDK or manifest shape changed. The boot warning is new log output (see
`UPGRADING.md`).

A refusal now depends on who is asking and what they asked for, not on which
handler answered. A JSON caller who is nobody gets `401`; a signed-in JSON
caller who may not do the thing gets `403`. A browser that is nobody is sent to
`/user/login` with a `303`; a signed-in one gets `403`.

Before this the same caller got a different answer from route to route. The
JSON permission helpers answered `403` to an anonymous caller, because they were
built on the HTML guard and threw away which of its two refusals it had made.
Sixteen handlers read the session's user id themselves instead of calling a
guard, some without the check that the account was still active, so a blocked
user's session could still create API tokens, upload files and hold content
locks until it expired. The translation screens refused with a `400` "error"
page, the content-lock routes with plain text, and the block editor with a bare
`403` for someone not signed in.

`require_login`, `require_permission` and `require_admin` now share one core,
`session_user`, which is the only place a handler learns who is asking and the
only place a blocked account's session is ended. `require_login_json` is new,
`require_permission_json` and `require_admin_json` return the `User` and answer
`401` or `403` as `AppError`, and `refusal_for` gives the same answer to a
handler that already holds the viewer (the item create, edit and delete routes,
and the gather access gate). The token, file, lock, comment, export, media,
chat, assist, mail preference, OAuth consent, account deletion, translation and
`/system/ajax` routes all go through them.

A session naming a user who has been deleted is now the anonymous visitor, with
the anonymous role's permissions; it used to be a visitor with no permissions
at all, so a published page that anyone could read answered that session `403`.

The `admin_ai_features` router, `/admin/config/ai/features`, is deleted along
with its template and its line in `docs/admin-permissions.md`. It was never
mounted, and nothing in the kernel or any plugin reads the `ai_features` config
it would have saved, so mounting it would have shipped a settings page with no
effect. A unit test now requires every `routes::*::router()` to be reachable
from `main.rs`.

Contract: an API client that told "not signed in" from "not allowed" by status
should read `401` for the first and `403` for the second; the ad hoc gather
endpoint, the dead-letter queue and embed admin endpoints, `/system/ajax`, the
block editor endpoints and `/api/lock/break` answered `403` to an anonymous
caller and now answer `401`.

Every JSON error a kernel route writes is now one shape, the `AppError` body:
`code`, `message` and `request_id` (and `details` for a validation failure).
Each route keeps the status it answered with before.

There were five shapes. `AppError` was the canonical one, but the comment,
category, gather and item handlers answered with the deprecated `JsonError`
(`{"error"}`) at about seventy sites; the recovery, sessions, account deletion
and passkey routes each carried a private copy of it; `/api/v1` answered
`{"error","status"}`; the rate limiter wrote `{"error","retry_after"}` by hand;
and the CSRF check and the API token middleware wrote raw `{"error"}`
literals. The content-lock routes, the plugin gates, the bearer-token
middleware, the OAuth endpoints and the image style routes answered in plain
text. A script had to know which route it had called to read the reason, and
`static/js/chat-widget.js`, which read `err.error`, showed "Unknown error" for
every `AppError` failure.

`JsonError` and the local copies are gone; `AppError::with_status` keeps a
route's own status where no named constructor fits, deriving `code` from it.
`require_csrf_header` returns an `AppError`. The rate-limit `429` is an
`AppError::RateLimited`, still with `Retry-After`. The passkey, session and
recovery JSON endpoints, which used the HTML login guard and so redirected an
anonymous script to the login form, use the JSON guards and answer `401`.

A plugin's API route errors are `AppError` too, with an `error` key carrying
the same string as `message`, so plugin clients that read `error` keep working
until 2.0. The block editor image upload keeps Editor.js's `{"success": 0}`.

The kernel's scripts (`chat-widget.js`, `ai-assist.js`, `api-tokens.js`,
`file-upload.js`, `gather-builder.js`, `passkey-login.js`, and the recovery,
passkeys and sessions pages) read `message`.

`tests/error_shape_test.rs` asks one failing request of each router and
requires the `AppError` body; a unit test refuses a `JsonError` or a
`json!({"error": ...})` body anywhere under `routes/` or `middleware/`.

Contract: JSON API clients read `message` (and `code`) instead of `error`; see
`UPGRADING.md`. Plugin route errors are additive (`error` kept).

A browser is shown every error as one page: the site's themed `error.html`
(with `error--{status}.html` tried first) inside the normal page wrapper, the
admin wrapper on an admin path, with a link to `/` or `/admin`. Any other
caller gets the `AppError` JSON body. Nobody signed in is sent to the login
form.

There were five error pages. An unknown path answered `404` with an empty
body; `/item/{id}` and the item add and edit forms answered a browser with
JSON; the gather pages and the four error helpers wrote their own unthemed
HTML, whose "Go back" link was a `javascript:` URL the CSP blocks; and some
thirteen handlers carried a hand-written "server error" page for when a
template failed to render.

The four helpers and `AppError` now attach an `ErrorPage` marker (status and a
message safe to show) instead of deciding what the error looks like. One outer
layer, `middleware::error_page::render_error_pages`, renders a marked response
from the request's `Accept`: `text/html` gets the themed page, everything else
the `AppError` body. Axum's own plain-text refusals (a body that does not
parse, a wrong method) are treated the same. A plugin's own response body is
left alone. The thirteen fallbacks are gone; when the theme itself cannot
render, the layer's one built-in page remains. The 404 page offers a search
box on the front end.

`tests/error_page_test.rs` covers the unknown path, an unknown admin path, a
missing item, the item forms, a missing gather and an admin `403` for a
browser, the login redirect, and the `AppError` body for every other caller.

Contract: themes may add `error.html` or `error--{status}.html`. A script that
calls an HTML page and fails now gets `AppError` JSON instead of HTML; see
`UPGRADING.md`.

An item now gets its URL alias, and loses it, whichever path wrote it. Pathauto ran from the four handlers behind the two edit forms (`routes/admin_content.rs` and `routes/item.rs`), so an item a plugin saved through `save-item`, a bulk action on `/admin/content`, a revert or `config import` wrote never got one. It now runs in `ItemService::create` and `ItemService::update`, and config import, which writes the item row itself, runs it after the save; an item the import set brings an alias for keeps that one. The route calls are gone.

Every alias was written in `en` on the live stage, whatever the item's language and stage. Pathauto now writes the item's own, and an item created without a language gets the site's default rather than `en`. Since an alias's language no longer says nothing, resolving an address and linking to an item prefer an alias in the reader's language and otherwise take one in any, where they used to accept only the reader's language or `en`; the RSS feed links through the site default language instead of `en`. An alias set by hand on the front-end edit form is stored on the item's stage and in its language as well.

Deleting an item left its aliases and every menu link to it behind, pointing at nothing. `Item::delete` now removes the aliases and the menu links to `/item/{id}` or to one of those aliases in the item's own transaction, moving a link nested under one of them up to its parent; config import's delete goes the same way.

A revert copied the body of `ItemService::update` and left out the presave tap, the reindex (`tap_item_update_index` and the embedding) and the alias, and dispatched `tap_item_update` past the write-reentry guard. It is now an `update` with the revision's title, status and fields, so it runs every gate and side effect an edit does.

The front page's promoted listing is now the viewer's. It rendered `list_promoted` with no viewer, so a visitor without `access content`, or one a plugin denied an item to, was shown every promoted item with its body teaser. The listing now passes its candidates through `ItemService::filter_page_for_view` for the viewer, over-fetching so a few hidden items do not leave it short, which also drops any field the viewer may not see before the teaser reads it.

The teaser cut the filtered HTML of the body at 200 characters, which could end inside a tag or an entity and leave an element open across the rest of the page. It now takes the body's plain text, cuts that, and escapes it.

An item set as the front page rendered its fields with a renderer of its own, which knew nothing of Blocks fields, compound sections or a text field's `_format` key. It now uses the item page's (`routes::item::render_item_fields`), so the item reads the same at `/` as at its own address.

The front-end edit form (`GET /item/{id}/edit`) checked `edit` only, so a user who may not see an item got a 403 that confirmed it exists, where the item page and the revision pages answer 404. A user it refuses now gets 404 when they may not view the item and 403 when they may; a user who may edit still gets the form without a view grant, as an author with `edit own content` does on their own unpublished item.

URL aliases, menu links, tiles and stages are written through services rather than by route handlers calling their models directly, so the next thing such a write has to do has one place to live. Two new services, `UrlAliasService` (`AppState::aliases`) and `MenuLinkService` (`AppState::menu_links`), plus write methods on the existing `TileService` and `StageService` (`update_stage`, `delete_stage`), cover every admin write of the four. The front-end item form's alias field goes through `UrlAliasService::set_for_item` and `clear_for_item`. The kernel defines no taps for any of these entities, so none are fired.

Deleting a menu link moved its children up to its parent in one statement and deleted the link in another, so a failure between the two left the children reparented under a link that was still there. Both now run in one transaction (`MenuLink::delete_in`).

Going passwordless (`POST /user/password/remove`) ran its own `UPDATE users`, so `tap_user_update` never fired for it. It now goes through `UserService::remove_password`, which also drops the cached user.

The stage visibility cache the item service once had is gone already: `ItemService::stage_view` reads stages from Postgres on every decision, so a stage write holds on the next request and there is nothing to clear.

The integration tests' `create_test_stage` named stages after the first eight digits of a v7 id, which are the clock, so two stages made in the same minute by a rerun collided on the machine name. It uses the random tail now.

The content audit log records what changed. `AuditService::log` had no caller,
so the `audit_log` table `trovato_audit_log` creates was always empty, and the
plugin's `/admin/reports/audit` menu entry led to a 404. Now every write
service (items and their translations, comments, users, roles and their
grants, categories and tags, URL aliases, menu links, tiles and stages, and a
stage publish) records a row through one kernel helper,
`services::audit::AuditTrail`, when the plugin is enabled: the action, what it
was done to, who did it and from which address. Who acted comes from the
request the write is part of, scoped by a new `scope_audit_actor` middleware,
so a write a bulk action, a plugin or a JSON route makes is recorded the same
as one from an admin form. `/admin/reports/audit` lists the rows, newest first,
50 to a page, to holders of `view audit log`. Rows now carry time-ordered ids,
so two writes in the same second list in the order they happened, and a write
with no request behind it records no address rather than `invalid`.

Security events cover the paths that were missing. Password login emits
`auth.login_succeeded` and `auth.login_failed` (with the reason), as passkey
login always did. A password set by reset email, by account recovery, by an
administrator or from `trovato user reset-password` emits `password.changed`
(or `password.set` for an account that had none) from
`UserService::update_password`, with `details.via` saying which; the
self-service change, which emitted its own, now goes through the same place.
New kinds: `role.created`, `role.changed`, `role.deleted`,
`role.permissions_changed` (with what was granted and revoked),
`user.roles_changed` (a role assigned or removed, or the administrator flag)
and `user.status_changed` (blocked or unblocked), emitted by `RoleService` and
`UserService` with the acting administrator as the actor. `trovato user
role-add` and `role-remove` emit `user.roles_changed` too.

Every AI call the kernel makes now leaves through one send path on
`AiProviderService`: `chat_once` for a complete chat answer, with or without
tools, `chat_stream` for the chatbot, and `embed_with` (behind `embed`) for an
embedding. AI search's expand, summary and follow-up, the sentiment classifier,
the writing assistant (`/api/v1/ai/assist`), the streaming chatbot, the AI
Assistant's tool-calling turns, the kernel's own embedding of items and of
semantic filter queries, and a plugin's `ai-request` all go through it. Each send
checks the provider's base URL with `validate_base_url`, takes the configured
timeout from `ai_timeouts` for its plane (request or background) unless the
feature configures its own, as the AI Assistant does, and goes through the
per-operation circuit breaker. A failure comes back as an error whose text is
safe to show; the provider's own words reach the log and nothing else.

The base URL check is the security half of this. Only the embedding call and the
connection test re-checked the URL at send time; the chat, tool and plugin paths
connected to whatever the `ai_providers` row named. The admin form refuses a
private address, but a row can reach `site_config` without the form, and an IP
literal never passes through the hardened client's resolver, so a provider row
naming `http://10.0.0.5` or `http://127.0.0.1:6379` was sent the prompt and the
API key. Each send now refuses it before a byte leaves.

The timeouts that were hard-coded per path (15 s for expansion, 30 s for a
summary or follow-up, 60 s for the writing assistant, 120 s for the chatbot) are
gone: each takes the `ai_timeouts` value for its operation and provider, which an
operator can raise. A streaming answer is bounded at its start, connecting and
the provider's response headers, and after that by the stream parser's per-chunk
idle timeout, so a long answer is not cut off by a timeout sized for a one-shot
call.

There was a request builder and response parser for each path, five pairs in
all, and they had drifted: the search and assist parsers read only the first
Anthropic text block and threw the token counts away, so the writing assistant
recorded every call with no prompt tokens and its whole total as completion
tokens. It now records what the provider reported. One builder and one parser
remain, in `services/ai_tools.rs`, along with the stream parsers in
`services/ai_chat.rs`; the unused `stream_ai_response` in the search routes is
deleted. A plugin's request and response shapes are unchanged. A plugin that
sends more than one system message to an Anthropic provider now has them joined
with a blank line rather than a single newline, and an OpenAI-compatible response
with no choice in it is now `ERR_AI_PROVIDER_ERROR` rather than an empty answer.

Every AI call is now recorded and held to a budget in one place: the send path
on `AiProviderService` that every call already goes through. It checks the
caller's budget before the request leaves and writes the `ai_usage_log` row,
with the prompt and completion tokens the provider reported and a cost
estimate, after it returns, including for embeddings and for a streamed chat,
whose row is written when the stream reports its usage. The per-route copies of
both are gone.

Several paths spent provider money without leaving a trace. AI search's
expansion, summary and follow-up recorded nothing and checked no budget, and
neither did the sentiment classifier or the kernel's embedding of items and
semantic filter queries. The writing assistant (`/api/v1/ai/assist`) checked no
budget and discarded the error if its usage write failed. All of them now record
real usage, and a failed write is logged.

Whose budget a call counts against follows from whose call it is. A signed-in
person's calls count against their per-user or per-role token budget, now also
for the writing assistant and AI search. A plugin's background calls count
against the plugin's token and currency caps, as before. Anonymous visitors
share one site-level cap: the `anonymous user` role's limit on the AI budget
screen (`/admin/system/ai-budgets`), metered over every call made for an
anonymous visitor, which is now recorded under the anonymous user's id rather
than with no user at all. That limit used to be ignored, because the budget
check treated an anonymous caller as unlimited. The sentiment classifier counts
against the anonymous cap too. A spent budget is a 429 from AI search, the
writing assistant and the chatbot, and `ERR_AI_BUDGET_EXCEEDED` to a plugin.

The AI search endpoints now ask for `use ai`, as every other AI route does;
before, anyone who could reach them could spend the site's tokens. So that no
site loses AI search on upgrade, a migration grants `use ai` to every role that
already holds `search content`, which in a default install is the anonymous and
the authenticated role. `use ai` alone sends nothing anywhere else: the chatbot,
the writing assistant and the AI Assistant also ask for `use ai chat` or
`use ai assistant`. One visible effect: the `trovato_ai` plugin's field rules
check `use ai` before running at item save, so on a site that has configured
field rules, a member without `use ai chat` now has the plugin attempt the call
and be refused at the permission gate, instead of being skipped silently.

The writing assistant has its own rate limit bucket, `ai_assist`, at 10
requests a minute, instead of sharing the generic `api` bucket at 100. The
chatbot and the AI Assistant already counted their hourly limits in the shared
Redis limiter.

`trovato_circuit_breaker_ai` now carries an `operation` label and reports every
per-operation breaker the send path uses (`chat`, `embedding` and the rest),
beside `connection_test`, the breaker the admin connection test uses, which was
the only one it reported before. The per-operation breakers are what open when
a provider fails during real traffic, so the gauge read closed while chat was
being refused. A query that selected the unlabelled series still matches; one
that expects a single series now gets one per breaker.

Every kernel JSON endpoint is served under `/api/v1`, every list answers in one
shape, and `/api/v1/openapi.json` is generated from the router. Fifteen paths
were versioned and about thirty-three were not; search was served twice, at
`/api/search` (`limit`, `{query, results, total_pages}`) and `/api/v1/search`
(`per_page`, Live only); the `x-api-version` header was on one router of many;
and the lists came in six shapes: `{items, pagination}`, `{data, total, page,
per_page}`, `{items, page_size}`, `{comments, total}`, gather's `{items,
total_pages, has_next}`, and bare arrays.

A route is now declared once, through `routes::api::ApiRoutes`, with its
handler and its OpenAPI description side by side. The same declaration mounts
it under `/api/v1` with the version header and lists it in the document; a
route served before the prefix existed also keeps its old `/api/...` path as an
alias of the same handler, answered with `Deprecation: true` and marked
`deprecated` in the document, until 2.0. `api_v1::kernel_api` is the one list
the router and the document are both built from, and a unit test refuses an
`/api` route registered any other way, so nothing can be served and not
listed. The integration test that requests every listed path now also requires
a kernel path to reach its route (the version or deprecation header only a
matched route gets), not just to avoid a 404. The hand-written list of eleven
kernel paths is gone; the document lists every kernel route, among them the
batch, token, category, tag, comment, gather, lock, assistant, media, page
builder and content type routes it was missing.

`/api/search` is folded into `/api/v1/search`: one handler, stage-aware as the
unversioned one was, returning `id`, `type`, `title`, `rank`, `snippet` and
`url` per result. Every list, over HTTP and MCP, is `crate::pagination::Page`,
`{data, total, page, per_page}`, and takes `page` and `per_page` with `limit`
and `offset` as aliases. A list the source does not page (categories, tags, a
comment thread, the gathers, the tokens, the content types) is paged in memory,
so it takes the same parameters. The MCP `search` tool now also takes `page`
and `per_page`, `list_items` also takes `limit` and `offset`, and every MCP list
tool answers in the same shape. The rate limiter files an alias in its
versioned route's bucket, so `/api/item/{id}/comments` and
`/api/v1/item/{id}/comments` share the comment limit.

The kernel's scripts and templates use the new paths and read `data`: the
comment form, the API token page, the block editor, the media picker, the
reference autocomplete, the gather builder preview (which read `rows`,
`results` or `data` and so showed nothing for the `items` it was sent) and the
page builder editor, rebuilt.

Each JSON capability now has one path, and HTTP and MCP apply the same rules to
it.

`GET /api/v1/user/export` is gone. It duplicated `/user/data-export`, read the
models and raw SQL directly, gave administrators every account's data on the
`is_admin` flag rather than a permission, and had none of the one-an-hour
`data_export` limit. Nothing in the kernel called it; the account page's
download is the export. The item autocomplete reads its candidates through
`ItemService::title_prefix_candidates` instead of SQL in the route, and now
escapes a backslash in the prefix as well as `%` and `_`. The bearer-token
middleware and the MCP server resolve a token through one function,
`services::api_token::resolve`. Before this they each called the `ApiToken`
model on their own: the MCP server checked the account's status and recorded
the use inline, and the middleware did neither.

The HTTP item reads (`/api/v1/items`, `/api/v1/items/{type}`,
`/api/v1/item/{id}`) now require `access content`, as the MCP tools do. A
caller who is not the site administrator lists published items only, as
`list_items` lists them. Titles over `content::MAX_TITLE_LENGTH` (1000 bytes)
are refused on the HTTP item writes as on MCP. A search or autocomplete query
over `search::MAX_QUERY_LENGTH` (1000 bytes) is refused by the JSON API as by
the MCP `search` tool. The two limits are constants each side reads, no longer
a literal in the MCP crate.

The dead-letter queue's JSON moves from `/admin/queue/dlq` to
`/api/v1/queue/dlq`, listed in the OpenAPI document. It requires
`administer site` instead of the `is_admin` flag, and lists a page in the API's
one shape rather than the first 500 rows.

Content locking works end to end. `trovato_content_locking` is enabled by
default and its heartbeat and break routes had no client, because nothing ever
took a lock. Both item edit forms (`/admin/content/{id}/edit` and
`/item/{id}/edit`) now take the lock when they open. An editor who finds it
held is shown who holds it and when it lapses. A holder of `break content lock`
gets a button that breaks it. A save under another editor's live lock is
refused with 409, and a successful save releases the lock.
`static/js/content-lock.js` sends a heartbeat once a minute while the form is
open. The heartbeat and break answers carry a fresh CSRF token because tokens
are single use. The lock service now exists whenever the plugin is enabled,
not only when it was enabled at boot.

The block editor's image upload (`POST /api/v1/block-editor/upload`, and its
`/api/block-editor/upload` alias) is counted in the `uploads` rate bucket, 10 a
minute, like `/file/upload`. Before this it fell through to the generic `api`
bucket at 100 a minute, so a client could write ten times as many files through
the editor as through the upload form. The admin "send test email" action
(`POST /admin/config/site/test-email`) is counted in the `mail` bucket instead
of the generic `forms` one.

A new test, `every_sensitive_route_is_in_its_named_bucket`, lists every kernel
route that writes files, sends mail or calls an AI provider with the bucket
that bounds it. It checks each one: the route exists, and the middleware puts
it in that bucket, or its handler checks the bucket itself and the middleware
does not count it there twice. `POST /api/v1/ai/assist` is listed as still in
`api`, pending the AI rate buckets.

Config export and import now carry every kind of configuration, the kernel's or
a plugin's, and no runtime state.

Plugin configuration did not round trip. Image styles, redirects, webhooks,
OAuth clients, configuration translations and interface string translations
live in tables a plugin's migrations create, and the list of config entity
types (`ENTITY_TYPE_ORDER`) was a kernel constant with no way for a plugin to
add to it; which plugins were enabled did not travel either. A manifest can now
name its configuration tables under `[config] tables` (additive; plugin API
0.105), and export writes each row as `<table>.<primary key>.yml`, a map of
column to value converted by Postgres itself (`to_jsonb` out,
`jsonb_populate_record` in), after the kernel's types. A `plugin_status` entity
records whether each plugin is enabled. Import applies it first, runs the
enabled plugins' migrations as boot would, and checks every row before writing
anything: its plugin must end up enabled, every column must exist, and each
value must fit its column. The six in-tree plugins that own configuration
declare their tables.

Variable export dumped all of `site_config`, so `installed`, `update_status`,
`update_check` and `queue_runner`, which describe one deployment rather than the
site, went into every config set and came back out on the next install.
`RUNTIME_STATE_KEYS` names them; export skips them and import ignores them with
a warning.

An export would not import into another install. Plugin migrations seed roles,
URL aliases and image styles under random ids, so the same role, alias or style
had a different id on the target and the import hit the unique name, or kept the
target's id. A role, URL alias or plugin row now takes over the row holding its
name, under the id its file names, with the role's grants and members moved
across; role, stage and URL alias timestamps from the file are applied on
update; and export skips, with a warning, the gather queries plugins seed that
no import would accept (the OAuth and webhook admin lists, over tables gathers
may not read, and `scheduled_items`, whose filter field the validator refuses). `trovato_oauth2` had never run its fourth migration, which
its manifest did not list, so the `scopes` column every OAuth client lookup
selects did not exist and every lookup found no client; the manifest lists it
now.

Import runs in its own process and cannot reach a running server's caches, and
the server reads plugin status only at boot, so import now says to restart it.

The stage-aware config storage (`StageAwareConfigStorage`,
`AppState::config_storage_for_stage`) had no caller and is deleted.
Configuration is not staged.

A two node test harness: `docker-compose.cluster.yml` runs two kernels from
the release image against one Postgres, one Redis and one shared volume, behind
HAProxy balancing round robin with no stickiness. A request carrying
`X-Trovato-Node: a` or `b` is pinned to that node, in proxy configuration only,
so a test can write on one node and read on the other. Thirteen scenarios in
`crates/kernel/tests/cluster_test.rs` drive it over HTTP when
`TROVATO_CLUSTER_URL` is set and skip otherwise, and a new `cluster` CI job
builds the image, brings the harness up and runs them, printing both kernels'
logs on failure.

Sessions, form tokens, passkey challenges, uploads on the shared volume,
blocking a user and the cron lock already work across nodes. Nine scenarios
fail today and are ignored, each naming the change that fixes it: in-process
caches invalidated only on the node that wrote (roles, items, types, terms,
gather queries, redirects, locale strings, the cache layer's local tier, and an
import run from another process), chat limits counted per node, a
`tap_install` that two kernels booting together both run, a plugin toggle
applied live on one node only, and a Pagefind index written to one node's
disk. The CI test shards leave `cluster_test` out of their round robin
partition, where it would only skip itself.

Every in-process cache that holds data from Postgres is now current on every
server within about a second of a write, on one server or many, with nothing to
configure.

Each such cache is a kind (permissions, users, items, field access, content
types, categories, gather queries, redirects, locale strings and config menus).
A write clears its own server's copy as before and then adds one to that kind's
counter in the Redis hash `cache:gen`. Every server reads the hash every 250 ms
and clears a kind whose counter changed since its last read; a difference, not
an increase, so a restarted Redis (which empties the hash) clears everything
once. A whole kind clears on a bump. When Redis cannot be read the server logs
once and the caches' existing TTLs bound how stale they get. A role revoked or a
user blocked on one server used to keep working on another for up to the
permission or user cache TTL.

Writes that skipped their service's invalidation now bump too: a plugin's
writes through the database host functions (by the table they name, so the
scheduled publishing plugin's raw `UPDATE item` reaches the item cache), a user
deletion (its items change author), a role rename, search reindexing and its
queued cron task, AI budget overrides, stage writes, a stage publish, and a
gather query's canonical URL, whose redirect is now written when the query is
saved rather than at the next periodic reload. `config import`, the plugin
commands and `user role-add`, `role-remove` and `reset-password` bump every
kind they could have touched, so `config import` no longer asks for a restart;
it says so only when it changed which plugins are enabled, which still waits
for one.

Two caches are gone rather than wired. The cache layer's in-process tier is
removed, so `get` and `set` go straight to Redis and a tag invalidated by one
server is invalidated for all; the `layer_l1` cache metrics are gone and
`CacheStats` reports the Redis key count. The stage cache and stage view are
removed too: stages and the workflow are read from Postgres (two small
queries), and `clear_stage_cache` is gone, along with the `item_stage` and
`stage_view` metrics. Locale strings now reload in place on a bump, so an
imported translation appears without a restart. Content types and gather
queries deleted elsewhere now leave the other servers on reload.

The session store's Redis pool had no reconnect policy, so after Redis
restarted every request that touched a session hung for good. It now
reconnects every half second and gives up on a command after five seconds.

The content type registry, the gather service and the locale service now
serialize their own writes with their reloads, so a reload that read the rows
just before a write on the same server can no longer put the old definition
back or drop one created meanwhile.

The chat, assistant and AI provider rate limits now count in Redis, keyed as
before (per user or IP per hour for chat and the assistant, per provider per
minute for AI requests), so a site on several servers refuses at the limit
rather than at the limit times the number of servers. They fail open when Redis
cannot be reached, like the other Redis limits. The AI circuit breakers stay
per server on purpose.

A plugin's `tap_install` is claimed in `plugin_status` before it is dispatched,
so when several servers boot together against a database with a plugin
pending, exactly one of them runs it. As before, a dispatch that fails stays
marked; reset `tap_install_called` to run it again.

Enabling or disabling a plugin on the admin page now changes only its stored
status, as the CLI already did. The running server keeps the plugins it
started with, so its routes, taps and menus no longer disagree with the other
servers'. The page says that changes take effect when the server restarts (all
servers, if more than one) and marks each plugin whose stored status waits for
a restart. Dependencies are checked against the stored statuses, and a newly
enabled plugin's permissions register at that restart, as they did whenever its
module was not yet loaded.

The server checks its uploads directory at startup: it writes, reads back and
deletes a uniquely named file in `UPLOADS_DIR`, and refuses to start, naming
the directory, if any step fails. A missing or read-only mount used to surface
only when someone uploaded a file, and on several servers only on the one whose
mount was broken.

`PAGEFIND_OUTPUT_DIR` says where the Pagefind index is built and served from.
It defaults to `pagefind` under the first `STATIC_DIR`, where the index has
always gone, and its URL stays `/static/pagefind/`; files under that URL are
now served from this directory only. On several servers, point it into the
shared volume, so the index one server builds is the one every server serves.
The swap is unchanged in kind: the build is made beside the output directory,
in the same parent, then the old index is renamed aside and the new one
renamed in. The build and backup directories are now named with a fresh id
rather than the process id, which is 1 in every container.

New `docs/deployment/multi-server.md`: what several servers share (one
Postgres, one Redis, one read-write volume such as a `ReadWriteMany` PVC, NFS
or EFS, for uploads and the Pagefind index), how a write reaches every server
within about 250 ms, why the AI circuit breakers stay per server, the rolling
restart that applies a plugin enable or disable, and how to run the two server
harness locally. The Pagefind swap has been tested on a Docker volume only.
`INSTALL.md` points to it, and `UPGRADING.md` lists what changed for every site.
The cluster compose file's test command now runs the scenarios one at a time,
as they require.

Two more tests that count a warm page's queries (`gather_tile_test`,
`menu_tree_test`) now let the cache generations their own writes bumped be
seen before they warm and count, as the revision page test already did; a
clear landing between the warm-up and the count made the count one higher.

Schema migrations can run as their own step, and a release is rolled back by
redeploying the previous image. `trovato migrate` applies pending kernel and
plugin migrations under one Postgres advisory lock held on a connection of its
own, so several runs at once apply each migration exactly once and a failed run
cannot leave the lock behind in a pool. Before this, sqlx's lock covered the
kernel half only, and plugin migrations took no lock at all. `--status` and
`--dry-run` change nothing.

`MIGRATE_ON_START` (default `true`, today's behaviour) can be turned off, and
then `serve` never changes the schema. It waits, answering `/health/live` 200
and `/health/ready` 503, instead of crash looping, and becomes ready by itself
once the migration has run. A binary now accepts applied migrations it does not
know, unless one is named incompatible. They used to stop it at boot, which made
a rollback by image impossible. Every new migration must keep the previous
release working (expand, then contract). A unit test run by the pre-commit
script and CI refuses the statements that break the previous release (a drop, including
of an index or constraint, a rename,
retype, `SET NOT NULL`, `DROP DEFAULT`, `TRUNCATE`, a `NOT NULL` column without
a default, a new constraint, a plain or unique index build on an existing table; see
`UPGRADING.md`), unless the file names the release that
stopped using the object. A CI job starts the previous binary on the schema the
change produces.

`/health/live` and `/health/ready` are new. `/health` no longer gives pool
statistics and the services report to anonymous callers, and none of the three
uses a session or a visitor rate-limit bucket. `/health` and `/health/ready`
check Postgres and Redis at most once a second; `/health/live` checks nothing.
On SIGTERM the server reports not ready at once, keeps serving for
`SHUTDOWN_DRAIN_SECS` (5 under Kubernetes, 0 elsewhere), and finishes
in-flight requests before it exits. Example Kubernetes manifests are
under `deploy/kubernetes/`.

Performance tests now catch a page that starts doing more work. `trovato
generate` writes a deterministic site of any size (members, an administrator,
tags, blog posts each with a revision, and comments) with set-based SQL, so
Postgres does the work. It writes only into an empty database and has no default
password.

The new `performance_test` builds a small generated site on a scratch database
with a fixed plugin set. It loads the front page, an item page with comments, a
tag page, the feed, search, the login page and the admin content list, as each
kind of visitor, with cold and warm caches. It compares the exact number of
statements, cache lookups, misses and writes, plugin calls, host calls, gather
runs and renders with `crates/kernel/tests/fixtures/performance_counts.toml`,
statement by statement, each named by its fingerprint and normalised text. Each
visit also checks that the visitor is signed in or not as it should be and that
the page shows its real content. The counts are the same on every machine and
every run, so they run on every pull request, and a change in a page's work
shows in review as a diff of that file.
`docs/performance-tests.md` explains how to add a page and what to do when one
fails. The request profile now keeps each statement's normalised text next to
its fingerprint, and a quoted identifier left unterminated no longer carries
the rest of the statement into it.

Traces can now leave the process. With `TRACING_EXPORT=otlp` the kernel sends its Gander spans (the request, every database statement, Redis cache call, plugin tap and host call, gather, render, queue job and outgoing HTTP call) over OTLP/HTTP to whatever `OTEL_EXPORTER_OTLP_ENDPOINT` names: Jaeger, Grafana Tempo, Honeycomb, Datadog or an OpenTelemetry Collector. Statements become `db.query` spans after the fact, from sqlx's own report, carrying the fingerprint and the normalised SQL with every literal replaced. A batch processor on its own thread does the sending, so no request waits on it; `OTEL_TRACES_SAMPLER_ARG` keeps a share of traces. Export is off by default, and off it costs nothing: no layer is interested in the spans, so they are never built.

`/metrics` no longer answers the internet. It used to give anyone who could reach the server the route templates, request rates, pool sizes and circuit breaker states. It now answers a loopback caller that is not a reverse proxy relaying someone else, or a request carrying `Authorization: Bearer` with the new `METRICS_TOKEN` (at least 32 characters); anything else gets a 404. A Prometheus on another host needs the token. A reverse proxy on the same host must add a forwarding header (`X-Forwarded-For`, `X-Real-IP` or `Forwarded`) to what it passes on, or refuse `/metrics` itself: nginx adds none unless configured to (see `docs/tracing.md`).

The `method` label of the request metrics, the method in a request span and the method on the request's log lines name only the standard HTTP methods; a method a client made up is counted as `_OTHER`, so it no longer adds a series of its own to `/metrics`.

Every request now runs inside a request profile that counts its database
statements, cache lookups and writes, plugin calls, host calls, gather runs,
template renders and outgoing HTTP requests, and the kernel has spans at the
few places all of that passes through: sqlx's own statement event (so no query
call site is wrapped), `CacheLayer`, the in-process cache counter, the plugin
export call, the host call wrapper, the gather runner, the theme engine's
render entry points, every outgoing HTTP call the kernel makes (plugin HTTP and AI calls, search, the
assistant and the release check), and the queue job
runner. The spans use the tracing target `gander` at `TRACE`, so with tracing
off, the default, nothing is interested in them and creating one allocates
nothing (`tracing_off_test` measures it). The design is
`docs/design/tracing-and-performance-tests.md`.

An administrator's responses carry the counts and durations in
`Server-Timing` (`db`, `cache`, `plugin`, `render`, `http`); everyone else
gets `total;dur=` exactly as before. `SERVER_TIMING=all` shows them to every
visitor and `SERVER_TIMING=off` to no one. The slow request line now carries
the route template and the same breakdown. `db_query_duration_seconds` and
`tap_duration_seconds`, registered since the start and never fed, are now fed
while serving a request, and `http_request_db_queries` (statements per request
by route) and `cache_sets_total` are new.

The request span `tower_http` logs now carries the matched route template
instead of the URI. The default `RUST_LOG` prints that span on every log line
inside a request, so a search term, a password reset token or an email in a
URL was written to the log with each one. `telemetry_privacy_test` seeds a
password, an email, a reset token, an API token and a search term and fails if
any of them reaches a log line, a span, `Server-Timing` or `/metrics`.

Mail is no longer lost when the mail server is down. Every message Trovato
sends (registration and email change verification, password resets, account
recovery codes and notices, comment notifications, the administrator's
registration notice, the SMTP test mail and plugin mail) is now a row in a new
`mail_outbox` table, written with the same database transaction as the event
that caused it where there is one: a registration that fails sends nothing,
and one that succeeds always sends. Mail workers in every web process claim
due messages with `FOR UPDATE SKIP LOCKED`, send them over SMTP (now with
pooled connections), and retry temporary failures with exponential backoff and
jitter for about six hours; a permanent refusal fails the message at once.
Every message ends sent, failed with its reason, suppressed or cancelled.
Before, each sender talked to SMTP inline or on a spawned task and logged and
forgot a failure, and the circuit breaker stopped attempts without retrying
them (BL-149).

Reset links, verification links and recovery codes are minted by the worker at
send time, so they are never stored in the clear, and a link sent after an
outage is fresh. Transactional mail is never delayed behind bulk mail; each
blog has hourly and daily limits, past which its mail is held, not dropped;
addresses that hard bounced or complained are suppressed network wide;
finished messages lose their bodies after 7 days and are deleted after 90.
The transport is a `MailTransport` trait, so an HTTP API provider can be added.
Plugin mail calls keep their signatures and codes; the WIT now says that
success means accepted for delivery, not delivered. Settings and DNS records
are in `docs/deployment/mail.md`; the design is
`docs/design/mail-queue.md`.

Changelog entries are written as fragments now, one file per pull request under
`changelog.d/`, and folded into `CHANGELOG.md` at release time by
`scripts/changelog-fold.sh`.

Two branches open at once both appended to `## Unreleased`, at the same place in
the same file, so every second one merged with a conflict that carried no
disagreement: each side wanted its own paragraph and neither touched the other's.
The entry now waits in a file named for the branch, which no other branch has a
reason to write to. `scripts/changelog-check.sh` is the gate, run in CI as
`Changelog Fragment`: a pull request that changes `crates/`, `plugins/` or
`templates/` and adds no fragment fails, and a change that needs no entry says so
in a fragment whose body is the single word `none` rather than by omitting one.

The fold appends fragments to a section in a deterministic order and deletes the
ones it folded, so `changelog.d/` is empty on a released tree and every file in it
is an entry that has not shipped. The entries already sitting under
`## Unreleased` were left exactly where they are; nothing was retrofitted.

Item writes now honour field-level edit access, and whether an item is published
is a permission rather than a value the client picks.

Field access was built and tested for reading. The decision has always taken an
operation and the design has always named two, `view` and `edit`, but every
caller asked for `view` and no write path asked at all — so a user who could
edit an item could overwrite a field they were not even allowed to see, and the
edit form showed them its current value to do it with. `create`, `update`,
`revert_to_revision` and `save_translation` now run the `edit` decision on the
client's own submission, before the presave tap, and refuse a change to a field
the user may not edit, naming the field. A denied field the submission leaves out
is copied back from the stored item rather than erased, which is what makes a
partial JSON update safe. The item forms render a field the viewer may not see
not at all, and one they may see but not edit as a disabled control, so a
refusal only happens on a request built by hand.

Whether an item is published came straight from the client — a `status` key in a
JSON body, a checkbox on the form — with no permission behind it, so anyone who
could create content could put it on the live site. The new `publish content`
permission is checked on every item write: without it a create asking to publish
is refused, a create that says nothing stores a draft, and an update or revert
that changes the published state is refused in either direction. Bulk publish and
unpublish on the admin content screen ask for it too. The published checkbox is
rendered only for a user who may publish, and travels with a hidden marker
saying it was rendered, because an unchecked checkbox posts nothing and the
handler read its absence as "unpublish". A forward-only migration grants
`publish content` once to every role that already held `create content`,
`edit own content`, `edit any content` or a per-type `create … content`, so no
existing site loses the ability to publish on upgrade; nothing implies it
afterwards.

The field-access decision cache is keyed on the viewer now — user id and
authenticated flag alongside the permission hash — not on the permission hash
alone. The payload a plugin receives carries the viewer's identity, and the
contract asking a plugin to decide from permissions alone was advice, not
enforcement: one plugin author taking the payload at face value meant one user's
decision was served to every other user holding the same permissions for up to
five minutes, an anonymous visitor and a logged-in user with the same effective
set included. Anonymous viewers all carry the nil id, so they still share
entries with each other.

A blog teaser in the blog listing has text in it.

`templates/gather/query--blog_listing.html` guarded its teaser on
`row.fields.body`. `body` is the field the kernel `page` type uses;
`trovato_blog` defines a blog post's long text as `field_body`
(`tap_item_info`), so the guard was never true and every post in the listing
rendered as a title, a date and a "Read more" link with nothing between them.
The template was written against the wrong content model, and because the miss
was a silent `is defined` guard rather than an error, it rendered a plausible
page instead of failing.

The template reads `field_body`. Neither field is renamed: that is a content
model change for existing sites and needs a migration and a decision.
`blog_listing_test` seeds a published post through the blog's own gather query
and requires the listing to show its body text.

Every route that serves file bytes or file metadata now applies the same access rule as the main file route. The image style route, the file info route (`GET /file/{id}`) and the media browser (`GET /api/v1/media/browse`) served file data without the referencing item check that `/files/{path}` already made, so a derivative (including one already cached on disk), a file's metadata, or a media listing could expose a file attached only to restricted content. The style route now checks the original on every request before reading its cache; file info answers 404 for a file the caller may not fetch; and the media browser shows a user without `access files` or `administer files` only their own uploads and files on published live items, with the total counted under the same filter. The style route also resolved the original with a bare path instead of its `local://` URI, so every uncached derivative failed with a 500; it now loads the original correctly. The plain file route no longer accepts `.` or empty path components or the `styles/` tree, so derivatives are reachable only through the style route.

Served files were marked `public` for shared caches regardless of who was allowed to see them. A file or derivative is now publicly cacheable only when an anonymous visitor would be allowed the same file, and is `private, no-store` otherwise. The uploads directory holds private originals and derivatives in one tree and must never be served directly by a web server or CDN; `INSTALL.md` and the infrastructure design note now say so.

The ad hoc gather endpoint, `POST /api/gather/query`, now requires `administer
site`, and a gather definition can no longer read account or credential tables.
The endpoint had no permission check at all, so anyone could post a definition,
and gather validation checked only that table and column names were well formed,
never which tables or columns a definition was allowed to read. A join from
content to an account table therefore projected that table's columns straight
into the response. The per-row access check looks only at the base content item
and never saw them.

The policy now lives in `GatherService::validate_definition`, which every
execution path runs through, so it holds for named gathers, includes, and the
definitions plugin migrations and config imports write directly to
`gather_query`, not just for the one route. A base table must be `item` or a
registered record type. A join may only reach a short allowlist of content
tables. Account, credential, token, permission and secret tables are refused
outright, and so are columns named like secrets. `register_query` and config
import now refuse an invalid definition when it is saved, naming the rule it
broke. The unused `core.user_list` default view, which read the `users` table, is
no longer registered.

The four `item-api` host functions decide now, as the user the plugin is
acting for.

They decided nothing before. `get-item`, `query-items`, `save-item` and
`delete-item` read and wrote through the `Item` model directly and used the
requesting user only as an author id. The module uses the model on purpose, to
avoid dispatching a tap from inside a tap, and the cost of that was supposed to
be the taps; it was also the access decision, because the decision lives in
`ItemService` beside them. So a plugin handling an anonymous visitor's request
could read an unpublished item and its restricted fields by id, list them, and
rewrite or delete any item on the site. Nothing about the request reached the
item: the user was an author id and the id was a lookup key.

A request-scoped call now gets that user's own answer. `get-item` requires
`view` and drops the fields the user may not see; `query-items` returns what
`filter_page_for_view` keeps, the same seam the REST, SSR, gather and search
read paths use; `save-item` requires `edit` on the item that is there for an
update and `create {type} content` for a create, the permission the item routes
check for the same act; `delete-item` requires `delete`. A denied read is
indistinguishable from a missing item and writes `null`, so the answer cannot
be used to confirm that a draft exists. A denied write returns the new
`ERR_ITEM_ACCESS_DENIED`.

Writes still go straight to the model. The insert, update and delete taps do
not fire from `item-api`, which is the contract the module exists for; only the
decision is new, and only the decision reaches `ItemService`.

Background callers need a capability. Cron and the queue worker run under the
kernel-internal background principal, which carries no identity and holds no
permissions, so there is no user whose authority a call could act with. Rather
than fall back to kernel authority silently — which is what every call did
before — a plugin that needs it declares `item_background = true`, on the same
manifest plane as `ai_background` and for the same reason. Without it, an
`item-api` call from a background context returns `ERR_ITEM_BACKGROUND_DENIED`.
No in-tree plugin imports the interface; the SDK has no binding for it, so this
reaches external plugins that hand-roll the imports.

Two pieces of plumbing come with it, both shaped by things that would otherwise
break.

The access decision needs an `ItemService`, and `ItemService` already holds a
`RequestServices`, so putting one inside `RequestServices` is a reference cycle
that never frees. The services hold a `Weak` handle instead, in a shared cell
set once by `AppState` after the service is built. The cell is shared rather
than copied so the clone the service itself took a moment earlier sees the
binding, and weak so neither side keeps the other alive. A handle that cannot
be upgraded reports no services rather than deciding without one.

And the decision dispatches `tap_item_access`, so a plugin handling that tap
could call `get-item`, ask for another decision, and dispatch the tap again
without end. An `item-api` call made while a decision is in progress fails
closed. The marker is a task-local rather than a field on the request state
because the dispatch does not carry the caller's state: `ItemService::tap_state`
builds a fresh `RequestState` for the handler, so the invocation depth
`plugin-api` carries does not cross that boundary and neither would a flag
beside it. Tap dispatch awaits its handlers inline, so a task-local does.

Separately, a plugin call has a wall clock ceiling. Guest CPU was already
bounded by epoch interruption, and the deadline callback extended that budget by
however long a call had spent parked inside host functions, so that waiting on
a slow provider was not billed as computing. That extension had no ceiling: a
guest looping over slow host calls bought another second of deadline for every
second it waited and was never interrupted, and request-scoped dispatch has no
other clock, so one plugin could hold a request open for as long as it liked.
Total elapsed time is bounded now as well, 120 seconds for a request-scoped
call and 900 for a background one, both far above the epoch budgets beside them
and both overridable. A call stopped by this bound is recorded as its own
outcome, distinct from CPU exhaustion, because the two say different things
about what the plugin was doing; a queue job stopped by it is dead-lettered
with its own reason rather than retried.

Every image published to `ghcr.io/jeremyandrews/trovato` now carries both
`linux/amd64` and `linux/arm64`, and the publish workflow fails rather than
pushing a tag that is missing either. arm64 was built only for release tags,
because it cross-built under QEMU and took over three hours, while every push
to `main` published a nightly under a release shaped version tag such as
`0.104.154`. Those nightly tags held amd64 alone, and nothing in the manifest
step checked which platforms it was about to publish, so the single platform
image went out under the full tag set unnoticed. A consumer that tracks
versions read `0.104.154` as newer than the `0.102.0` release, pulled it on
arm64, and failed with `no match for platform in manifest`. arm64 now builds on
GitHub's native `ubuntu-24.04-arm` runner on every run, nightly and release
alike, with no QEMU anywhere, so there is no longer a reason to skip it. The
manifest job refuses to run unless it has exactly one digest from each
platform, and after pushing it inspects the published version tag and fails
unless both platforms are really there. The workflow also takes a
`concurrency` group per ref, after one `v0.104.0` push started two runs that
raced each other over the `0.104.0`, `0.104` and `latest` tags and paid for the
three hour QEMU build twice.

A `SITE_URL` that names a bare IP address (`http://10.20.250.7`, LAN deployments
without DNS) no longer stops the kernel from starting. At startup `AppState`
built its WebAuthn relying party straight from `SITE_URL` and propagated any
failure with `?`; `webauthn-rs` refuses an IP address as a relying party id
("rp_id is not an effective_domain of rp_origin"), so an IP `SITE_URL` aborted
the whole process before a single route could serve. Browsers do not allow
WebAuthn on an IP origin either, so refusing to start was never buying
anything. The WebAuthn relying party is now optional: when `SITE_URL` cannot
be one, the kernel logs a warning naming the reason and starts with passkeys
disabled, rather than failing to start. The passkey registration and login
endpoints answer a plain "Passkeys are not available on this site" instead of
reaching into a relying party that does not exist, the login and passkey
management pages say the same in place of the registration/sign-in controls,
and password sign-in is unaffected. Every other absolute URL `SITE_URL` feeds
— feed and sitemap links, canonical links, reset and verification mail — was
already built by plain string formatting and never depended on WebAuthn, so
those already carried the IP address correctly; only startup was the problem.
A hostname `SITE_URL` configures WebAuthn exactly as before.

A Gather filter, sort, exposed filter, contextual argument or option list on a
`fields.<name>` now matches a stored `{"value": "Barga"}` exactly as it matches
a stored `"Barga"`. The field-value module has owned a field's shapes since
B125: an edit form keeps a wrapped value wrapped, and importers and plugins
store wrapped values legitimately — some local databases carry upward of a
hundred `conference` items whose `field_city` is one. Gather never read that
shape. Its filter and sort expressions extracted `fields->>'city'` directly,
so a wrapped row's comparison text was the literal string
`{"value": "Barga"}`, never the city; `GatherService`'s two hand-written
option-list queries (`fetch_distinct_values`, `fetch_faceted_distinct_values`,
and the facet scope condition both build) had the same bug, so an exposed
filter's dropdown showed the wrapped JSON as one of its choices. A second
reader already existed and disagreed: the `item_search_vector` SQL function
unwraps the same shape for full-text indexing, so gather and search read one
row differently.

One function now owns reading a `fields` value as SQL, mirroring
`field_value.rs`'s `scalar_of`: the wrapper's payload when there is one,
otherwise the bare value. Every gather comparison, the sort expression and
both hand-written option-list queries call it; nothing else does. No index was
added — the GIN index on `fields` was never used for a text comparison to
begin with, and the new expression measures within noise of the old one.

Folded in: a filter value spelled as an unhyphenated or uppercase UUID now
compares as the exact text it was written as. `FilterValue`'s untagged enum
tries a `Uuid` variant before its string catch-all, and `uuid`'s deserializer
accepts the simple, braced, URN and uppercase spellings alongside the
canonical one — so any of those in a stored filter definition silently became
the canonical lowercase-hyphenated string before the comparison ran, which
never matched a JSONB text field storing one of the other spellings. The
`Uuid` variant now deserializes only from that canonical spelling; every other
spelling falls through to the string variant unchanged.

A gather row read in a translated language now carries the translation's
field values — body, summary, image alt text, every `fields.*` key — falling
back to the original item only for a key the translation does not set. The
title was already translated, because it lives in its own column; every other
field came from `item.fields`, merged with the translation as
`COALESCE(_tr.fields || item.fields, item.fields)`. PostgreSQL's `||` on JSONB
lets the *right* operand win on a duplicate key, and `item.fields` was the
right operand, so the original's keys overrode the translation's rather than
the other way around: a Spanish listing page showed an English body and an
English image alt text under a Spanish title.

The merge now puts the translation on the right (`item.fields || _tr.fields`),
and the same `COALESCE`-overlay rule is applied to the gather query builder's
other SQL projection, an explicit `fields.x` select, which previously read
straight from the base item and showed no translation at all regardless of
viewer language. The item page's own overlay (`routes/helpers.rs`) already
applied this rule correctly and needed no change; it's the reference the SQL
side was brought into line with.

A plugin can now resolve a user name to the same public account record
`load_users` already answers for an id: `host::find_user_by_name` for an
exact, case-insensitive match (the same match login makes), and
`host::find_users_by_name_prefix` for a short, sorted list an autocomplete
widget can offer as a person types. Both need the `user_directory` capability
`load_users` already needs, and neither ever answers an address, a password
hash or a user's `data`.

Before this, a plugin's own "add member by name" form could only match names
the plugin had already seen go through its own tables: `users` is a protected
table no plugin's raw SQL can read, correctly, since it holds hashes and
emails, but that also meant an account created straight on the site, never
mentioned to the plugin by id, answered "no such user" to a search by the
name right there on the account page. The new calls close that gap at the
kernel boundary instead of leaving every plugin to work around it.

Neither call filters a blocked account or the anonymous account out of its
answer — the same visibility rule `load_users` already has, and the one the
kernel's own `/admin/people` listing uses for the capability's calling
context: nothing beyond the capability gate itself.

The exact match reuses the unique index login already matches names against
(`idx_users_name_unique`, on `LOWER(name)`). The prefix match needed a new
one: under the official Postgres image's default `en_US.utf8` locale, neither
that index nor a plain `ILIKE` can turn a prefix search into a range scan, so
it fell back to a sequential scan of the whole table (180ms against 200k
seeded rows in testing). `idx_users_name_prefix`, a `text_pattern_ops` index
on `LOWER(name)`, answers the same query in under 1ms regardless of the
database's locale. The prefix itself is escaped before it reaches the query,
so a literal `_` or `%` a person typed matches itself rather than acting as a
wildcard.

A tap or a plugin-served route (`tap_api`) could carry at most 64 KB of input
or output. Output failed first in practice — the `#[plugin_tap]` macro wrote a
tap's JSON result into a `static mut [u8; 65536]` and refused anything larger —
but input was capped the same way: the kernel's dispatcher wrote it at a
hardcoded offset 0 inside the plugin's own linear memory, which was only safe
because a fresh module's first page happened to be unused scratch space, not
because anything made that true above 64 KB. A plugin route's request body had
its own independent 256 KB cap in `routes/plugin_api.rs`, and the SDK's own
host-function helpers used a third number, 256 KB, for an unrelated protocol —
three guesses at what was conceptually one channel. The motivating case: a
site plugin could not render a 54 KB field through a tap, because the output
side of that channel was the tightest of the three.

Both directions now share one ceiling, `trovato_sdk::abi::PLUGIN_IO_MAX_BYTES`
— 8 MiB, comfortably above any real page and well inside the 64 MiB a plugin
may use in total for one call. Neither side writes into memory it does not
own any more: a plugin built against the current SDK exports an allocator,
`__trovato_alloc`, and the kernel asks it for input scratch space instead of
guessing an offset; a tap's output is leaked from a `Vec` the plugin itself
allocated instead of a fixed-size static buffer. Sending or returning more than
the limit is refused with a clear, documented error — never silent
truncation — and a buggy or hostile guest that lies about an output's length
is caught by the same check rather than reading whatever the claimed bounds
happen to overlap in memory, which is what the pre-existing protocol did.

**Plugin ABI, additive.** The plugin/kernel boundary is frozen: an
already-compiled `.wasm` must keep working. This is new surface, not a change
to existing surface, and the new allocator export's *presence* is the
compatibility signal the dispatcher checks, so no manifest version bump was
needed or made. A plugin compiled before this change has no such export and
runs through the old offset-0 protocol unchanged, at the old 64 KiB ceiling. A
plugin rebuilt against the current SDK exports the allocator and gets the
larger ceiling automatically — every in-tree plugin was rebuilt as part of
this change. `routes/plugin_api.rs`'s request-body cap now equals the shared
constant instead of carrying its own number, and the plugin API router raises
axum's framework-default 2 MB body limit to match, since the shared ceiling
sits above it.

A small tap now costs one extra guest call: the host asks the plugin's new
allocator for input scratch space before writing into it, instead of writing
at a hardcoded offset with no call at all. Measured over 500 calls of a
32-byte echo tap against the same benign fixture both ways: roughly 504 μs/
call before (writing at offset 0, no allocator call) versus roughly 630 μs/
call after (one allocator call added) in an unoptimized debug build. The
difference is the one extra call, not the allocation itself, and it is
dwarfed by the per-call cost both numbers already include — instantiating a
fresh `Store` and module per call — which a real request path's millisecond-
scale DB and network I/O will not perceive.

A plugin route's form can now include a file field (BL-107, G-FILE-NO-HOST-API). A plugin-served request's body was UTF-8 text capped at 256 KiB for the trip across the WASM boundary, so a multipart upload was either over that cap or opaque bytes no plugin could store, and there was no host call to store one through anyway — a site workaround was not an option, since file storage has exactly one path and it lives in the kernel.

The kernel now parses `multipart/form-data` for a plugin route itself, before dispatch. Every part carrying a filename is stored through the same `FileService` `/file/upload` already uses — the same size limit, MIME allowlist and magic-byte check, `Temporary` until referenced — and the plugin receives `ApiRequest::files`, a list of `UploadedFile` ids and metadata, never bytes. The form's other fields arrive re-encoded as a JSON object in `ApiRequest::body`, so a handler reads one shape whether the caller posted JSON or a multipart form. CSRF is checked against whichever form a caller used (header, a url-encoded `_token` field, or now a multipart one) before any file is written to storage, so a request failing that check costs memory, never storage. A plugin attaches the stored id to an item through the existing `item-api` `save-item` call, which promotes the file out of `Temporary` exactly as it would for a person's own upload widget — a plugin never touches file storage directly, and there is still exactly one upload path.

An item's edit lock now lapses 3 minutes after the holder's last heartbeat,
not 15. PR 242 had the edit forms take the lock on open and heartbeat it every
minute, but left the lease at 900 seconds, so a closed tab, a navigated-away
tab or a crashed browser — none of which can send a heartbeat again — blocked
every other editor from the item for up to 15 minutes with nobody at the
keyboard. Three missed heartbeats is enough margin for ordinary jitter and
still a short wait.

A well-behaved exit no longer has to wait out even that: the edit form now
beacons a release, via `navigator.sendBeacon` on `pagehide`, to a new
`/api/v1/lock/release`, so closing the tab or navigating away frees the item
immediately. A beacon cannot set a custom header, so this route is the one
place the CSRF token travels in a form-urlencoded `_token` field rather than
`X-CSRF-Token`; the server accepts either. A browser process killed outright
still gets no chance to send it, which is what the shorter lease is for. The
form also sends an extra heartbeat when a backgrounded tab becomes visible
again, since a hidden tab's timers can be throttled or frozen well past a
minute.

A feed's `Last-Modified` tracked only its entries, so editing the channel's
title, description, or the alias its link resolved through left the header
where it was. A reader polling with `If-Modified-Since` alone (no
`If-None-Match`, which already caught this through the `ETag`) kept getting
`304 Not Modified` and the old channel values after such an edit.

`Last-Modified` is now the latest of the newest entry's `changed`, the gather
query's own `changed` (already covering a title or description edit, since
those live in the feed's config), and, when the channel link resolves through
a `url_alias` row instead of a fixed path, that row's own `changed`. The alias
had no reliable change time before this: editing one rewrites its row in
place, leaving `created` as it was. `url_alias` gets a `changed` column, set on
insert and on every update, backfilled from `created` for existing rows.

The gap analysis skipped an actions/trigger system, reasoning that the tap
system plus webhooks already cover the same ground. That held for a plugin
author, and said nothing about trovato.blog's blog owners, who are not
developers and cannot write a tap. `docs/design/rules.md` reverses the
decision for that audience and designs an events/conditions/actions layer
with no expression language and no code action: an additive kernel event
catalogue every tap already dispatches into automatically, a fixed condition
and action set that calls the same kernel services and permission checks as
any other write, and a per-blog, least-privilege service account that can
never hold more than its owner granted it. `docs/alignment/gap-analysis.md`
and `docs/alignment/roadmap.md` record the reversal, and `docs/BACKLOG.md`
gets ten new rows (BL-154 to BL-163) for the build parts that follow, in
dependency order. Nothing in this change is built yet; it is the design the
build parts will implement against.

Trovato's performance claim has a written plan now: `docs/performance/PLAN.md`
sets per-scenario latency and throughput targets at 10 thousand, 1 million and
10 million items, names `scripts/bench.sh` and the query count fixtures as the
one way to measure them, states the guardrails a performance change follows,
reads where today's time goes in the item, gather, search, session and save
paths, and ranks the first ten fixes. No code changes with it.

Raw SQL from a plugin is parsed now, and there is a floor under the table
allowlist.

The allowlist a plugin's structured `db` calls are confined to was built from
the tables its own migrations create, unioned with the `db_tables` list in its
manifest. Both halves are written by the plugin, so the allowlist was a request
rather than a limit: a manifest saying `db_tables = ["users"]` put the users
table in reach of a structured `select`, and a migration reading
`CREATE TABLE IF NOT EXISTS users` succeeds silently against the table the
kernel created at install and was then taken as owning it. A protected list now
sits underneath, checked before the allowlist and refusing whatever the plugin
declares: the credential and authorization tables, the tokens of every kind,
passkeys, the audit trail that would record tampering with them, the tables
deciding which plugins run and which migrations have been applied, the
form-state cache, the tenant grant, and `oauth_client` and `webhook`, which
hold secrets of their own.

`site_config` is on the list too, and that one changes behaviour. The table
holds the SMTP password: the configuration form writes whatever the
administrator types into the `smtp_password` key, in the clear, unless they
used the `env:` indirection. A single `SELECT value FROM site_config` was
therefore a credential read available to any plugin holding `raw_sql`.
`trovato_ai` is the one shipped plugin that reads the table and is refused now;
it already handled the error by logging it and returning no field rules, and
the query had in any case been failing on its own since it was written, because
it filters on a column named `name` and the column is `key`.

Raw SQL was judged by its first keyword, which cannot see two things. A
statement can be a read at the front and a write inside, so
`WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x` begins with `WITH` and
passed the read-only check while PostgreSQL ran the delete. And the first
keyword is only the first keyword if you skip comments the way the server does:
PostgreSQL nests block comments, the scanner stopped at the first `*/`, so in
`/* /* */ SELECT 1 */ DROP TABLE t` it read `SELECT` where the server read
`DROP TABLE`. Statements are parsed with the PostgreSQL dialect now and judged
as a tree. `query-raw` takes a read and nothing else; `execute-raw` takes one
INSERT, UPDATE or DELETE and nothing else, which is narrower than the old rule
of "anything whose first word is not DDL" by `SET`, `RESET`, `DO`, `CALL`,
`COPY`, `LOCK`, `COMMENT`, transaction control, `VACUUM`, `REFRESH` and
`DISCARD`. A statement that does not parse is refused rather than handed to the
server to interpret.

`SET` mattered more than the rest of that list. The host runs an `execute-raw`
statement inside `BEGIN` and `COMMIT` on a pooled connection, and a plain `SET`
is not scoped to the transaction, so one call changed the session every later
kernel query on that connection ran under. `SELECT set_config(..., false)` did
the same thing through the read path, so the function denylist closes that one
and, with it, the session-level advisory locks, `dblink`, the large-object
functions, the server-file readers, the backend and log controls, and the
`*_to_xml` family, which takes a table or a query by name and would walk around
the relation check. `pg_sleep` stays allowed.

Both raw paths also walk the relations the statement names and refuse a
protected table anywhere in the tree, including inside a CTE and inside a
scalar subquery, which is where a table name hides best.

The control that does not depend on any of this being right is the transaction.
`query-raw` runs in one the server opens `READ ONLY`, from its first statement
rather than after some first statement has already run, so a write the parser
somehow blessed is refused by PostgreSQL. `do_insert` still needs a read-write
transaction for its `RETURNING *`, so the access mode is a parameter of the
shared row-fetching path rather than a property of it.

Every raw statement the shipped plugins and the two test fixtures send is
pinned in a test, copied verbatim, and accepted by the function that sends it;
no plugin source changed. Raw SQL is still a declared trust grant and a plugin
holding `raw_sql` can still read any table this floor does not protect. What
closed are the paths that ran past that grant. A per-plugin database role, with
its own grants, remains the stronger answer and stays a post-1.0 option.

Ten read paths that returned item data now decide what the caller may see
through `ItemService::check_access` and the field-access seam, the way the REST,
SSR, gather and search paths already do. Each of them loaded items with
`Item`/`ItemService::load` or with raw SQL of its own and never passed the
result through either seam, so its visibility rules were whatever that one
query happened to say, and they had drifted from the rest of the kernel.

The revision list at `/item/{id}/revisions` asked for a login and nothing else,
so the titles and log messages of every draft on the site were readable by
anyone with an account; it now answers to `view` and then `edit`, the bar the
revert button on the same page already applied. The RecordReference autocomplete
endpoint took no session at all and matched on `status = 1`, which is not a
stage filter, so an anonymous caller could enumerate the titles of published
items on internal stages a prefix at a time. The item page rendered the title of
every item a reference field pointed at and of every item pointing back, and the
edit form prefilled a reference target's title, all from unchecked loads: the
viewer had passed `check_access` for the page's own item, which says nothing
about its neighbours. Posting a comment loaded the parent item only to notify
its author, so `post comments` was also a way to confirm that a draft exists,
and reading a comment by id checked the parent item but never the comment's own
status, so a held, unpublished or spam comment — which the listing endpoint
filters out — could be read one id at a time. The MCP `list_items` tool forced a
published default and returned the page unfiltered, so it handed over items that
`get_item`, in the same file and through the proper seam, reported as missing.
The four content-translation screens required `translate content` and nothing
more, and put the item with every field into the template context. The AI chat's
RAG context and the static Pagefind index each went from SQL straight to
formatted text, the first into a model prompt and the second into a file any
visitor downloads, with no decision at either tier. And an AI assistant
conversation could be opened on any item whose type the scope listed, viewable
or not, after which the scope plugin was asked for context about it.

Nothing about the access model itself changed: no new policy, no change to the
published fast path, and the field-access default stays fail-open. A denied item
is still indistinguishable from a missing one wherever the surface already
answered 404 for missing, and every response shape and status code a permitted
caller sees is what it was.

Two supporting changes come with it. `CronService` now holds the item service,
wired from `AppState`, because the Pagefind export has to be filtered as the
anonymous visitor who downloads the index — with the anonymous role's real
permissions, not an empty set — and a cron service built without one skips the
rebuild rather than publishing an unfiltered index. The export's "which items
and which text" step is a function of its own so it can be tested without a
Pagefind CLI on the path.

Holding the revision list to `view` and then `edit` turned up a second defect
behind it, and that one is fixed here too: `Item::get_revisions` and
`Item::get_revision` each selected eight named columns, and `ItemRevision` grew
`change_summary` and `ai_generated` two migrations later without either list
being updated. `FromRow` wants a column per field, so both queries failed at
runtime for every caller, which is why `/item/{id}/revisions` answered 500 to
everybody and the revert button behind it could not work. Both lists now name
every column the struct declares.

Adding a field to a content type now refuses a machine name the type already
has, and a database that already holds duplicates is repaired on migration.

`ContentTypeRegistry::add_field` pushed the new definition onto
`settings->fields` without looking at what was already in the list, and neither
caller checked either: the admin form and the AJAX callback behind the same
button validated the label, the machine name's syntax and the field type, and
nothing about collision. So a type accumulated two or three definitions of one
field, one per submission. It is not a cosmetic duplicate — at three copies of
`search_test_field` on `page`, the content translation form rendered none of the
type's fields at all. The check now lives in the registry rather than at either
route, so every path that adds a field inherits it, and both routes turn the
typed refusal into a message that names the machine name that collided.

Two defects in the same function are fixed with it. `add_field` built
`{"fields": ...}` and wrote it over the whole `settings` column while
`persist_fields`, three lines below and used by field edit and field delete,
merges into the existing object for the stated reason that non-field keys have
to survive; so adding any field through the admin silently dropped
`title_label`, `published_default` and `revision_default` from the type. It now
persists through that same merge. And an unrecognised `field_type` string fell
through a `match` to `Text`, so a typo or a stale option in the template stored
a field of the wrong type without a word; it is now refused and named.

The `20261006000001_dedupe_item_type_fields` migration repairs rows already
written, keeping the first definition of each name and dropping later copies.
The first is the one kept because it is the one the type has been running with.
No item data is touched: `item.fields` and `item_revision.fields` are JSONB
objects keyed by field name, so one key holds one value however many times the
type declared the field, and a redundant definition cannot own a value of its
own. The migration is idempotent, and only rewrites rows that actually hold a
duplicate.

The manage-fields template rendered neither the `errors` nor the `values` the
route had been passing it since before this change, so every validation refusal
on that screen — an empty label, a malformed machine name, a missing field type
— came back as an unchanged page with no message and the typed values gone. It
now shows the errors and keeps the submitted values.

A plugin's item write is now the write a person makes. `save-item` and
`delete-item` called the `Item` model directly, a deliberate shortcut to keep a
tap from dispatching taps, and it skipped everything the item service does
around a write: the presave, insert, update, index and delete taps, the field
and publish write gates, file reference sync and the item cache, so a page kept
serving the old title for up to `CACHE_TTL_ITEMS` after a plugin changed it.
Both functions now call `ItemService::create`, `update` and `delete`; the access
decision and the gates are the service's, and the host keeps only the create
permission check the item routes also make before calling the service.

Re-entrancy is bounded by a task-local stack of the plugins whose write tap is
running: a write made from inside one fires its taps on every other plugin, and
a write nested four deep is refused with the new `ERR_ITEM_WRITE_DEPTH` (-63). A
background call from a plugin holding `item_background` goes through the service
as the background principal, which now passes `check_access` beside the
administrator, the way it already passed both write gates. `delete-item` on an
id that names no item returns the new `ERR_ITEM_NOT_FOUND` (-62) instead of 0,
`save-item` takes `language` on a create, validated against the site's
languages, and the separate embed enqueue for plugin saves is gone because the
service's own indexing now covers them.

The SDK binds all four `item-api` functions as `trovato_sdk::host::get_item`,
`save_item`, `delete_item` and `query_items`, and `host_errors` documents every
code they return. The plugin documents now describe `tap_item_insert`,
`tap_item_update` and `tap_item_delete` as notifications whose output is not
read, and `tap_item_presave` as the tap that changes a write (BL-88, documents
only).

Contract: the WIT signatures are unchanged and the SDK bindings and two error
codes are additive. What a plugin observes from `save-item` and `delete-item`
changes (taps fire, the gates refuse, a missing delete is -62), which is a
change to an existing host function's behaviour and so frozen surface under the
version map's rules, taken in a minor release while the project is 0.x.

`/api/openapi.json` now describes the routes the server serves, plugin routes
included. It was a hand-written list rebuilt on every request, so no plugin
route ever entered it, and three of its paths (`/api/v1/items`,
`/api/v1/items/{id}`, `/api/v1/categories`) were never routed at all. The
document is built once at startup from the kernel's route metadata and every
enabled plugin's `api` menu entries (method, path, title and required
permission), read through the same rule the plugin router mounts from; the
three paths are now the routed `/api/items`, `/api/item/{id}` and
`/api/categories`, and a test requests every listed path and fails on a 404 or
405.

A plugin built with Rust after 1.96 runs. The kernel's WASI stubs did not
include `clock_time_get`, which those builds import, and loading never noticed
because the import check skipped WASI and instantiation happens per dispatch,
so every tap failed with `unknown import` while the loader reported success.
`clock_time_get` now returns real time for the realtime and monotonic clocks,
and loading pre-instantiates the module against its linker, so any import the
kernel does not provide, WASI or not, fails that plugin's load with a message
naming it (BL-118).

Contract: additive. The WASI stub is a new import the kernel satisfies, and the
OpenAPI document is a kernel route, not plugin contract. A plugin that loaded
before with an unresolvable import now fails to load instead of failing every
dispatch; it could never have run.

An editor who may edit an item, administrator or not, can now read its whole
history: the revision history page names the author of every revision, previews
any past revision at `/item/{id}/revisions/{revision}`, rendered by the same field
renderer as the item page, and compares any two at
`/item/{id}/revisions/compare?from=...&to=...`, field by field, with changed text
marked word by word and unchanged fields counted rather than listed. The compare
is computed from the two revisions' `fields` snapshots: `item_revision.change_summary`
and `ai_generated` are written by nothing, so there was no stored summary to show,
and their doc comments now say so. Revert, which no test had ever posted as a
non-administrator, now has one.

Every revision route answers to the same rule, through one `ItemService` method:
the item must be viewable (otherwise it is not found) and then editable
(otherwise the request is refused), every revision id is checked against the item
in the path with one query scoped to it, and every revision's fields are cut to
the ones the viewer may view, so a hidden field appears in neither the preview nor
the compare, not even as removed. The history, preview and compare each make a
fixed number of queries, the authors' names included, whatever the number of
revisions or authors.

The history page's 500 for any item with a revision, which Ritrovo met on 0.103.0
and 0.104.0, was fixed earlier by `c137f2b`: both revision queries selected eight
columns while `ItemRevision` derives `sqlx::FromRow` over ten, and `#[serde(default)]`
does nothing for `FromRow`, so every non-empty result failed to decode. Its entry
in `KNOWN-ISSUES.md` is gone and BL-91 is marked fixed.

The View, Edit and Revisions tabs on the admin edit forms led nowhere: the tab
macro piped each path through `escape` under Tera's autoescape, so `/` was
escaped twice and every tab's `href` was the literal text `&#x2F;item&#x2F;...`.
The macro now leaves escaping to autoescape, and the Revisions tab takes an
editor to the history.

A design note, `docs/design/forms-pipeline.md`, fixes the shape of one kernel
forms pipeline before the code moves. The form taps were unreachable because no
route ever called `FormService::build` or `process`, and content forms came from
two separate renderers that each wrote HTML of their own, so a plugin's
alteration, a field's cardinality and the save that kept unshown fields had no
single place to live.

A save from either item form erased every field the form had not rendered:
both routes built the stored field map from the POST body alone, and
`Item::update` replaced the whole map, so a plugin written key, a key the type
does not declare and the values of a multi value field were lost by one save,
while a checked Boolean was stored as the string `"1"`. One submission
builder, `content::submission::build_item_fields`, now starts from the stored
fields and overwrites only the fields the form's server side state says it
rendered, coercing each by its field type: Booleans to JSON `true` and `false`,
numbers to numbers, multi value fields to arrays counted against their
cardinality, long text to its value and a permitted format.

`extract_content_fields` and the field loop in `ItemSubmission::from_form` are
gone. Each item form now saves its state under the `_form_build_id` it prints,
and a submission without a live state for the same user is refused as expired.
A migration rewrites Boolean strings already stored in items, so gather
filters on a Boolean match items saved from the admin form; revisions are
immutable and keep theirs.

No route called `FormService::build` or `process`, so `tap_form_alter`,
`tap_form_validate` and `tap_form_submit` never fired for the item, admin
content or profile forms, each of which was hand written HTML with its own
widget mapping; and `build` gave every plugin the original form and kept the
last answer, so two alterations could not compose. `FormBuilder` now emits a
`Form`, every one of those forms goes through `FormService::build` (chained
alters, form state) and `process` (CSRF, `tap_form_validate` with field
errors) and fires `tap_form_submit` once after a save, and one renderer draws
them with the scripts each widget needs attached as asset libraries.

The `/item/{id}/edit` form now carries the block editor for a Blocks field and
renders every value of a multi value field plus one empty slot, inside the site
theme without inline styles. AI Assist buttons arrive through
`tap_form_alter` with their script. The field list's "Add field" button links
to the form on the page instead of the POST-only route (B36).

Form AJAX worked for administrators only, and not even for them from a
browser: `/system/ajax` required `administer site`, dispatched
`tap_form_ajax` without services, and the client sent no CSRF header while
the route required one, so "Add another" and every plugin trigger failed. The
route now takes a rotating per form token in `X-CSRF-Token`, recalls the form
state for the caller and re-checks that they may open the form, runs
`tap_form_ajax` with the host API, and builds "Add another" on the server for
any editor.

Fields can now carry `settings.visible_when: {field, equals}`. The form hides such a
field while the condition is false (`static/js/form-states.js` keeps it
current), and a save neither stores its posted value nor requires it. The
record reference autocomplete is attached to the item form on every route.

A plugin rendering HTML a user wrote could not filter it: `FilterPipeline` was
reachable from kernel code only, so a plugin either escaped everything or
returned the user's HTML as written, and the kernel does not filter what a
plugin returns. A new host interface, `text-format`, exposes `filter-text`
(SDK `host::filter_text`), which runs the same pipeline the kernel renders a
field with, for a format the acting user may use by the rule the item forms
apply, and errors (`-70` not permitted, `-71` unknown, `-72` too large) rather
than return unfiltered text. The plugin API version is now 0.105.

The admin sidebar listed every administration screen to anyone admitted to
`/admin`, so a delegated role (say, `edit any content` with `access
administration pages`) saw a column of links that answered 403: the list was
hand written in `page--admin.html`, and `render_admin_template` never knew who
was looking. It now takes the viewer, and the sidebar is built from one table
in `routes::admin_nav` naming each screen and the permission its page asks
for, so the viewer sees only what they may open (BL-117).

Rate limits can differ by role, each request is counted once per budget, and a
person can issue API tokens from their own account.

Every rate-limit bucket had one count for everyone, so a brief like "60 API
requests a minute anonymous, 300 signed in" could not be written down. A bucket
can now give the holders of a role their own count, configured as
`rate_limit.<bucket>.<role>` in the site configuration or
`TROVATO_RATE_LIMIT_<BUCKET>__<ROLE>` in the environment (the environment
winning, a bad value ignored with a warning, as for the bucket defaults). The
role key is the role's name lower-cased with underscores, so `anonymous user` is
`anonymous_user`. A request with no credential is counted per client IP at the
anonymous count; a signed-in request, by cookie or token, is counted per user at
the most generous count among the user's roles, falling back to the bucket
default, with no exemption for administrators. Before authentication, a request
that presents a credential is also counted per IP at the bucket's highest count,
in a budget of its own, so a stream of bearer values that authenticate nobody is
still bounded per address. One function, `RateLimitConfig::count_for`, chooses
every count, and the user's roles come from the permission service's cache, so
the limit costs no query per request.

Login and registration spent their budget twice: the per-IP middleware counted
`POST /user/login*` and `/user/register*`, and the handlers counted the same key
again, so `register: 3 an hour` admitted one registration and `login: 5 a minute`
two or three attempts. The handler counts are gone. The handler checks the
middleware does not duplicate (email verification, account recovery, password
reset mail, profile and password changes, passkey sign-in, account deletion,
data export) stay.

`/user/tokens`, linked from the account page, lists a user's API tokens (name,
created, last used, expiry), creates one with a name and an optional expiry and
shows the raw token once, and revokes one. It calls the existing `/api/tokens`
endpoints with the page's CSRF token; neither they nor `ApiToken` changed.

The admin item edit form (`GET /admin/content/{id}/edit`) now asks
`ItemService::check_access(item, "edit")` before it renders, so a plugin's Deny
on edit refuses the form with a 403 as well as the submit; the submit's item
level refusal is now a 403 too, where it was a 500.

Contract: additive. The configuration keys, the token page and the 403s are new;
no host function, WIT type, SDK type or existing endpoint changed.
`RateLimitConfig` gains a `by_role` field, and `RateLimiter` a `check_as` method
beside `check`.

Editors read from their stage; everyone else reads Live; a gather's total
counts only what its caller can see.

Which stages a read drew on was decided in three places, and none asked a
permission. `/admin/stage/switch` stored any string as the session's active
stage, unchecked; search and the alias fallback read that session value for
whoever held it; and the gather routes took `?stage=` from anyone, anonymous
visitors included, whose `total` then counted the published items on an
internal stage. One resolver, `routes::helpers::read_stages`, now answers every
read: Live, or the requested stage (a `?stage=` parameter or the session's
active stage) with Live beneath it for a holder of the new kernel permission
`use stages`. It is granted to no role; administrators hold it implicitly.
Without it a requested stage is ignored rather than refused, and the permission
is asked on every read, so revoking it takes effect on the next request. The
switch keeps its CSRF check, requires the permission (403 otherwise), and
answers 400 for a value that is not the id of a stage; reading the current
stage requires the permission too.

A gather's `total` was a SQL count that, for any signed-in viewer, included
rows the access pass then dropped, and its pages began at raw offsets, so a
page could repeat or skip visible rows. An Item gather now counts the
candidates the viewer can see, in one query up to the existing scan cap and
with the same `check_access` the page uses, and starts each page at the visible
row that opens it. The Pagefind index is documented as what it already was:
Live only.

Contract: additive for the permission. Frozen for the observable changes to
existing behaviour: `/admin/stage/switch` and `/admin/stage/current` answer a
non-administrator holder of `use stages` (an administrator still may use them)
and an unknown stage is a 400 where it was stored; `?stage=` without the
permission is ignored; gather totals change for signed-in viewers. No
host function, WIT type or SDK type changed. See
`docs/design/stage-workflow.md` §8.

Items move between stages by the editorial workflow, and new content lands on
the stage marked default.

Nothing moved one item from one stage to another: the forms carried no stage,
the bulk actions set only `status`, `save-item` ignored a stage, and the
tutorial's `workflow.editorial` variable was read by nothing. The kernel now
reads it, as a typed `Workflow` (`models/workflow.rs`) cached beside the stages
in `ItemService`. A user moves an item from stage S to stage T only if they may
edit the item and a transition names exactly S to T with a permission they
hold; no transition is implied by another, an administrator may move any item,
and with no workflow configured only an administrator moves items.
`UpdateItem` gains `stage_id`, and `ItemService::update` and `create` apply the
rule (a create on a stage other than the default is a move from the default).
The admin and front-end edit forms offer a "Move to stage" selector listing
only the stages the user may reach, the content list has a "Move selected to
stage" bulk action that decides each item, and `save-item` accepts a `stage`
machine name (an unknown one is `ERR_PARAM_DESERIALIZE`, a refused move
`ERR_ITEM_ACCESS_DENIED`). A move takes the item's URL alias and menu links with
it, through the same `move_dependents` the stage publish uses. `config import`
refuses a workflow transition that names a stage or a permission it cannot
find, with the same permission rule role files use.

`/admin/structure/stages` said the default stage is "where new content lands",
and nothing read the flag: creation bound Live. `Item::create` now resolves a
missing stage to the default stage in the INSERT itself, Live when none is
marked, so every caller (forms, JSON, `save-item`, MCP) gets it.

The content list's bulk form could not be submitted from a browser: each row's
Delete button sat in its own `<form>` inside `#bulk-form`, so the browser closed
the bulk form after the first row and sent `_token` twice, and every bulk action
(publish, unpublish, delete, and the new move) answered 400 "The form could not
be read." The Delete buttons now submit forms rendered after the table through
the `form` attribute.

`ItemService::check_access` treated an item whose stage row could not be found
as visible; it now denies, and logs.

Contract: additive for `UpdateItem::stage_id`, the selectors, the bulk action
and the import check. Frozen for two observable changes to existing behaviour:
`save-item` honours a `stage` field it used to ignore, and a create with no
stage lands on the default stage rather than Live (the same thing on a site
that never marked another stage default). No WIT or SDK type changed. See
`docs/design/stage-workflow.md` and `UPGRADING.md`.

**Behavior change.** A file in a File field is now served to every viewer of a
published item that references it, kept on every write path, and uploads up to
the documented 10 MB work. A File field stores the bare `file_managed` id, but
the reference index that `/files/{path}` consults was built only from storage
paths found in field text, so a file attached through a File field had no
`file_reference` row and was served as an orphan, to its uploader and
administrators only. It was also promoted out of `Temporary` only by the two
admin content form routes; the item API, plugin `save-item` and `config import`
left it to the six hour temporary file cleanup, which deleted it.

One seam, `content::file_refs`, now reads an item's references (File field ids
from the type's field definitions, plus the existing `local://` and `/files/`
scan) and, inside the item's own write transaction, replaces its
`file_reference` rows and promotes every referenced temporary file, in three
statements whatever the number of files. `ItemService` create, update and
revision revert, and `config import`, all run it; the admin form's own
promotion is gone. A write that adds a reference to a file the acting user may
not be served (someone else's unattached upload, or a file held only by items
they cannot view) is refused with 403, decided in one query for the whole set;
config import, operator trusted, is exempt. A migration backfills references
for File field values already stored and promotes the files they name.

Item templates get `referenced_files` beside `referenced_items`: for each File
field, `{id, url, filename, mime, alt}` per file the viewer may fetch, resolved
in one query.

Uploads up to 10 MB work: the upload routes had no body limit of their own, so
axum's 2 MB default refused anything larger with a generic 400. They now accept
`MAX_FILE_SIZE` plus multipart framing, and a larger file is a 413 that says
it is too large. Upload refuses a filename whose extension does not map to the
validated type (a PNG named `x.svg`), using one extension table that serving
shares. `/files/{path}` sends the stored, validated `filemime` instead of a
guess from the path, and the guess table, used only for paths with no
`file_managed` row, no longer maps `.svg` to `image/svg+xml`. The unused
`"s3"` arm of the storage scheme match and `LocalFileStorage::generate_uri`,
which built URIs from 8 uuid characters, are removed.

A config set can ship files. The only way to create a `file_managed` row was
`POST /file/upload` with a session and a CSRF header, so a set could name a
file by id in an item's File field but never carry the file itself.

`file.<uuid>.yml` is a new config entity (`filename`, `filemime`, optional
`alt_text`, and `path`, the bytes' location relative to the config set
directory, by convention `files/<filename>`). Import reads and validates the
bytes in its validation pass, through `FileService::validate_file`, the same
size, allowlist, magic byte and extension checks an upload gets, so a bad file
fails the whole set before anything is written; a path that is absolute, uses
`..` or leads out of the directory through a symlink is refused the same way.
The file is stored `Permanent` under its declared id through
`FileService::store`, the one write path upload now uses too; re-import keeps
the id and storage URI and rewrites the bytes only when they changed. Files
import before items, so the item's reference is indexed on the same run.
Export writes every permanent file's YAML and bytes. Files are not staged.

SMTP is configured in the environment and nowhere else, and the site settings
page no longer stores an SMTP password. **Behaviour change:** `/admin/config/site`
has no SMTP inputs any more, a POST that carries them stores nothing, and
migration `20261121000001_remove_site_config_smtp` deletes the six `smtp_*` keys
from `site_config`.

The root cause was a form with no reader. The page wrote `smtp_host`,
`smtp_port`, `smtp_username`, `smtp_password`, `smtp_encryption` and
`smtp_from` into `site_config`, the password in the clear unless it began with
`env:`, but the one `EmailService` has only ever been built from `SMTP_*` in the
environment. So the values an administrator typed changed nothing about how
mail was sent, and every database where one was typed held a credential in
clear text, in backups and in every config export (which dumps `site_config`
whole). The fieldset is replaced by a read only summary of what the kernel is
actually using: host, port, encryption, from address, and whether a username
and a password are set, never their values. "Send test email" works as before.
See `UPGRADING.md` for recovering the stored values before the migration deletes
them.

A plugin's `send-to-site-contacts` now sends from `tap_cron`, `tap_queue_worker`
and `tap_install`. **Behaviour change for plugin authors:** an existing host
function goes from always failing in the background to sending, bounded by the
per-plugin `mail` bucket that already applied on those paths (BL-72,
G-MAIL-UNAVAILABLE-IN-BACKGROUND).

The root cause was that background dispatch was built without the email
service. `RequestServices::for_background` leaves it unset, only the request
template attached it, and neither `CronService::background_services` (used for
`tap_cron`) nor the queue worker's own builder nor the `tap_install` dispatch
called `with_email`, although `CronService` already held the very same
`EmailService` for its `email:send` queue. So on a site with SMTP configured,
every plugin mail from cron or a queue worker returned `ERR_MAIL_NOT_CONFIGURED`
and logged "no SMTP host configured". `CronService` now keeps the service it is
given and attaches it in `background_services`, which the queue worker now uses
too instead of assembling the same services itself, and `tap_install` attaches
it beside the rate limiter. It is the one shared service, with the kernel's
SMTP transport, `from` address and circuit breaker.

A plugin can look up public facts about the site's users by id: `user-api`
gains `load-users` (SDK: `host::load_users`), gated by a new manifest capability,
`user_directory`. **API change:** a new host function in an existing interface
and a new `[capabilities]` boolean, defaulting to false; no existing signature
changed (BL-105, G-NO-USER-DIRECTORY).

The root cause was that `user-api` could only answer about the user a request
was for, so a plugin mailing a digest to its subscribers, or listing who did
what, had no way to learn a name. The only way round it was the `users` table
itself, which is now on the protected floor and stays there. `load-users` takes
up to 100 ids and answers, in one query that selects only these columns, each
existing user's id, name, status (`"active"` or `"blocked"`), timezone,
language and creation time; never an address, a password hash or `data`.
Declaring `user-api` alone does not grant it: the plugin must also declare
`user_directory = true`, so reading about people it is not serving is visible
in its manifest. New error codes `ERR_USER_DIRECTORY_DENIED` (-73),
`ERR_USER_IDS_INVALID` (-74) and `ERR_USER_IDS_TOO_MANY` (-75).

A plugin can mail one of the site's members by user id: `mail` gains
`send-to-user` (SDK: `host::mail_send_to_user`), gated by a new manifest
capability, `mail_users`. **API change:** a new host function in an existing
interface and a new `[capabilities]` boolean, defaulting to false; no existing
signature changed. **Behaviour change:** a new table, `user_mail_preference`,
is created by migration and is on the protected table floor, and every member
mail carries an unsubscribe link (BL-94).

The root cause was that `mail` only ever reached the site contact address, so
a plugin with something to tell a member (a digest, a reply notice) had no
way to do it short of reading addresses out of `users`, which the protected
floor forbids. `send-to-user` resolves the address in the kernel and never
hands it back: not in a return value, an error or a log line. Each message
goes through the one `EmailService` builder with a footer and
`List-Unsubscribe` / `List-Unsubscribe-Post: List-Unsubscribe=One-Click`
headers. Opening the link shows a confirmation page that changes nothing, so
link scanners cannot unsubscribe anyone; the POST records the opt out (it has
no CSRF token, by design: the unguessable token in its path is the
credential, and a mail client's one-click POST has no session). A member sees
the plugins they opted out of on their account page, with a CSRF-protected
Resubscribe button. Two new rate limit buckets apply: `mail_users`, 1000 an
hour per plugin, and `mail_recipient`, 10 an hour per plugin and member. New
error codes `ERR_MAIL_USERS_DENIED` (-56), `ERR_MAIL_RECIPIENT_UNAVAILABLE`
(-57) and `ERR_MAIL_OPTED_OUT` (-58).

Password reset by email is a flow a person can finish in a browser:
`/user/password-reset` is a page asking for an email address, and the link the
mail carries opens a set-password page that, on success, signs out every
device and lands on the login page with "Your password has been reset". The
login page's "Forgot password?" link points at it. **Behaviour change:** `GET`
on the reset link answers HTML unless the request sends `Accept:
application/json`; `POST` with a JSON body keeps its JSON answers (BL-07).

The root cause was that both steps were JSON endpoints and nothing rendered
them, so the link in the mail returned JSON to whoever clicked it. The pages
are plain forms with CSRF tokens and no inline script or style, so they work
under the enforcing CSP; the set-password page is sent with
`Referrer-Policy: no-referrer` because the token is in its URL. The request
page keeps the existing limits, the spawned send and the identical answer for
every address. Two defects found on the way are fixed: the token was marked
used only after the password changed, so two concurrent posts of one token
both succeeded (it is now spent first, in one `UPDATE ... RETURNING`); and the
reset mail was built by an untemplated sender while a `password_reset` email
template sat unused (the mail now uses the template, and the untemplated
sender is removed). A third: the security headers middleware overwrote every
response's `Referrer-Policy` with the site default, so a route's
`no-referrer` never reached the browser. It now keeps a route's
`no-referrer`, the one stricter value; this also makes the unsubscribe page's
`no-referrer` (BL-94) take effect.

Search ranking and the AI search stages are site configuration rather than a JSON literal in `templates/search.html`: one settings model, stored under the `site_config` key `search`, edited at `/admin/config/search` (permission `administer site`) and read by both search backends. Its defaults are the old literal's values, so a site that never opens the screen ranks as before. New signals: per type boosts, applied in the browser and as a multiplier on `ts_rank` in server search, and priority pages (keyword to path, one shared boost, default 10) in the browser.

**Behavior change:** `/api/v1/search/expand`, `/summarize` and `/followup` now answer 404 when their stage is switched off in the settings, before any provider call; switching a stage off used to hide it from the stock page only. `/followup` answers 400 to a conversation holding more than `max_followups` (default 3) follow-up questions.

**Behavior change:** the Pagefind index now indexes titles (the title was outside the indexed region and not findable on its own) and gives each configured search field its own weight from its `search_field_config` letter (A 10, B 4, C 2, D 1). Browser ranking changes once the index is rebuilt (BL-126, BL-127).

Search analytics: what visitors search for is counted per UTC day, with the query normalized (lowercased, email addresses and numbers of six or more digits replaced) and nothing that identifies a visitor stored, no IP address, account, session, browser or time finer than the day. Counts are kept in memory and flushed every 30 seconds, so recording adds no database work to a search. Server searches are counted on their first page; the browser search page reports its own searches through `navigator.sendBeacon` to `POST /api/search/log`. The expansion cache's hits and misses are counted too. Rows older than the retention (default 90 days) are deleted by cron. The report at `/admin/reports/search` (permission `administer site`) shows totals, the zero result share, the top queries, the top queries that found nothing and the expansion hit rate for 7, 30 or 90 days (BL-111).

**Behavior change:** analytics is on by default on new and existing sites. Switch it off with "Record search analytics" on `/admin/config/search`. The debug log line that printed a raw query on an expansion cache hit is gone.

Follow-up sentiment (roadmap 30.6): with "Classify follow-up sentiment" switched on in `/admin/config/search` (off by default; it needs analytics on), each follow-up question to the AI search assistant is classified as satisfied, confused or unmet by the site's Chat provider, after the answer has been sent, and the day's count of each label appears on `/admin/reports/search`. The classification runs in one background worker fed by a bounded queue (256); a full queue drops the job and logs how many were dropped. The text is sent to the provider and not stored. The three AI search calls and the classifier now share one send and parse function.

`/metrics` now counts the cache hits and misses and the HTTP requests it has
always advertised. `cache_hits_total` and `cache_misses_total` carry a `cache`
label, one series per kernel cache (`item`, `item_stage`, `stage_view`,
`field_access`, `user`, `permissions`, `content_type`, `gather_query`,
`gather_distinct`, `category`, `category_tags`, `menu`, `locale`,
`template_suggestion`, and `layer_l1` and `layer_l2` for the two tiers of the
cache layer), and `http_requests_total` and `http_request_duration_seconds` are
recorded for every request, labelled by method, status and the matched route
template, never the raw path.

The root cause was wiring, not counting: the series were registered with the
Prometheus registry, and so exported, but `record_cache_hit`, `record_cache_miss`
and `record_request` had no caller anywhere in the kernel, and none of the
services that own a cache held the metrics handle at all. Every one of them read
zero on a busy site. Each cache-owning service now takes the one `Metrics` from
`AppState` when it is built, and every read goes through a single helper,
`Metrics::cache_lookup`; the request series are recorded by the timing middleware
that already measured the request. The upload, rate-limit and active-connection
series are still registered and never recorded.

Every counter on `/metrics` was also exposed with its `_total` suffix twice
(`cache_hits_total_total`, `trovato_permission_load_failures_total_total`),
because the registry appends `_total` to a counter's name and the kernel
registered names that already ended in it. Counters are now registered without
the suffix, so each is exposed under the name the documentation gives it; a
dashboard or alert written against the doubled name needs the single one.

Every page and item template gets `viewer`: `{id, authenticated, roles}`, the id a string (`null` when anonymous) and the role names sorted; nothing else about the viewer reaches a template.
Role names are loaded and cached by `PermissionService` with the permissions, in one cache entry, and carried on `UserContext`; a warm cache costs no new query.
The item template gets `plugin_output`, every `tap_item_view` output on its own; `children` is unchanged, the field dump followed by that same output.
The item route hands the viewer it loaded to the new `inject_site_context_for`, so an item page loads its viewer once.
**Behaviour change:** fixed B63, an item page printing "body format: filtered_html": the field dump showed a text field's sibling `<name>_format` key as a field of its own, and now skips it when the type declares `<name>` as text.

Every config menu reaches templates as `menus_by_name` (menu name to a weight-ordered tree of `{title, path, active, children}`), filtered and localized for the viewer; `main_menu`, `footer_menu`, `main_menu_owned` and `menus` keep their shapes and values.
Plugin entries registered with a parent reach templates as `menus_by_parent` (parent path to trees), by the same permission rule as root entries; the root entries also arrive as a tree, `menus_tree`.
A `{uid}` path segment in a plugin entry becomes the signed-in viewer's id; an entry with `{uid}` for an anonymous viewer, or any other `{...}` segment, is left out, `menus` included.
`active` is set in Rust by one rule: the requested path is the link's path or under it, and `/` is active only on `/`.
Fixed B62, a `menu_link` and a `url_alias` query per menu per render: all config menus of a stage load in one query and their alias sources in one more, cached by stage and language and cleared by every menu link, alias, item, item translation, config import and stage publish write.
A `menu` tile renders its menu server side through `elements/tile--menu.html`, overridable per tile machine name; tile rendering takes one `TileRenderContext`.
**Behaviour change:** the stock `page.html` draws the main menu as nested lists (children in a dropdown on wide screens), the account area adds `menus_by_parent["/user"]` and the `user` menu, active links match pages under them, and below 768 px a menu button from the new `static/js/site-nav.js` shows and hides the nav.
**Behaviour change:** a `menu` tile renders its links instead of an empty `<nav class="tile-menu" data-menu>`.

Fixed BL-99: a `gather_query` tile renders the first rows of its gather server side through `elements/tile--gather_query.html`, overridable per tile, with a link to the gather's own page.
The tile's gather runs once, as the viewer, in the page's language, through `execute_query_only`, the path the gather's JSON and feed use; `execute_query_only` takes an optional page size.
A `gather_query` tile's config takes `items`, the number of rows from 1 to 50 (default 5); the tile form and `config import` refuse any other count, and export carries it.
**Behaviour change:** a `gather_query` tile renders its rows instead of an empty `<div class="tile-gather" data-query-id>`.
**Behaviour change:** a `gather_query` tile whose gather the viewer may not run, or that names no gather, is left off the page; the second logs a warning.

Trovato is on sqlx 0.9.1, up from 0.8.6, and the live advisory audit is clean.
Every sqlx 0.8 release capped libsqlite3-sys below 0.39, the release that fixes
RUSTSEC-2026-0459, so the lock could not take the fix even though Trovato never
compiles sqlx's SQLite driver; sqlx 0.9.1 lifted the cap, and the lock now
carries libsqlite3-sys 0.40. The same upgrade moved rsa behind an opt-in MySQL
feature, so RUSTSEC-2023-0071 left the lock too. Neither advisory was ever
reachable, and neither was ever suppressed.

`.cargo/audit.toml` has an empty ignore list. Its four remaining entries
(RUSTSEC-2023-0071, -2026-0141, -2026-0194 and -2026-0195) matched no crate in
`Cargo.lock` and are removed.

Queries behave as before. sqlx 0.9 refuses a query string that is not a
`&'static str` unless the caller marks it with `AssertSqlSafe`, so the places
that build SQL at run time (gather queries, the plugin database host functions,
plugin migrations, and a handful of model and test queries) now say so
explicitly; the SQL they send is unchanged. Trovato uses no compile-time
checked `query!` macros, so there is no offline query data to regenerate.

Three sqlx changes reach an operator's configuration rather than Trovato's
code. `PGHOSTADDR` (or `hostaddr` in `DATABASE_URL`) now follows libpq: it
skips DNS but `host` is still the name used for TLS. A password read from
`.pgpass` has its backslash escapes processed, as libpq does. And sqlx's
slow-acquire log line spells its field `acquired_after_secs`, not
`aquired_after_secs`, which matters only to anyone parsing those logs. A
deployment that sets its database through `DATABASE_URL` alone sees none of
these.

The `render_blocks` template filter now escapes a plain string instead of
passing it through, and a gather that projects the whole `fields` column now
drops the fields `tap_field_access` denies, as a `SELECT item.*` gather always
did.

The filter's output is meant to be used with `| safe`. An array of blocks went
through the block renderer, which sanitizes it, but a string value was returned
unchanged, so the filter trusted a string nothing had sanitized. Any write path
that stored a Blocks field as a string (a plugin's raw SQL, a config import, a
TextLong value rendered with this filter) could put live markup on a page. A
string is now HTML-escaped and renders as text; content that is meant to be
markup belongs to the `markdown` or `text_format` filter. Block rendering is
unchanged. The tutorial templates' comments now say what the filter guarantees.

Gather field access mapped only `fields.<name>` projections back to the item
field they read, so a projection named exactly `fields`, labelled or not, mapped
to nothing and its nested object came back whole, every denied field included,
to anyone allowed to run the gather. That projection is now filtered key by key
exactly like the star case, on every gather path (named, ad hoc, includes and
semantic search). `fields.<name>` projections are filtered as before.

API tokens work again behind the full middleware stack, leave no session
behind, and stop working the moment they are revoked. The OAuth JWT middleware
runs before the API token middleware and answered 401 to every
`Authorization: Bearer` value that was not a valid JWT, and to every bearer
value at all while OAuth was disabled, so an API token never reached the
middleware that checks it; the test harness omitted the JWT middleware, which is
why no test saw it. That middleware now judges only values shaped like a JWT,
and only while OAuth is enabled, and passes everything else through; a real JWT
is handled exactly as before. A token request no longer writes its user into the
cookie session: the token middleware gives the rest of the request an ephemeral
in-memory session holding the token's user, so no `Set-Cookie` is sent, no
session record is stored in Redis and no entry appears in the per-user session
index. Previously that write turned every token request into a site-wide
browser session that carried CSRF state, lasted a day of inactivity and kept
authenticating after the token was deleted. Token lookups no longer go through a
process-local cache, which kept a revoked token working for up to a minute, kept
an expired one working past its expiry, and could not be invalidated from the MCP
server's process or another instance; every lookup now asks the database's unique
index. A token-authenticated request is exempt from the JSON and form CSRF
checks, because no browser attaches a bearer token to a cross-site request by
itself; a cookie-authenticated request needs a CSRF token exactly as before, and
a request carrying both a session cookie and a token still acts as the cookie's
user. `ApiToken::delete_all_for_user` is new.

Changing, resetting or recovering a password, and blocking an account, now end
the account's other sessions, and a block also deletes its API tokens. A
password change only rotated the caller's own session id, and a reset by
emailed token, an account recovery and a password set by an administrator ended
nothing, so a session opened under the old password, possibly by the person the
change was meant to lock out, kept working. A block left the user's sessions and
API tokens in place. The admin edit path said cross-user invalidation needed a
user to session index that did not exist, but `SessionRegistry` has provided one
since multi-device session management landed. One kernel function,
`session_registry::end_user_sessions`, now ends a user's indexed sessions, audits
each one as revoked by the user or by an administrator, and logs rather than
fails when Redis is unreachable. A self-service password change keeps the
caller's own device, as does an administrator setting their own password. A
reset, a recovery (before its new session is established), a password an
administrator sets for someone else and a block keep none. The command line
`user reset-password` ends the sessions through the same index in Redis and
prints how many it ended, or says plainly that it could not reach Redis and
ended none. Separately, a blocked user kept a working identity on every page
built from `get_user_context` (item, gather, search, file, comment, batch and
the JSON API among them), because that helper loaded the session user without
checking that the account was active. It now deletes the session and answers
as anonymous, as `require_login` already did. An API token whose owner is not
active is refused with the same 401 an unknown token gets, decided in the token
lookup itself, so the MCP server refuses it too.

Login lockout, client address resolution and password reset requests are
harder to abuse.

The login lockout was keyed by the submitted username alone, as typed, so five
wrong guesses from anywhere locked the real owner out everywhere, while `admin`,
`Admin` and every other capitalisation each got their own five guesses because
login matches usernames case-insensitively. Every lockout key is now built from
the username trimmed and lower-cased, inside `lockout.rs` so no caller can
forget. Five failures in fifteen minutes now lock only the address they came
from (username and client address together). A second counter across all
addresses locks the account everywhere after fifty failures in fifteen minutes,
which caps guessing spread across many addresses. A successful sign-in clears
only its own address's counter. `LockoutService::is_locked`,
`record_failed_attempt`, `clear_attempts` and `get_lockout_remaining` now take
the client address; `clear_all` still clears everything for a username.

Behind a trusted proxy, the client address was the leftmost `X-Forwarded-For`
entry, which a proxy that appends passes through from the client, so a client
could choose its own rate-limit and lockout identity. The address is now the
first entry from the right that is not itself a trusted proxy; `X-Real-IP` and
then the socket peer remain the fallbacks. The session management audit events
now record that vetted address instead of reading `X-Forwarded-For` themselves.

Signing in as an unknown or blocked user skipped the Argon2 verification that a
wrong password for a real account pays for, so response time showed which
usernames exist. All three now do one production-cost verification, against a
dummy hash when there is no real one.

A password reset request created its token and sent its mail before answering,
but only for an address with an account, so response time showed which
addresses exist. It also had no limit of its own beyond the generic per-client
form limit. The lookup, the token and the mail now happen after the answer. Two
limits apply: five requests per client address per fifteen minutes (the
`recovery` bucket), and three mails per destination address per hour (a new
`reset_mail` bucket, overridable as `TROVATO_RATE_LIMIT_RESET_MAIL`). Over either
limit no mail is sent and the answer is the same success body, so the limit
itself reveals nothing.

The outbound HTTP fence now refuses an internal address however it is written.
Every IPv6 literal used to pass it: the policy took `Url::host_str()` and tried
to parse the result as an IP address, and for an IPv6 literal that string keeps
its brackets, so `[::1]` never parsed and the whole address check was skipped as
though the host were an ordinary name with no matching internal suffix. All
three layers of the fence shared the one function, so a plugin could reach a
loopback sidecar with `http://[::1]:8080/`, a redirect could land on one
mid-chain, and the AI provider's base-URL validator, which had copied the same
pattern, accepted one from an administrator form.

Hosts are classified from the parsed `Url::host()` now, as a domain, an IPv4
address or an IPv6 address, so the brackets never enter the question and the
hostname rules apply only to actual hostnames.

Behind that, the two copies of the range list have become one shared classifier
in `net_policy`. They had drifted: the plugin fence blocked carrier-grade NAT
and the broadcast address, the AI provider blocked `0.0.0.0/8`, and neither
blocked the other's, so which internal addresses were reachable depended on
which caller you were. The shared list is the union of both plus the reserved
space neither had: `192.0.0.0/24`, `198.18.0.0/15`, multicast in both families,
and `240.0.0.0/4`.

It also unwraps an IPv6 address that carries an IPv4 one inside it and
classifies the address within. `::ffff:127.0.0.1` and `::ffff:169.254.169.254`
read as ordinary global unicast to a classifier that only looks at the IPv6
bits, which made them a way to reach loopback and the cloud metadata endpoint
both as literals and through a hostname whose AAAA record answered with one.
The IPv4-compatible form, the NAT64 well-known prefix and 6to4 are unwrapped the
same way.

Nothing a legitimate caller does changes. A public IPv6 literal still works, the
AI provider still accepts the `.internal` and `.local` names an on-premises
model server is reached by, and a denial is still the same
`ERR_HTTP_INVALID_URL` the fence has always returned.

The test suite can be run twice in a row against one database, from a database
that was never migrated, and CI enforces both. Neither held before, and neither
was visible to any CI job: every job started from a database nobody had run the
suite against, and migrated it first.

Four defects were behind it, and three of them share a shape. Tests created
roles and did not remove them, about a hundred a run, which matters because
`/admin/people/permissions` renders every permission for every role on one page:
the page the permission-grid tests read under a 4 MB body cap outgrew the cap at
around five hundred roles, and the failure then arrived as a body-length error
in tests that had nothing to do with roles. Teardown that did exist sat at the
bottom of a test body, which is the line a failing assertion skips, so the runs
that left state behind were exactly the runs that had already gone wrong — one
AI budget test left a per-user override and its usage rows, against a user whose
name was fixed and whose id therefore survived into the next run, and three
files created a scratch database per test and dropped it on their last line,
leaving a migrated database on the server for every red run. And the harness
seeded the `language` table before `AppState` ran the migrations, so `createdb`
followed by `cargo test` failed at the first target that needed the shared app.

A fifth only showed up on the fourth consecutive pass, and is the clearest
argument for guarding growth rather than green runs. A test added a field to the
shared `page` content type through the admin field form, which appends to
`settings->fields` without checking for a name already there, and never took it
away; three copies of that field in, the content-translation form rendered none
of the type's fields and a test with nothing to do with search configuration
failed. Six other tests made a content type per run and left it, and every content
type shows on `/admin/structure/types` and `/admin/content/add`; four of those
six were found by the new CI guard on this branch's own first run, after both
passes had gone green.

The fixes are at those causes rather than at the symptoms. `common::defer_cleanup`
registers teardown where the state is created and runs it from `run_test`,
including while a failing assert unwinds; `common::create_test_role` is now the
one way a test makes a role and removes it again, replacing eight hand-written
copies of the same insert; `common::ensure_database_migrated` runs the
migrations before anything in the fixtures reads a table;
`trovato_test_utils::ScratchDb` drops its database in `Drop`; and
`TestApp::cleanup_content_type_on_exit` and `restore_content_type_on_exit` put
the content types back. The permission grid's 4 MB cap is unchanged, because
raising it only moves the cliff.

CI enforces the property rather than trusting it. Each shard runs its targets
twice against one database, and a step between the passes snapshots the role
count, the content type count and `page`'s field list so a step after them can
fail the job if any of the three grew. None of the five failures above was
visible on a second run; the growth was visible on every one.

One smaller thing came with it. Nine test files had `redis://127.0.0.1:6379`
written out while the rest read `REDIS_URL`, so pointing the suite at another
Redis — a second checkout, a non-default port — moved some of it and not the
others, and whatever was left behind failed in whatever way a foreign Redis
happened to produce. They all go through `trovato_test_utils::env::redis_url()`
now.

A plugin's `item-api` calls work from `tap_cron` and `tap_queue_worker` again.
Since B68.2 (`db45018`) routed plugin writes through `ItemService`, every
`save-item` and `delete-item` from either path by a plugin declaring
`item_background` answered `ERR_NO_SERVICES` (-10) and wrote nothing; the
translation writes added by B80.4 did the same. A plugin without the
capability was refused with `ERR_ITEM_BACKGROUND_DENIED` (-61) throughout, as it
should be. The root cause was that `CronService` builds the services for both
background paths in one place, `background_services`, and that builder never
bound the item service `CronService` already holds, so the `item-api` host
functions found no service to decide with and refused a call the capability
had already cleared. The request path binds it in `AppState::new`, and every
kernel test of background item writes bound it by hand, so nothing in the suite
exercised the services cron actually builds. `background_services` now binds
it, through the same once-only, weak `set_item_service` the request path uses,
and `crates/kernel/tests/background_item_api_test.rs` runs a fixture plugin,
`test_item_background`, through the real `AppState`, the real cron cycle and
the real queue drain, creating, loading, updating and deleting an item from
each. A plugin without `item_background` is still refused there, with
`ERR_ITEM_BACKGROUND_DENIED` (-61).

`tap_install`, which runs at boot in a task started before the item service
exists, now waits for it and binds it too, so a plugin's `item-api` calls from
`tap_install` are decided as on a request rather than refused. If startup fails
before the item service is built, the task claims nothing, so each pending
hook still runs on the next start that succeeds.

A dispatch path that carries no item service now answers the new
`ERR_ITEM_SERVICE_UNAVAILABLE` (-64), and the server log names the plugin and
the call, instead of the general `ERR_NO_SERVICES` with nothing logged. Every
server path carries one; the code is there so that a wiring fault like this one
says what it is.

The plugin runtime runs on wasmtime 49.0.2. Five advisories stood against the
47.0.4 it was on, and no 47.x release fixes any of them, which is why the major
version moved rather than the patch: RUSTSEC-2026-0315, where `call_ref` and
exception `catch` could drop fuel accrued by a callee and so amplify a guest's
fuel budget exponentially; RUSTSEC-2026-0316, where dynamic record lifting could
allocate past the hostcall fuel limit; RUSTSEC-2026-0325, where mis-typed
WebAssembly tag imports could corrupt the GC heap; RUSTSEC-2026-0326, where
rooting for GC values live across `try_call` could be missing, corrupting the GC
heap by another route; and RUSTSEC-2026-0327, scored 9.3, a native stack buffer
overflow from an unvalidated result count on a component async-lifted callback.

Two of the five are fuel bypasses, and fuel is the sandbox's deterministic CPU
bound, so they are the ones that matter most here even though they score lowest.
Nothing in Trovato's own code had to change for the upgrade: `Config`,
`PoolingAllocationConfig`, `Store`, `Engine`, `Linker` and `ResourceLimiter` all
kept their surface across both major versions, and the one removal that the 48
and 49 release notes never mention, `Linker::define_name`, is a method the kernel
never called. `KERNEL_API_VERSION` is unchanged and plugins already built for
`wasm32-wasip1` load exactly as before.

Nothing turns a WebAssembly proposal on or off either, so the engine still
enables what it enabled on 47 plus wide arithmetic, which 49.0.0 made a default
for every embedder rather than something this configuration asks for. No shipped
plugin uses any of it.

The fuel regression test now asserts wasmtime's out-of-fuel trap by name instead
of accepting any failure. It accepted `Failed`, which is equally what a
memory-limit breach, a failed instantiation and every ordinary guest trap
produce, so it could not distinguish the fuel bound firing from the call breaking
for some other reason, which is the single thing it exists to establish. A spin
loop whose fuel stopped being charged would run on to the wall clock ceiling and
report a different error, and the test now notices.

The daily live audit had been failing since 2026-09-29 while reporting "no
vulnerability block in its output", with the advisories plainly present in the
log it attached to the same comment. `CARGO_TERM_COLOR: always` was set on a job
whose whole purpose is to machine-parse cargo's output, so every label reached
the parser wrapped in ANSI escapes and its `^Crate:` anchors never matched. The
variable is gone from that workflow, and the parse step strips escapes before
reading, so neither a future colour setting nor a cargo-audit default can break
it the same way again.

The pinned advisory database that pull requests audit against moved to the
2026-10-06 head of `rustsec/advisory-db`. It surfaces nothing beyond the five
advisories this change fixes, so the floor rises without anything being hidden
behind it.

The administration is organised by task. Every admin page now belongs to one
of seven groups (Content, Structure, Appearance, People, Extend, Configuration,
Reports) or is the Dashboard, and that placement lives in one registry
(`routes::admin_nav`) that the sidebar, the group landing pages
(`/admin/content`, `/admin/structure`, …), the breadcrumbs, the Dashboard and
the new admin search all read. Before, the sidebar, the Dashboard cards and the
breadcrumbs each kept their own list. They had drifted apart: a page could be
in the sidebar and missing from the Dashboard, or named differently in each,
and a plugin's admin page landed wherever its URL put it. A plugin still
declares its pages as before and may name a group; one that names none goes to
Extend. No URL changed and no route's permission changed. The sidebar, the
landing pages and the search are display only: each lists only the pages the
viewer may open, and every page still checks its own permission.
`docs/admin/inventory.md` lists every admin route with its handler, template,
permission and group, and a test fails when a route is added without a row or
the row's group disagrees with the registry.

Every admin page has the same anatomy: a skip link, a banner holding the admin
search, a sidebar navigation labelled "Administration" whose groups open and
close (by keyboard too, remembered between pages), a breadcrumb ending at the
current page, and one `h1` that also leads the document title. A test checks
it on every page type. Below 48rem the sidebar sits behind a Menu button, and
no admin page scrolls sideways at 320 CSS pixels. The admin stylesheet is
rebuilt on design tokens with a light and a dark theme, uses logical properties
only, so a right-to-left language mirrors the whole layout, and draws a 3px
focus ring on everything focusable. Tests compute the contrast of every token
pair in both themes and refuse a physical property. Each person may choose a
theme (system, light, dark) and an administration language under the new Your
preferences page (`/admin/preferences`). The admin no longer loads the site
theme's `base.css`, whose body colour and link rules leaked into it and failed
contrast in dark mode. The active admin theme's own stylesheets are still
linked after the admin's.

Every word the admin shows goes through one translation function, `t`, which
handles placeholders, plurals by CLDR category and context. It is registered
whether or not the locale plugin is enabled, and it returns the English source
when there is no translation. Flash messages are stored as messages, not as
sentences, so they are translated in the language of the page that shows them,
and one that reports a failure (a bulk action that changed nothing or only
part of the selection, a plugin that could not be switched, a test email that
could not be sent) is shown as an error and announced at once, not labelled
"Done:".
A marker-mode test opens every admin page type and fails on any text,
`aria-label`, `title`, `placeholder` or `alt` that did not go through `t`.
`TROVATO_PSEUDO_LOCALES=1` adds an accented and a right-to-left pseudo
language for checking layouts. The form templates the front end shares with
the admin now use `t` as well; outside an admin page `t` returns its English
source, and a test proves those templates render byte for byte as before. On an
admin page the file widget's label now names its file chooser, which it did not
before.

A new CI job, "Admin accessibility", generates a site and runs axe (WCAG 2.0,
2.1 and 2.2, A and AA) on every page in the inventory in four modes: light,
dark, 320px wide and right to left. It also runs keyboard checks and snapshots
each page type's accessibility tree. `docs/admin/design.md`,
`docs/admin/accessibility.md` and `docs/admin/translation.md` describe the
design, the checks and how to translate the admin. An admin page now makes one
more query, to read the viewer's preferences, plus one site setting.

The Pagefind search index is now updated item by item. An edit or a new comment
is searchable at the next cron run, after an update that rewrites only that
item's part of the index. Before this change, the whole index was rebuilt.

The cause was that every change asked for a full rebuild. The item service, the
comment service and the content type screen each set
`pagefind_index_status.rebuild_requested`, and `trovato_search`'s `tap_cron`
set it too whenever `MAX(changed)` over published items moved. One comment
therefore cost an export, render and encode of every searchable page. This is
part 3 of `docs/design/incremental-search-index.md`.

On the largest local content set (1,237 searchable pages) the numbers are:

* a title edit is searchable 41 to 54 ms after the write starts, and a comment
  32 to 42 ms after;
* the cron update takes 21 to 25 ms of that and writes 5 files (the item's
  fragment, the two word chunks its words are in, the meta file and
  `pagefind-entry.json`);
* the same edit followed by a full build took 272 to 280 ms and re-encoded every
  page.

Changes are now queued in a new table, `search_index_queue`. It holds one row
per item, so the last change before cron wins, and records a reason. Writes to
items, comments and URL aliases are queued by triggers on those tables, in the
write's own transaction, whoever makes them: the item, comment and alias
services, pathauto, and a plugin's raw SQL. A write that commits is therefore
queued even if the server dies right after the commit, and a write that rolls
back queues nothing. Three other writers queue their items in code:

* the content type search switches, the search field weights and the reindex
  button, each of which queues every item of the type;
* workspace publishing, inside its own transaction, for every item it moves or
  deletes;
* plugins, for a change no trigger sees.

Plugins have one new host function, `search-index-enqueue` in the `db`
interface (`host::search_index_enqueue` in the SDK). The rule for plugin
authors, in `docs/plugin-development.md`, is this: a plugin that changes
something else that decides what an item's search page holds calls it
afterwards with the ids it affected. Typically that is data its
`tap_field_access` reads. Writes to items, comments and aliases need no call.

Scheduled publishing used to change `item.status` with raw SQL, which nothing
saw, so a scheduled unpublish left the item in search results until something
else rebuilt the index. It also kept each date after applying it, so an item
whose publish and unpublish dates had both passed was published and
unpublished again at every cron run. It now claims each due item with one
statement that removes the date only if the item is still due, so an editor's
change to the date before then is respected. It then sets the status through
the item API, so the item is queued, a revision is recorded and the write taps
fire. A scheduled unpublish therefore takes the item out of the results at the
next cron run. `trovato_search` no longer polls `MAX(changed)`. Its `tap_cron`
reports how many items are queued.

How a cron run applies the queue:

* It drains the queue in batches of 200 items.
* For each batch it exports only those items, through the same access seam as
  a full build, and tokenizes only pages whose HTML changed.
* It rewrites the word chunks their old and new words are in. The chunk ranges
  are recorded in a new table, `search_chunk`, and stay frozen until the next
  full build. A chunk past 40,000 locations splits, and one that empties is
  dropped.
* It publishes a new meta file and then the entry file. That happens under the
  same advisory lock and generation check as the full build.

The full build remains, and is taken with a `warn` log line naming the reason
when an update's preconditions fail. Examples are a live entry the database
did not record, a missing or unreadable cached term set, a new or emptied
language, or a backlog of more than 1,000 items that is more than a quarter of
the index. A full build reads a cached term set that does not decode as a miss
and tokenizes the page again, so one bad row cannot fail every run. An
operator can ask for one with the new `trovato search rebuild` command. If
that build fails, or another publisher gets in first, the request stays set
for the next cron run.

Deleted and unpublished items leave tombstones in their language's page slots,
and new items reuse them. When tombstones pass a share of a language's slots,
cron compacts the index with one full build that renumbers densely. The result
is byte for byte the index a first build of the same content writes. The share
is a new search setting, "Compact the search index above this percentage of
freed slots", at `/admin/config/search`. It defaults to 5% and takes 0.5 to 50.

A browser tab that loaded the index more than 24 hours before a search can ask
for a chunk or fragment that has since been deleted. `scolta.js` now reloads
the index once when a search fails that way, and runs the search again.

## v0.104.0 — 2026-09-24

A queue release. Eight entries, and between them they are the difference between
a queue-driven site that needs someone watching it and one that runs unattended.

Every failure here was found on a running site and every one of them ended the
same way: work stopped, and nothing said so. A cron run whose HTTP client hung up
at thirty seconds was dropped mid-await, leaving a heartbeat spinning on a closed
channel that renewed the global cron lock forever, so every later trigger
answered "another instance is running cron" and anything reporting on queue
health from `tap_cron` went quiet at the moment it was needed. A job that reached
`max_attempts` while claimed was handed back to a worker on every cycle, climbing
past its own limit, holding a slot that everything else queued behind. A plugin's
queues all ran at the width of its widest, so four stuck jobs on one queue
starved the rest. A guest parked in a host call was billed for the wait as though
it had been computing, and a single slow provider response lost the job on its
first attempt. The shape is the same each time: a bound that was never applied,
or applied to the wrong quantity, and no way to see it from outside.

So the bounds are real now, and they are observable. `max_attempts` fences the
claim rather than only the failure bookkeeping, and a reaper gives an ending to
the rows that were stranded without one. Each queue drains at the width it
declared. A drain pass, the lock renewal and a worker that burns its CPU budget
are each bounded, and the CPU budget counts guest execution rather than wall
clock, so waiting and looping are finally distinguishable. All 35 host functions
across the 13 host interfaces report entry, return and elapsed time, and keep a
live registry so a wedged call can be named while it is still wedged rather than
inferred afterwards. Every queue job emits one line saying which outcome it
reached and how long it took.

Four of these change what a running site does with work it has already claimed:
**see [UPGRADING.md](UPGRADING.md) before upgrading.**

The plugin API moves to `(0, 104)`, and nothing about the boundary changed with
it. No WIT signature moved, no tap was added and no host function was added. The
rule is the one it has always been: a manifest's major must equal the kernel's
and its minor must not exceed it, so a plugin declaring `api_version = "0.103"`
installs and runs on this kernel exactly as before, and a plugin declaring
`"0.104"` is stating what it was built against rather than reaching for something
gated behind the number.

- The project version is 0.104.0 and the plugin API is `(0, 104)`.

  The manifest count in `docs/design/version-map.md` stays at 37. That file gains
  one row for a location it did not list: the two manifest fixtures in
  `info_parser.rs` that spell out a version. Unlike the API-compatibility tests
  beside them they pass whatever version they name, so a miss there is silent.

- Add: every queue job reports what became of it and how long it took.

  `elapsed` was measured on the dispatch path and then thrown away on the
  success and ordinary-failure paths, and a succeeded row is deleted outright,
  so "how long do jobs take" could only be answered by sampling `plugin_queue`
  from outside and hoping to catch a job mid-flight. A stage that had quietly
  doubled in latency looked exactly like one that had not.

  Every outcome now emits one structured line naming the plugin, the queue, the
  row, the attempt, the elapsed milliseconds and which outcome it was:

      queue job finished plugin=argus queue=argus_decide item_id=1013
        attempt=1 elapsed_ms=1561 outcome="succeeded"

  All four outcomes are covered — `succeeded`, `retried`, `dead_lettered`, and
  the `abandoned` case the drain's ceiling produces, which carries the host call
  it was abandoned in as well.

  No timing columns were added to `plugin_queue`, and not because the migration
  would have been awkward: a succeeded row is deleted, so a duration column
  could only ever retain the failures. That is the biased half of the
  distribution, and a table that answers the question wrongly is worse than one
  that does not answer it.

- Fix: a worker's CPU budget counts what the guest executed, not what it waited for.

  The epoch deadline is wall clock. A guest parked in a host call — an AI
  request, a feed fetch — was billed for the wait exactly as though it had been
  computing, so a slow provider and a runaway loop were indistinguishable. The
  drain then compounded it: `dispatch_to_plugin` collapsed every failure into
  `None`, so the only way left to ask *why* a call failed was how long it took,
  and a call that ran roughly the budget was recorded as CPU exhaustion. That
  path dead-letters on the spot and ignores `attempts` entirely, by design —
  which is right for a worker that burns its budget and is data loss for one
  that merely waited. The comment justifying the guess claimed the two were
  "orders of magnitude apart"; at the shipped 150 seconds, against an observed
  analyze call of 12 to 17 seconds, the margin is about tenfold, and a single
  slow response was enough to lose a job on its first attempt.

  Two changes, and the guess is gone rather than retuned:

  - The `Store` now carries an epoch-deadline callback that extends the deadline
    by however long the call has spent inside host functions since it was last
    asked, so only guest execution counts the budget down. Host-call time is
    accumulated by the tracing added for the host-call registry, which was
    already measuring exactly this.
  - The dispatcher reports *why* a call failed. `ExportCallError` gained a
    `CpuExhausted` variant, set by the epoch callback as it interrupts, and the
    drain reads that instead of inferring from the clock.
    `exhausted_its_cpu_budget` is deleted; a wall-clock heuristic with a better
    threshold would still be a wall-clock heuristic.

  A job that only waits is no longer cut off at all: it finishes. A guest that
  burns its budget is still dead-lettered on the first occurrence, which is the
  behaviour that bound it in the first place.

- Fix: one queue job that never returns no longer holds the whole drain pass open.

  The epoch deadline bounds a guest that keeps *executing*. It cannot bound one
  that has stopped executing — parked in a host call that never comes back — and
  the drain awaited its in-flight batch unconditionally. A single such job
  therefore held the pass, and with it the cron lock, for as long as it liked;
  the pass's own 60-second budget bounds only how many further batches it
  *starts*, never the batch already running.

  The drain now stops waiting past a ceiling of the worker's CPU budget plus
  `JOB_ABANDON_SLACK`. The ceiling sits above the epoch deadline deliberately: a
  job the epoch mechanism is about to cut off cleanly should be cut off cleanly,
  not abandoned a moment earlier. Every job still running at the ceiling is
  handed back through the ordinary failure path, so `attempts` and
  `max_attempts` mean what they always meant and nothing is dead-lettered for
  being slow once. Each one logs an `error` naming the plugin, the queue, the
  row and the host call it was sitting in, read from the in-flight registry.

  What the bound can and cannot do, stated plainly: it recovers the *row*, not
  the *task*. Dropping the `JoinSet` aborts what it holds, and an abort lands at
  the next await point — so a job waiting on I/O stops there, while a guest
  burning CPU without yielding reaches no await point and keeps its thread until
  the epoch deadline reclaims it. The drain no longer waits for that to happen.

- Fix: a cron run no longer leaves an immortal heartbeat spinning on a closed channel.

  The Argus pipeline stopped about thirty seconds into a cron run and stayed
  stopped: a runtime worker pinned at a full core, the database pool completely
  idle, no provider call in flight, and every later trigger answered "another
  instance is running cron" until the process was restarted. The jobs were never
  the problem — re-claimed later, the same ones finished in about two seconds
  each.

  Thirty seconds is not a coincidence. It is the poker's `curl -m 30`. When a
  run outlasts it the client hangs up, and the handler awaited `CronService::run`
  inline, so the whole run was dropped mid-await. That alone left claimed queue
  rows (their tasks aborted where they stood) and a lock that `release_lock`
  never reached. The heartbeat, though, was spawned rather than contained: it
  outlived the run that owned it, and the only sender for its stop channel was a
  local in the future that had just been dropped.

  `stop_rx.changed()` reports a closed channel immediately, and every time it is
  asked thereafter. The arm was written `_ = stop_rx.changed()`, discarding that
  result; the stored value was still `false`, so it did not break, and the `loop`
  re-entered `select!` to be told the same thing again. A tight busy loop, on a
  runtime worker, still renewing the cron lock on every tick, with no run left
  that could ever stop it. Each timed-out run leaked another one.

  Both halves are fixed:

  - The heartbeat treats a closed channel as what it is. The only sender lives in
    the run that spawned it, so the channel closing means the run is gone —
    including in the one way it cannot announce, its future being dropped part
    way. There is nothing left to hold the lock for, so the task ends and the
    lock expires on its own TTL.
  - The route runs cron on a task of its own and awaits that. A dropped
    `JoinHandle` stops observing a task, it does not stop the task, so a client
    disconnect now cancels only the waiting. The run finishes, releases its lock,
    and completes its queue bookkeeping instead of being torn up in the middle of
    it.

  Found by attaching gdb to the wedged container: two independent samples caught
  the burning thread inside `run_heartbeat`'s `select!`.

- Add: every host call a plugin makes is observable, and a wedged one can be named.

  A guest suspended inside a host function is invisible to every bound the
  kernel has. The epoch deadline interrupts guest code, so a guest waiting on
  the host is never interrupted; fuel metering is off by default; and the queue
  drain's dead-letter classifier only runs once `dispatch_to_plugin` has
  returned. When a worker stopped coming back, nothing in the logs said which
  host function it had entered — the kernel knew a tap was running and nothing
  more.

  All 35 host functions across the 13 host interfaces are now wrapped. Each
  emits a paired record: `debug` on entry, `debug` on return carrying
  `elapsed_ms`, and `warn` instead of the second `debug` when the call took
  longer than `SLOW_HOST_CALL_MS` (30 seconds — about twice the slowest
  legitimate call measured, an analyze against a large model at 12 to 17
  seconds, and a fifth of the 150-second background-tap epoch budget, so a call
  heading for trouble is visible long before any deadline could fire). A host
  call that is entered and never returns leaves only its entry line, which is
  what names it in a log tail.

  Logging alone does not answer the question at the moment it is asked, though:
  a call that has not returned has produced no record to read. So the same
  wrapper maintains a live registry of in-flight calls, keyed by a new
  per-invocation id on `PluginState`. The guest inside one tap invocation is
  single-threaded and can therefore have at most one host call outstanding, so
  that key answers "which host call is this invocation sitting in right now" for
  any caller that needs it.

  Asynchronous host functions go through a `Linker` extension
  (`func_wrap_async_traced`) that wraps the returned future, so the call sites
  changed by one method name and no closure body moved. Synchronous ones cannot
  be wrapped that way — `IntoFunc` is sealed over each closure's own arity — so
  they open an RAII `HostCallGuard` as their first statement instead.

  Both paths record their exit from `Drop` rather than from a call on the way
  out, because a host call does not always reach its own end: when a cron run's
  HTTP client disconnects, the request future is dropped and every host call
  under it is torn out mid-flight. Recording that as an ordinary return would
  leave a registry entry insisting the guest is still inside a call it left long
  ago, so a dropped call is cleared and logged as cancelled, with its elapsed
  time, and only a call that reached the far side of its own `await` is reported
  as having returned.

- Fix: a cron run gives the lock back, and a job that burns its CPU budget dies.

  A queue worker's only bound is the background epoch deadline, 150 seconds. A
  worker that burns CPU therefore holds its slot for the whole budget at 100% of
  a core and is then cut off, and the kernel recorded that as an ordinary failed
  attempt and rescheduled it. The retry gets the same full budget and burns it
  against the same wall. Measured on a synthetic queue load with no AI provider
  configured: one such job held the drain for 150.2 seconds and came back `ready`
  for another go.

  Nothing bounded the drain around it. `MAX_QUEUE_ITEMS_PER_CYCLE` bounds a pass
  in items, never in time, so a pass could run for `items / width * budget`:
  over an hour at the shipped values, all of it inside a cron run holding the
  global cron lock. And `run_heartbeat` extended that lock every 60 seconds for
  as long as the run had not returned, with no condition attached, so a run that
  could not finish kept the lock looking healthy: `TTL cron:lock` sat at its
  maximum and *rose* between samples precisely because the run was stuck. Every
  later trigger answered "another instance is running cron", `tap_cron` never
  dispatched again, and anything reporting on queue health from `tap_cron` (a
  plugin's own stuck-queue alarm, say) went quiet at the moment it was needed.
  That is why such an alert fires once and never re-fires.

  Three bounds, none of them a supervisor and none of them a restart:

  - A failed dispatch that consumed its whole epoch budget is dead-lettered at
    once, whatever `attempts` says, with a reason naming the budget. The bound it
    breached is not the attempt count, and a retry only buys the same burn again.
    The boundary is one epoch tick of slack, an absolute second, not a fraction
    of the budget: the engine's epoch advances once per second, so an N-tick
    deadline can expire anywhere in `(N-1, N]` seconds, and a proportional
    tolerance reads a short budget's plain exhaustion as an ordinary failure.
  - One drain pass has a wall-clock budget (`QUEUE_DRAIN_BUDGET_SECS`, 60s,
    settable per service). It bounds how many further batches a pass starts, not
    the batch in flight, so the ceiling on a pass is the budget plus one
    dispatch. Work left over is claimable again next cycle.
  - Lock renewal stops after `MAX_LOCK_RENEWAL_SECS` (900s). The lock then
    expires on its own TTL, the next trigger proceeds, and reporting resumes. A
    stuck run is not killed; it simply stops being able to keep the whole cron
    system waiting on it. Release stays owner-checked, so a run that outlived its
    renewal cannot delete a newer run's lock.

  The background epoch budget moves onto `ResourceLimits`, settable with
  `PLUGIN_BACKGROUND_TAP_DEADLINE_SECS`. The default is unchanged at 150. It was
  a constant read at the dispatch site, which left a deployment no way to lower
  the price of one runaway worker, and left the exhaustion path untestable
  without waiting out the real budget.

- Fix: a queue's declared concurrency bounds that queue
  (`G-QUEUE-CONCURRENCY-COLLAPSED`).

  `tap_queue_info` has always returned one entry per queue, each with its own
  `concurrency`. The kernel read the **maximum** across those entries, once per
  plugin, and applied it to a claim that named no queue at all. A plugin
  declaring `analyze: 4, cluster: 1, summarize: 1` therefore ran `cluster` four
  wide, because the width it was given was `analyze`'s, and the four slots it ran
  in were shared by every queue the plugin owned. Per-queue declarations bounded
  nothing, and a plugin had no way to say that one of its queues must be
  serialized.

  That is also what set the blast radius when workers stalled: with one width and
  one undifferentiated claim, four stuck jobs on one queue occupied every slot
  the plugin had, and its other queues starved behind them rather than draining
  alongside.

  `claim_batch` now claims within a named queue, and each queue drains at its own
  declared width, clamped to `QUEUE_CONCURRENCY_CAP` as before: a declaration can
  lower the kernel ceiling, never raise it. A queue holding rows that the plugin
  never declared is not stranded by a declaration it has no say in; it drains at
  width 1.

  The per-cycle cap stays per plugin, because it is the fairness bound *between*
  plugins and giving each queue its own would let a plugin multiply its share by
  declaring more queues. A plugin's queues take turns spending it rather than
  draining one at a time in name order, so splitting the width did not buy
  cross-queue starvation in exchange.

  `CronService::resolved_queue_widths` reports the widths the drain will honor,
  per queue. There was no such answer to give before, only one number per plugin,
  so a site could not see that the width it declared for one queue was being
  applied to another.

- Fix: `max_attempts` bounds the claim, not only the failure bookkeeping.

  A queue job that reached `max_attempts` while claimed, and whose lease then
  expired, was handed straight back to a worker. `claim_batch` selected on
  `status`, `next_attempt_at` and `locked_until` alone, with no
  `attempts < max_attempts` term, and incremented `attempts` unconditionally.
  `max_attempts` was read in exactly one place, `mark_job_failed`, which runs on
  the *failure* path. A claimer that never returns never reaches that path, so
  the bound was never applied to it: the row was re-dispatched on every cycle
  forever, `attempts` climbing past its own limit while `dead_at` and
  `dead_reason` stayed null.

  The cost was not the wasted dispatches. It was the worker slot. Four such rows
  held all four slots of `QUEUE_CONCURRENCY_CAP` on every cycle they were picked
  up, so every other queue starved behind jobs that had already spent their
  attempts. Marking those four rows dead by hand drained the rest of the queue in
  a single cycle. That is what turns a worker stall into a permanent outage.

  `attempts < max_attempts` is now part of the eligibility predicate, so an
  exhausted row is never claimed again. Because nothing else would then give it
  an ending, a reaper runs at the head of every drain and retires rows that are
  claimed, past the bound, and past their lease, with a `dead_reason` that says
  what happened to them: the claim lease expired with no terminal outcome
  recorded. Both arms get it, the plugin drain and the kernel embed drain, since
  both claim through the same primitive.

  Crash recovery is unchanged. A job with attempts still on it whose claimer died
  is reclaimed exactly as before, which is the at-least-once guarantee (D-47);
  the new term fences only rows that have nothing left to spend.

## v0.103.0 — 2026-09-21

A fix series, and the permission work that turns a delegated role from something
the grid could describe into something a site can actually run on.

0.102 gave every admin screen its own permission and left the kernel holding two
notions of administrator that did not agree. Five entries here close that.
`UserContext::is_admin()` stopped being derived from the permission list, so
`administer site` is an ordinary permission and the superuser flag is the column
it always claimed to be. `tap_perm` is dispatched, so a plugin's declared
permissions exist to the kernel, can be seen in the grid and can be named in a
`role.*.yml`. The grid stopped replacing each role's whole set from the boxes it
rendered, which on one site had silently deleted 29 plugin grants. A user can be
put in a role at all, from the form or the CLI. And `/admin` itself took a new
`access administration pages`, so a role given one screen reaches the dashboard
that links to it instead of a 403 at the front door. Two of those change
behaviour on a running site: **see [UPGRADING.md](UPGRADING.md) before
upgrading.** The administration sidebar is not yet filtered to what the viewer
may open, so a delegated role still sees links that answer 403; that needs
`render_admin_template` to know the viewer, across its 80 call sites, and is
recorded as BL-117.

Content translation gained a write path. `item_translation` had been read since
the table was added, by the request overlay, the gather query builder, the
sitemap and the menu, and nothing in the kernel ever wrote a row: every one of
those readers was reading something no part of the product could produce, and
the two admin translation screens could display a translation they had no way to
create. There is now an admin form and a delete route, and a config entity so a
translated site exports and re-imports like any other. Those two admin screens
also render, rather than answering 500 on templates that were never written.

The rest is a fix series, most of it findings recorded in `docs/BACKLOG.md` by
this tree and by the projects built on it, each entry naming a root cause rather
than a symptom: a documented slow-query threshold that steered nothing because
its layer was applied to no router, twenty startup warnings the project's own
plugins emitted about callbacks that could never be called, passkey sign-in that
a browser never ran because the button's script was inline under a CSP with no
`unsafe-inline`, a sitemap whose every address was relative and therefore
unusable, listings that advertised a UUID in every link while the friendly alias
sat unused, and a markdown filter that stripped the class that says what language
a code block is. One security entry: rustls 0.23.37 to 0.23.45, clearing
RUSTSEC-2026-0285, a lockfile-only bump that had been turning every pull request
red since the advisory published.

The plugin API moves to `(0, 103)`, and nothing about the boundary changed with
it. No WIT signature moved, no tap was added and no host function was added, so a
plugin declaring `api_version = "0.102"` installs and runs on this kernel exactly
as before. The rule is the one it has always been: a manifest's major must equal
the kernel's and its minor must not exceed it, so a plugin declaring `"0.103"` is
stating what it was built against rather than reaching for something gated behind
the number. The two surface additions reach a plugin whatever it declares.
`ERR_MAIL_RATE_LIMITED` (-55) is a new value the `mail` host function can return,
distinct from a send failure because waiting fixes it. And
`current-user-has-permission` now answers the *effective* permission, the same
question the kernel's own route guards ask, so a plugin's check and the kernel's
gate no longer disagree about an administrator.

- The project version is 0.103.0 and the plugin API is `(0, 103)`.

  The manifest count in `docs/design/version-map.md` stays at 37. That file gains
  two rows for locations it did not list: `UPGRADING.md`, whose `## Unreleased`
  heading closes with the changelog's, and the worked `git tag` example in
  `docs/RELEASING.md`, which is the one stale version a reader is most likely to
  paste into a terminal.

- Add: `access administration pages`, so a delegated role can use the dashboard.
  **See [UPGRADING.md](UPGRADING.md).**

  #92 gave every admin screen its own permission and left `/admin` itself on
  `administer site`. The gap had a shape: a role granted only `administer
  comments` could reach `/admin/content/comments` by typing the address, and got
  403 on the dashboard that would have linked to it. The delegation worked
  everywhere except at the front door, which `docs/admin-permissions.md`
  recorded as a known gap.

  `/admin` now takes `access administration pages`. It is the weakest permission
  in `KERNEL_PERMISSIONS` by design: admission to the administration section,
  and no authority inside it. Every screen there still asks for its own
  permission, so the new one cannot be used to reach anything. `/system/ajax`
  keeps `administer site`, because it serves the admin forms' AJAX callbacks
  rather than admitting anyone.

  The dashboard's cards are filtered to what the viewer may open — the structure
  card on `administer site`, each "Add *type*" link on the same `create {type}
  content` string `/item/add/{type}` checks — so the page no longer offers a
  door that answers 403. The site navigation's "Admin" link asks the same
  admission question, so a role that cannot enter is not shown the way in. The
  update banner stays on `administer site`: someone admitted to moderate
  comments cannot act on an update, and whether a security release is
  outstanding is not theirs to know, so opening the section wider does not widen
  that disclosure.

  A migration grants the new permission once to every role already holding
  `administer site`, so no existing site loses its dashboard on upgrade. That is
  the only automatic grant, and it is a one-time correction rather than a rule:
  nothing makes `administer site` imply admission afterwards.

  The admin layout's sidebar still lists every screen unconditionally, as it has
  since #92 made those screens delegable. That is not introduced here and is not
  fixed here; filtering it means giving `render_admin_template` the viewer, a
  signature change across its 80 call sites.

- Fix: the kernel has one notion of administrator (BL-33).
  **Behaviour change — see [UPGRADING.md](UPGRADING.md).**

  It had two, and they did not agree. `require_admin` and `require_permission`
  read the `users.is_admin` column. `UserContext::is_admin()` read whether the
  context's permission list contained the string `administer site`, and
  `context_from_permissions` pushed that string onto a column administrator's
  list so the second notion would hold for them. Each direction of the
  disagreement was its own defect.

  **A role could be an administrator without being one.** A role granted
  `administer site` and nothing else satisfied `UserContext::is_admin()`, so it
  passed every bypass keyed on it — the item routes, item and field access,
  menu visibility, the gather access predicate, file serving, the assistant's
  scope gate — while `require_permission` still refused it on the admin screens
  those bypasses were supposed to accompany. One permission string was quietly
  the most powerful grant in the system, and the screen that granted it said
  only "administer site".

  **An administrator was refused by plugins.** Because the builder pushed the
  marker onto the permission list rather than carrying the column, and
  `current-user-has-permission` answered from that list literally, a superuser's
  plugin-side permission set was the marker and whatever their roles happened to
  grant. A plugin's own check therefore refused an administrator anything but
  the literal string `administer site`. The worked example is the AI assistant:
  the kernel let an administrator open a plugin's conversation and the plugin's
  belt refused every tool inside it. The assistant's own test suite carried a
  helper that granted the permission for real to work around exactly this.

  The root cause is that `is_admin()` was a *derived* answer. Deriving it from
  the permission list made the list a place where authority could be forged, and
  made the honest list unavailable to anyone who wanted it.

  `UserContext` now carries the `users.is_admin` column as a private field set
  by its constructors, and `is_admin()` reads it. Nothing is added to a
  permission set: an administrator's context holds their real role permissions
  and no marker, so `has_permission` answers what the roles grant. The new
  `UserContext::can` is the effective question — the column, or the permission —
  and is the one place the bypass for a context-based check lives. Every site
  that used to write `has_permission(p) || is_admin()` by hand calls it, and
  `current-user-has-permission` answers it, so a plugin's check and the kernel's
  gate now agree. The bypasses that name no permission (item and field access,
  the gather predicate, file serving) still ask `is_admin()`, which now means
  the column and only the column.

  `require_permission` and `require_admin` keep their column bypass, which is
  now the only other one in the kernel. `administer site` is an ordinary
  permission: it opens the structure and configuration screens 0.102 gated on
  it, and nothing else. The superuser flag stays non-delegable.

  Additive: no WIT signature change, no new host function, no
  `KERNEL_API_VERSION` bump and no migration. `current-user-has-permission`
  gains a doc comment in `crates/wit/kernel.wit` saying it answers effective
  permission.

- Fix: the permission grid no longer deletes what it does not display, and a
  user can be put in a role.

  Two defects seen on the same 0.102.0 site, with the external `netgrasp` plugin
  enabled.

  **The grid deleted 29 grants across every role.** All of them were plugin
  permissions, `administer netgrasp` among them, inserted by the plugin's
  migrations. The grid rendered the kernel's list and nothing else, and the save
  replaced each role's whole set from what the form returned — so every
  permission the screen had never shown arrived looking exactly like a box
  someone had deliberately unticked, and was taken away. They were restored with
  SQL, and the next unrelated checkbox would have removed them again.

  Dispatching `tap_perm` shrinks the problem because a plugin's permissions now
  appear in the grid, and it does not remove it: a disabled plugin's grants, a
  permission inserted by SQL and one left behind by a migration are all still
  invisible to this screen. So the save is scoped rather than merely better
  informed. The form states which permissions it rendered, and
  `Role::set_permissions_within` confines removals to that set; a permission the
  grid did not render keeps whatever it had, granted or not.
  `Role::set_permissions` is untouched and still replaces the whole set, because
  that is right for `config import`, where the file is the complete statement of
  what a role holds.

  **There was no way to give a user a role.** Neither the user edit form nor the
  CLI offered it, so the run used SQL to put `netadmin` into `network_admin`.
  `user_roles` has always existed and `RoleService::assign_to_user` has always
  been there; nothing called them. The user add and edit forms now carry a
  checkbox per role, gated on `administer users` like the rest of those screens,
  and the CLI gains `trovato user role-add <username> <role>`, `role-remove`,
  and `roles <username>` to see what someone holds, in the same two-word style as
  the existing `user reset-password`.

  Role membership is delegable and the permissions it can carry are not.
  `administer users` is itself a grantable permission and roles carry
  permissions, so without a guard a delegated user administrator could assign
  themselves a role holding `administer site` and become a site administrator by
  way of the screen they were given to manage usernames. A non-superuser may
  therefore grant or revoke only a role whose permissions they already hold: they
  can hand out what they have and no more. A superuser is unrestricted, and a
  role the actor may not touch is left exactly as it was on the target rather
  than silently dropped. The CLI has no such guard, because it is not reachable
  over the network and whoever runs it already has the database; it does say that
  a running server will not see the change until its permission cache expires,
  which is the limitation `config import` has always had for the same reason.

  Regression tests: a grant the grid never rendered survives a save (verified
  failing against the old replace-all semantics, where the grant came back as an
  empty set), a rendered permission left unchecked is removed, a rendered one
  left checked is kept, and ticking one box leaves an invisible grant alone. On
  the role side: the form offers the checkboxes, assigns and removes, a user
  without `administer users` can change neither someone else's roles nor their
  own, a delegate cannot grant a role beyond their own permissions but can grant
  one within them, a delegate cannot set the superuser flag, and the model calls
  the CLI verbs sit on assign, are idempotent, and remove.

- Fix: a content translation can be written, not only read.

  `item_translation` has been read since the table was added. The request
  overlay resolves an item through it, the gather query builder joins it, the
  sitemap builds alternate links from it and the menu takes translated labels
  from it. Nothing in the kernel ever put a row in. Outside the tests the only
  writer was SQL, so every one of those readers was reading something no part of
  the product could produce, and the two admin translation screens could display
  a translation they had no way to create.

  Two write paths now exist. `POST /admin/content/{id}/translate/{lang}` saves a
  translation from the screen that already showed it, with the same `_token`
  form protection as the rest of the admin, and a sibling delete route withdraws
  one: a screen that can add a translation and not take it back leaves a wrong
  translation on the site with no way to retract it short of SQL, which is the
  situation this path exists to end. The form is built from the content type's
  own text fields rather than a fixed pair of boxes, and only from its text
  fields: a boolean, a date or a file reference is the same value in every
  language, and offering a per-language copy of one invites two answers to a
  question that has one. Saving replaces on `(item_id, language)`, because a
  translation is one language's version of the whole item; `created` survives a
  replace and `changed` moves. Translating an item into its own language is
  refused, since the overlay would apply the row on top of the original it
  duplicates.

  The second path is configuration. `item_translation.<uuid>.<lang>.yml` is a
  config entity, so a config set can ship an item and its translations together
  and a translated site exports and re-imports like any other. It is a separate
  entity rather than a key on the item file because a translation has its own
  identity: `(item_id, language)` is its primary key and it carries its own
  timestamps, where the two things an item file already carries beyond its struct
  (a tag's parents, a role's permissions) are 1-to-N data with no identity of
  their own. The filename parser splits an entity file on its first dot, so
  `<uuid>.<lang>` arrives as one id and no parser change was needed. The type is
  ordered after `item`, so both import in one pass, and importing a translation
  of an item that does not exist is refused: `item_translation` has no foreign
  key to `item`, so it would otherwise import cleanly and be read by nothing.
  Export tolerates the table being absent entirely, which it is on a site that
  never enabled the plugin that creates it.

  The third path asked for is **not** added, and this is the finding rather than
  an omission. There is no SDK-visible way for a plugin to write a translation
  because no existing WIT item write can carry a language: `save-item` takes
  opaque item JSON, and the host behind it builds `UpdateItem`, which has no
  `language` field at all, or `CreateItem` with `language: None` hardcoded, so a
  plugin cannot set even an item's own language. The `tap-item-*` exports are
  notification hooks carrying the same language-free JSON. Adding a carrier would
  mean changing a WIT signature or an SDK type, so it is recorded instead.

  Regression tests: the form creates a translation that was not there, a second
  save replaces rather than accumulating, a forged `_token` writes nothing, a
  user without `translate content` writes nothing, an unknown language is a 404,
  an item cannot be translated into its own language, a translation can be
  removed, and a written translation is what an Italian reader is served. On the
  config side, an item and its translation import together, and a translation of
  a missing item is refused.

- Fix: a plugin's permissions exist, can be seen, and can be granted.

  `tap_perm` was declared in the WIT and never dispatched, so the permissions a
  plugin declares existed only inside the plugin. Nothing in the kernel had a
  list of them, and three things followed from that at once: the permission grid
  could not show or grant one, `config import` refused to name one in a
  `role.*.yml` file, and saving the grid **revoked** any that SQL or a migration
  had inserted, because the save rebuilt each role's whole set from the kernel's
  list and anything absent from that list was indistinguishable from a box
  someone had deliberately unticked. On a 0.102.0 site that last one deleted 29
  grants across every role. So there was no durable way for a role to hold a
  plugin's permission at all: the documented workaround was SQL, and the next
  visit to the grid undid it.

  The kernel now dispatches `tap_perm` at boot, beside `tap_menu` and for the
  same reason: a declaration is a constant, so it is collected once rather than
  asked for during a request. What comes back is parsed into a registry that
  stamps the declaring plugin onto each permission, the way the menu registry
  already stamps its own, and a plugin that returns nonsense is recorded and
  skipped rather than allowed to stop the boot.

  The result is also written to a new `plugin_permission` table, which the other
  two registries have no need for. The reason is `config import`: it validates a
  role file's permission list, and it runs as a CLI with a pool and no
  `AppState`, so a registry that lived only in memory would be invisible to
  exactly the consumer that makes a plugin's permission nameable in a config
  file. The table is a cache of declarations and never a grant; `role_permissions`
  remains the only place a permission is held. A plugin that is disabled cannot
  answer, so a refresh replaces only the rows of the plugins that did, and
  validation still accepts a permission some role already holds, which is what
  lets such a site re-import.

  The grid renders plugin permissions beside the kernel's with a column naming
  the plugin that declared each, and its save now replaces only the permissions
  the form actually rendered: `Role::set_permissions_within` confines removals to
  that set, so a permission the screen never showed keeps whatever it had,
  granted or not. The form states which permissions it rendered by submitting
  each name as a hidden value keyed by row index, which also retires the old
  checkbox key that encoded the name with spaces replaced by underscores. That
  encoding was not reversible and stopped being merely ugly the moment plugin
  permissions arrived, since names like `create argus_feed content` already
  contain underscores.

  A plugin enabled from `/admin/plugins` while the server runs has its
  declarations refreshed immediately when its module is already compiled, and
  otherwise registers them on the next restart, because the runtime loads the
  plugins enabled at boot and builds the tap registry from that set once. That is
  the same restart boundary `plugin enable` on the CLI already documents and the
  one `tap_install` already waits for; KNOWN-ISSUES.md records it.

  Not changed, and reported instead: `current-user-has-permission` on the user
  host interface is still a literal string membership test, so it disagrees with
  the kernel's route check for an administrator. Correcting it needs a decision
  that is not this change's to make, because the kernel's own routes do not agree
  with each other about what `administer site` means: `require_permission` bypasses
  on the `users.is_admin` column, while `routes/item.rs` bypasses on
  `UserContext::is_admin()`, which is the string `administer site`. A role holding
  `administer site` and nothing else therefore passes one check and fails the
  other. BL-33 already records the split. Making the host function match "the
  kernel's semantics" requires first choosing which of the two the kernel means.

  Regression tests: a declaration is parsed with its owner, a double-encoded one
  parses too, a malformed one is dropped without taking the others down, a plugin
  cannot claim a kernel permission or one another plugin claimed, a refresh
  replaces only the plugins that answered, and a config file grants a permission a
  plugin declared while one nothing declares or holds is still refused.

- Fix: the admin UI asks what you may do, not whether you are the superuser.

  111 admin route handlers across 17 files called `require_admin`, which gates on
  the `users.is_admin` column. That column is not a permission: it cannot be
  granted to a role, it cannot be named in a `role.*.yml` config file, and it
  does not appear in the permission grid. So a role holding exactly the right
  permissions still could not use the admin UI to exercise them. The case that
  named the bug was a role granted `administer argus` that got 403 on
  `/admin/content/add/argus_feed`, because that screen asked whether you were a
  superuser rather than whether you could create that content. The public
  `/item/add/{type}` route had always asked the second question, so the two ways
  into the same operation disagreed, and the admin one was unreachable by anyone
  but a superuser.

  Each call is now `require_permission` against a permission the route already
  implies, drawn from the strings the kernel and its plugins already declare; no
  parallel names were invented. `/admin/content/add/{type}` builds
  `create {type} content` from the path, the same string `/item/add/{type}`
  checks, which is what makes a plugin's content types reachable by a role.
  `/admin/content/bulk` is gated on the action it was asked to perform, so a
  role that may publish cannot obtain a delete by routing it through the bulk
  endpoint. Content, comment, file, user, category and AI screens take their own
  permissions; the structure and configuration screens, for which nothing more
  specific is declared anywhere, take `administer site`, which is at least a
  grantable permission where `is_admin` was not. `docs/admin-permissions.md` is
  the full table.

  Nothing was weakened. `require_permission` keeps the superuser bypass, so a
  superuser reaches every one of these routes exactly as before, and a user
  without the permission is refused exactly as before; the only new case is the
  non-superuser who holds it. Two routes keep `require_admin` on purpose:
  `/admin/plugins` and `/admin/plugins/toggle`, because enabling a plugin runs
  new code in the kernel and can introduce permissions, routes and tables, which
  is not something a permission should be able to grant.

  The superuser flag itself did not become delegable. The user add and edit
  forms carry an `is_admin` checkbox, and those routes are now gated on
  `administer users`, so honouring that checkbox for a non-superuser would have
  turned `administer users` into a self-escalation to superuser, and would
  equally have let a delegated user administrator revoke the real superusers and
  lock them out. Only a superuser may set or clear the flag; for anyone else the
  stored value is preserved whatever the form submitted, and the rest of user
  administration stays delegable. Regression tests: a non-superuser holding each
  permission reaches its screen, a user holding none is refused every one, one
  admin permission does not open the others, the superuser bypass still reaches
  all of them, and `create conference content` opens the conference add form
  while `create page content` does not.

- Fix: the kernel reads one long text field under both the names it goes by.

  The kernel's `page` type declares its long text field as `body`; a content type
  that declares its own calls it `field_body`, as `trovato_blog` does. Every
  kernel reader was written against one name, so each rendered nothing for half
  the site's content: the promoted-items renderer on the front page read `body`
  only, so a promoted blog post's teaser was a title, a date and a "Read more"
  link with nothing between them, and page metadata read `field_description` then
  `field_body` only, so a stock `page` item had no meta description, no Open Graph
  description and no feed description at all. Neither reader was wrong about its
  own content type, which is why both survived: each one works on exactly the
  half of the content model it was written for.

  `content::body_field` is now the one place that knows the pair. It reads
  `field_body` first, then `body`, so a type that declared its own field wins over
  the kernel default, and treats a present-but-empty field as absent. The
  promoted-items renderer, page metadata, the feed description, the pagefind
  description and index text, and the `ts_headline` snippet source in search all
  go through it; the snippet's field-access redaction now denies on either name,
  so widening the source cannot leak a restricted field. No field is renamed:
  that is a content migration for `item.fields`, `search_field_config` and every
  plugin reading the other name. `trovato_seo` still reads `field_body` alone and
  is left for a plugin change. Regression tests: a promoted item shows its teaser
  under either name on the front page, and a `page` with a `body` yields a meta
  description.

- Fix: the five remaining static file types BL-09 named are served as themselves.

  `mime_from_path` gained `.md`, `.txt` and `.xml` in `c4dffc5`, leaving five of
  the eight the finding listed still falling through to
  `application/octet-stream`, which under `nosniff` makes a browser download a
  file instead of displaying or using it. `.webp` and `.avif` are now
  `image/webp` and `image/avif`, `.pdf` is `application/pdf`, `.otf` is
  `font/otf`, and `.webmanifest` is `application/manifest+json`, without which a
  browser ignores a web app manifest entirely. An extension outside the table is
  still `application/octet-stream`, which is the point of the fallback. The test
  covers all eight in one place.

- Docs: `docs/BACKLOG.md` carries the fix series' results.

  Seven rows the series closes outright are marked fixed with the commit they
  landed as (BL-10, BL-11, BL-12, BL-13, BL-15, BL-16, BL-17); BL-09 closes on
  `c4dffc5` plus the work above. BL-07 stays open: #70 repointed the link at a
  page that exists, and how the reset flow should work has not been ruled on.
  BL-21 becomes partly fixed, its kernel readers done and its listing template
  still open. Four findings the run turned up and did not fix are recorded as
  BL-112 to BL-115: the login page's unconditional sign-up link against a
  registration route that 404s when registration is closed, which is the default;
  the startup warning that tells an operator a served path will 404 when only the
  callback is dead; the web layer design document describing a per-query, per-tap
  and per-template profiler where the middleware times the whole request and
  nothing else; and eleven plugin menu paths no route serves, checked by
  composing the kernel's whole route set rather than by searching for the strings.
  Every passage that counts rows was recounted against the rows rather than
  adjusted.


- Fix: `QUERY_SLOW_THRESHOLD_MS` does something, and every response reports its
  duration.

  `track_request_timing` in `middleware/query_profiler.rs` was written,
  exported, and unit-tested for its threshold arithmetic, and applied to no
  router. So no response ever carried a `Server-Timing` header, no slow request
  was ever logged, and a documented setting with a documented default steered
  nothing. Its module doc also claimed a `query-profiler` cargo feature, which
  `Cargo.toml` has never defined, and per-query profiling, which it has never
  done.

  The layer is now applied in `main.rs`, outside every other layer so the
  measurement covers the whole request, and in the integration test harness in
  the same position. The module doc says what it measures. `request_timing_test`
  requires a parseable `total;dur=` on a page, a static asset and a 404 (the
  layer is outside routing), and asserts that `main.rs` applies it, since the
  defect was precisely that nothing did.

- Fix: a themed plugin page renders its title as the page heading.

  `ApiResponse::themed` carries a title, and `ThemeEngine::render_page` puts it
  in the template context, where `templates/page.html` used it for the document
  `<title>` and nothing else. A plugin page therefore rendered with no `<h1>`:
  the site chrome and a bare body, with no heading for a reader or a screen
  reader to navigate by. `/contact`, the themed plugin page in the tree, was
  the visible case.

  `page.html` now renders `page_heading` as the page's `<h1>`, and the themed
  plugin route sets it from the response's title. It is a separate context key
  rather than `title` because an item page, a gather page and an assistant
  conversation already render their own heading inside the content they pass to
  the same template; making the template render `title` unconditionally would
  give each of them a second one. `contact_form_test` requires the heading,
  requires exactly one `<h1>` on the page, and requires it to precede the
  plugin's form.

- Fix: a default install no longer logs a startup warning for every stock
  plugin's admin menu.

  Sixteen in-tree plugins built their `tap_menu` entries with
  `MenuDefinition::new(...).callback("...")`. `MenuDefinition` has no handler
  type, so the kernel reads each entry as `handler_type = "page"`, and it only
  ever dispatches a callback for `"api"` (`routes::plugin_api`). Every one of
  those 22 callbacks named a handler that did not exist and could never be
  called. The kernel warns
  about exactly this at startup, so the log opened with twenty warnings about
  the project's own code, which buries the one warning meant to catch a plugin
  route that is really dead. The callbacks were copied from plugin to plugin as
  if a menu entry needed one.

  The dead `.callback(...)` calls are removed; paths, titles, permissions and
  parents are unchanged. The warning stays a warning, so external plugins that
  trip it still load. Whether each of those menu paths is served by some route
  is a separate question this change does not touch. `plugin_menu_callbacks_test` scans every in-tree plugin's
  shipped source for a callback on a `MenuDefinition`, or on a `MenuRoute` that
  is not `"api"`, and fails naming the file and line. It reads source because CI
  builds only some plugins to WASM, and a runtime check would silently skip the
  rest. On the old source it names all 22 entries.

- Fix: item views no longer log an error from `trovato_blog`.

  `trovato_blog.info.toml` listed `tap_item_view` in `[taps] implements`, and
  the plugin's source has never exported it. The kernel registers a handler for
  every declared tap without checking the module's exports, and only finds the
  function missing when it dispatches, so every item view of any type
  instantiated the blog module, found no export, and logged "tap invocation
  failed". The blog renders items through templates and has no view hook to
  offer, so the declaration is removed.

  Nothing compared a manifest with the source it describes. A test in
  `plugin_surfaces_test` now reads every in-tree plugin's manifest and requires
  each declared tap to be exported, through `#[plugin_tap]`,
  `#[plugin_tap_result]` or a hand-written `no_mangle` function. On the old
  manifest it names `trovato_blog` and nothing else.

- Fix: the two content translation admin pages render instead of answering 500.

  `routes/admin_translation.rs` renders `admin/content-translate-list.html` and
  `admin/content-translate-edit.html`, and neither template was ever written.
  `trovato_content_translation` puts both routes in the admin menu, so a user
  allowed to translate content followed a menu link to a Tera "template not
  found" error. Nothing checks at build time that a template name in Rust names
  a file, so the gap was invisible until a request. Writing the templates
  exposed a second omission: both handlers leave `path` out of the context, and
  `page--admin.html` reads it for the active menu entry, so the pages would
  still have failed in the parent template. Both handlers now insert it.

  The templates are read-only: a list of the site's languages with each one's
  translation state, and one language's stored translation beside the original.
  Translations still have no write path, and the pages do not pretend to offer
  one. BL-15 in `docs/BACKLOG.md` covers this.

  `admin_translation_test` renders both pages against a real translation and a
  missing one, and scans kernel source so every template passed to
  `render_admin_template` or `tera().render` must exist under `templates/`.

- Fix: `.md`, `.txt` and `.xml` files under `static/` display instead of
  downloading.

  `mime_from_path` in `routes/static_files.rs` is a closed list of extensions
  with `application/octet-stream` as the fallback, and it had no arm for any of
  the three. Every static response also carries `X-Content-Type-Options:
  nosniff`, so a browser trusts that type and saves the file: `llms.txt`, a
  served markdown document and a static sitemap all downloaded. The arms now
  exist, as `text/markdown`, `text/plain` and `application/xml`, each with an
  explicit UTF-8 charset so the text is not decoded as Latin-1. One unit test
  per extension. The other extensions BL-09 lists (`.webp`, `.avif`, `.pdf`,
  `.webmanifest`, `.otf`) are still open.

- Fix: passkey sign-in appears on the login page of a default install.

  The kernel's enforcing CSP has no `'unsafe-inline'`, nonce or hash in
  `script-src`, and the passkey code in `templates/user/login.html` was an inline
  `<script>` block, so the browser never ran it. That script is what reveals the
  hidden "Sign in with a passkey" button, so the button never appeared. The
  mismatch survived because `middleware/security_headers.rs` documented that
  every inline script had already been externalised, and because the
  integration test harness never installed the security-header middleware, so
  no test could see the policy a browser enforces.

  The script now lives in `static/js/passkey-login.js`, loaded with `src`.
  `script-src` is unchanged. The comment now says what the policy does and
  points at BL-08 for the templates that still carry inline scripts;
  `KNOWN-ISSUES.md` and `docs/BACKLOG.md` drop login from that list. The test
  harness installs `inject_security_headers` in the same position as `main.rs`,
  and `login_page_csp_test` reads the CSP from the response and requires every
  executable script on the login page to load from a file.

- Fix: the login page's "Forgot password?" link loads a page.

  `templates/user/login.html` linked it to `/user/password-reset`, which the
  kernel registers for POST only: it is the JSON endpoint that mints a reset
  token, not something a browser can open, so the click answered 405. The link
  was written against the API endpoint rather than the human recovery flow, and
  nothing rendered the login page and followed its links, so the mismatch never
  surfaced. It now points at `/user/recover`, the recovery entry page.
  `login_page_test` follows the link the page renders and requires it to load.

- Fix: a hand-written config file may omit `created` and `changed`.

  Only `ConfigItem` gave its timestamps a serde default. `Role`, `Tag`,
  `UrlAlias`, `Stage`, `Tile` and `MenuLink` declared `created` as a plain
  required field, and all but `UrlAlias` required `changed` too, so a file
  written by hand without them failed to deserialize. Config import validates
  the whole set before it writes anything, so that one file stopped every other
  file in the set from importing. The report named roles; the same fault was in
  five more entity types.

  Each of those fields now defaults to the time of deserialization
  (`models::unix_now`, and `Utc::now` for a role, whose `created` is a
  timestamp rather than an integer). Storage never overwrites an existing row's
  `created` on re-import, so the default only dates new rows.

  Import stays atomic on purpose: a file that is genuinely wrong is named in the
  error and nothing is written. `config_import_test` imports one file of each of
  the six types with no timestamps and checks each row is stamped with the
  import time, and pins that a bad role file is reported by name with its valid
  sibling left unwritten. The role/everything-else timestamp type mismatch in
  BL-11 is still open.

- Fix: config import sets an item's `promote` and `sticky`, and updates
  `created` on re-import.

  `ConfigItem` had no `promote` or `sticky` field, so a file that declared them
  parsed cleanly and the values were silently dropped; `save_item` in
  `config_storage/direct.rs` then bound both as literal `0, 0` in the INSERT
  and left them, and `created`, out of `ON CONFLICT DO UPDATE`. No config file
  could put an item on the front page or pin it, and a corrected creation time
  never reached an item that already existed.

  `ConfigItem` now carries both flags (default false), export writes them, and
  the upsert takes all three from the file. A file that omits `created` still
  inserts with the current time and, on re-import, keeps the stored value, since
  an absent timestamp is not a claim that the item was created now. `changed`
  is unchanged. `item_import_sets_promote_sticky_and_created_and_updates_them`
  covers insert, re-import, an omitted `created`, and the export round trip.

- Tests: the kernel's config-import tests read a fixture the kernel owns, instead
  of the tutorial's configuration set.

  `docs/tutorial/config/` carried Ritrovo's content model as well as the
  tutorial's, and that model has moved to the Ritrovo repository
  (jeremyandrews/ritrovo#11, merged as `3322075`). Four kernel tests read the
  directory: a unit test `include_str!`d `item_type.conference.yml`, so a rename
  downstream would have stopped `cargo test --lib` compiling, and three
  integration tests asserted the set's counts and its roles' exact permissions.

  They now read `crates/kernel/tests/fixtures/config-set/`, thirteen files chosen
  for the import behaviour each one pins rather than for what they model: a
  `search_field_config` whose bundle resolves to an item type in the same set, a
  tag resolving both its category and its parent, a menu link resolving another,
  roles whose permissions import applies, and stages landing under the UUIDs their
  files declare. Its README says what each file is for.

  `docs/tutorial/config/` is unchanged and still shipped: the image copies it and
  Parts 1 to 7 import it. Parts 2, 3, 4 and 7, the directory's own README and
  `docs/docker-development.md` now say where the model went and that this copy is
  a frozen snapshot. Nothing in the tutorial's instructions changes.

- Security: rustls 0.23.37 -> 0.23.45 (and rustls-webpki 0.103.13 -> 0.103.15,
  which it requires), clearing RUSTSEC-2026-0285.

  The advisory, published 2026-09-14, is that rustls accepted TLS 1.3 handshake
  messages across encryption level boundaries. Nothing in the tree changed: the
  lockfile pinned a release the advisory database now names, so `cargo audit`
  turned every pull request red from that date. The fix is a lockfile-only bump
  within the same minor series; the `Security Audit` CI job is the regression
  guard, and it fails against the old lockfile.

- Fix: plugin mail is rate limited on every path, not only the web-facing one.

  The `mail` host interface bounds who a plugin may write to — the site's own
  contact address and nothing else — but not how often. The web-facing case was
  covered incidentally, because a plugin-served POST falls into the `forms`
  bucket per client IP; a plugin calling `mail` from `tap_cron` or
  `tap_queue_worker` fell into nothing, because the rate limiter only ever saw
  requests and background dispatch never carried it.

  There is now a `mail` bucket, 100 messages an hour, checked inside the host
  function so it applies wherever the call comes from. It is keyed **per plugin**
  rather than per client: the mailbox being protected is the site's own, so a
  plugin in a loop floods it regardless of who set it going, and a per-IP bound
  cannot see that. Configurable like every other bucket, through
  `TROVATO_RATE_LIMIT_MAIL` or the `rate_limit.mail` site config key.

  The check runs before the SMTP-handle test and before payload validation, so
  the answer does not depend on which path the call arrived on, and a plugin
  cannot spend its window on malformed requests. A refusal returns the new
  additive `ERR_MAIL_RATE_LIMITED` (-55), distinct from `ERR_MAIL_SEND_FAILED`
  because the remedy is different: waiting fixes this one.

  The rate limiter is now built early enough in `AppState::new` to reach every
  dispatch path, and is threaded through `CronService` into both background taps.

- Fix: static assets are no longer rate limited as API calls.

  `categorize_path` had no branch for an asset path, so a GET of a stylesheet,
  a script or an uploaded image fell through to the trailing `"api"` default and
  was counted against the generic bucket of 100 a minute per IP. A page carrying
  twenty assets therefore spent twenty-one of a visitor's hundred requests, and
  five such page views in a minute came within a hair of serving a 429 for a
  favicon — worse behind a NAT, where the bucket is shared. Nothing was
  configurable either: `state.rs` built `RateLimitConfig::default()` and there
  was no way to raise any limit without recompiling.

  Static reads now have their own `static` bucket, defaulting to 2000 a minute,
  matched on `/static/`, the configured `FILES_URL` (default `/files/`) and
  `/favicon.ico`. Only GET and HEAD qualify, so a write under an asset prefix
  does not inherit the generous limit. Every bucket's limit is now overridable by
  `TROVATO_RATE_LIMIT_<BUCKET>` or the `rate_limit.<bucket>` site config key,
  environment first; an unparseable or zero value is ignored with a warning
  rather than taking the site down. The full table of buckets and defaults is in
  [INSTALL.md](INSTALL.md#rate-limits).

- Fix: a plugin reading a variable that is not set is no longer silent (BL-03,
  partly).

  `host/variables.rs` namespaces every key as `plugin.{plugin_name}.{name}` and
  returned the caller's default on `Ok(None)` with nothing logged. The
  namespacing makes the mistake easy to hit: a plugin asking for another
  plugin's setting is asking for a key under its own name, which will never
  exist, so the read succeeds with the default forever and nothing anywhere says
  why.

  The host now logs at warn level the first time each key misses in the life of
  the process, naming both the key the plugin asked for and the namespaced key it
  resolved to. Once per key, so a variable read on every request warns once;
  tracking is capped so a plugin building keys from request data cannot grow the
  set without bound.

  The return value is unchanged and still indistinguishable from a set variable.
  The WIT declares `get: func(name: string, default-value: string) -> string` —
  no `option`, no `result` — and the SDK reads every negative return as a host
  failure, so a sentinel for "unset" would make an ordinary unset variable look
  like a failure to every plugin already compiled. That is a contract change, not
  an additive one, and it is recorded against BL-03 in `docs/BACKLOG.md` as an
  opt-in addition for a later minor.

- Fix: `sitemap.xml` emits absolute addresses, and lists translations.

  `routes/sitemap.rs` wrote the URL alias, or `/item/{id}`, into `<loc>` with no
  scheme or host. The sitemap protocol requires a full URL, and a sitemap is
  fetched with no request context to resolve a relative path against, so every
  address in the document was unusable. `robots.txt` named the sitemap the same
  relative way, which its own specification also forbids.

  Both now build on `SITE_URL`, the setting the canonical link, the Open Graph
  tags and the RSS feeds already read; a trailing slash on it no longer doubles.
  Values are XML-escaped, so an alias containing `&` no longer makes the document
  unparseable. `SITE_URL` itself was undocumented and is now in
  [INSTALL.md](INSTALL.md) and `.env.example`.

  A translated item was listed once, in the default language, with no `hreflang`
  at all. It is now listed at every address it is readable at, each entry
  carrying the whole alternate set built by `build_hreflang_links` — the same
  function the item and front pages use for their `<head>`, so a crawler cannot
  be told two different stories about one page. Existence comes from one bulk
  query rather than the per-item lookup `available_translations` does.

  A plugin claiming `/sitemap.xml` or `/robots.txt` is still refused, but now at
  startup with a message naming the plugin and the path, instead of by a panic
  inside `Router::merge` reporting an overlapping route.

- Fix: gather rows carry a resolved `url`, so a listing can link to a friendly
  address.

  `templates/gather/row.html` and every other shipped listing template emitted
  `/item/{{ row.id }}`, because the row had nothing else to emit. A site built on
  the kernel therefore advertised a UUID in every link on every listing, while
  the friendly URL the item itself was served at sat in `url_alias` unused. A
  template could not fix it alone: resolving an alias is a database lookup, and a
  template doing one per row is the shape to avoid.

  `GatherService` now resolves the whole result set at once — one query for the
  page, not one per row and not one per render — and gives each row a `url`: its
  URL alias when it has one, `/item/{id}` otherwise. Resolution honours the
  gather's own stage overlay and language. A row from a field list that omits
  `id` gets no `url` rather than a wrong one, and the shipped templates keep a
  `/item/{id}` fallback for it. Record gathers are untouched: a record has no
  `/item/` address.

- Fix: the `markdown` filter keeps the class that makes a code block
  highlightable.

  `theme/engine.rs` sanitized the filter's output with `ammonia::clean`, whose
  defaults strip every `class` attribute. `pulldown_cmark` renders a fenced
  ```` ```rust ```` block as `<code class="language-rust">`, the class was
  removed, and nothing downstream could then tell what language the block was —
  so highlighted code was impossible in any Markdown field on the site.

  The filter now runs an `ammonia::Builder` that allows `class` on `code`, `pre`
  and `span` only, and only for values on an allowlist: `language-*` and `lang-*`
  on the block, with the name bounded to the characters real language names use,
  and the token classes Prism and highlight.js apply on a span. A class with
  nothing left after filtering is dropped rather than left empty. Everything else
  is unchanged from ammonia's defaults: a class on a `div`, a `style`, an
  `onclick` and a `<script>` are all still removed.

- Fix: `argus_notify_test` no longer passes or fails on scheduling (#67, closes
  #66).

  `the_pipeline_turns_a_summarized_story_into_a_dispatched_notification`
  asserted that the founding notification carries `article_count` 1. That value
  is snapshotted when summarize runs (`argus-core/src/pipeline.rs`), so it is 1
  only if the second article has not clustered yet — an ordering the test never
  established and the queue never promised. `claim_batch` claims per *plugin*,
  ordered by `created_at` across every stage at once, and runs the batch in
  parallel, so two articles seeded together produce two cluster jobs that may or
  may not coincide. On one unchanged commit in CI this failed twice and passed
  once.

  The test now seeds and drains the two arrivals one at a time, so the sequence
  it asserts is one it arranged. No assertion was loosened; two were added,
  pinning the founding state before the second article exists so a regression
  names its own cause. The pipeline is unchanged — it was behaving correctly
  under the documented per-plugin concurrency model.

- `docs/BACKLOG.md`: every kernel finding recorded by this tree and by the
  projects built on it (the trovato.rs site, Netgrasp, Argus), verified against
  the code and classified as blocking 1.0, due in 1.0.x, or after. Four
  descriptions in `KNOWN-ISSUES.md` that the code contradicted are corrected:
  inline scripts, the permission grid, plugin mail from cron, and template
  reloading.

## v0.102.0 — 2026-09-02

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

The other half of the release is multilingual. A site could be configured in
several languages and served correctly in only one of them: a language prefix
reached content aliases and nothing else, a translated gather page carried
default-language navigation, and the front page offered no way out of whichever
language a reader arrived in. Those are one fault in three places, which is that
the kernel knew the language and the things built from it did not.

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

- Fix: a language prefix reached only content, so most of a translated site 404'd.

  Root cause: two layers each believed the other was not stripping the prefix.
  `negotiate_language` rewrites the URI to remove `/{lang}`, which in axum 0.8 is
  too late to affect routing (`Router::layer` middleware runs *after* the route is
  chosen) but early enough that `path_alias_fallback`, which re-detects the prefix
  by stripping one itself, found nothing left to strip. Its `had_lang_prefix` was
  therefore false for every request that came through the middleware, which is all
  of them, and its "a prefix was here, try the stripped path" branch never ran.

  So every language-prefixed address that was not a content alias returned 404:
  `/it/search`, `/it/user/login`, a gather page in Italian, and `/it/` itself, the
  address a multilingual site is most often entered on. Aliases went on working
  the whole time, because they take the other branch, which is why this survived a
  release: the only prefixed addresses anybody tested were the ones that worked.

  The fallback now reads `RequestedPath`, recorded before the strip and the only
  surviving record of the address asked for. A prefixed path with nothing behind
  it is still a 404: the re-dispatch forwards the stripped path, it does not
  invent a page.

- Fix: an Italian gather page shipped English menus.

  Root cause: a context-ordering contract that held for a value and not for what
  is derived from it. `inject_site_context` fills `active_language` in only when
  the context does not already carry it, which made a route's own insert
  order-independent; it then reads that language back out and builds the localized
  menus from it, which makes the *call* order load-bearing again. `routes/gather.rs`
  inserted after the call, so the menus were built for the site default and the
  language arrived in time for `<html lang>` and nothing else.

  A reader got `lang="it"` over navigation addressed and labelled in English: the
  one combination that tells a person and a screen reader different things about
  the same page. The insert moves above the call, and the helper's contract now
  says what it actually requires rather than what was true before menus read it.

- The front page offers its translations, like every other page.

  The item route gave every page `available_translations` and `hreflang_links`;
  the front route gave the same item neither, so the page a multilingual site is
  most likely to be entered on was the one page with no switcher to leave it by
  and nothing for a crawler to follow. The addresses are the front page's own, `/`
  and `/{lang}/`, not the configured item's alias: a switcher that lands a reader
  on `/it/whatever-the-item-is-called` has moved them off the front page.

- Fix: Gather sorted numeric JSONB fields as text.

  Root cause: `add_sorts` routed a `fields.*` sort through `jsonb_extract_expr`,
  which builds `fields->>'path'`. `->>` extracts **text**, so `ORDER BY` compared
  strings whatever the JSON type actually was. A documentation index sorted by a
  numeric `fields.weight` (0, 10, 20 ... 350) rendered as 0, 10, 100, 110 ... 20,
  200 ...: the tutorial's Part 7 led the list and Parts 1 to 6 sat at the bottom.

  `ORDER BY` now extracts with `->` (jsonb) through a new `jsonb_sort_expr`
  beside the existing helper, which is left alone: filters and selects read text
  on purpose. jsonb comparison is typed, so numbers compare numerically and
  strings lexically without the query author declaring anything, and a nested
  path stays jsonb at every hop. Mixed types under one key now sort by jsonb's
  type ordering, which is a data problem surfaced rather than created.

- Security: update wasmtime to 47.0.4, closing two advisories in the plugin
  runtime.

  RUSTSEC-2026-0269 (8.8 high) is a filesystem sandbox escape when paths or
  symlinks carry trailing slashes. RUSTSEC-2026-0268 (6.9 medium) lets a guest
  drive a host heap allocation of its own choosing through WASIp3 streams. Both
  land squarely in the threat model: the kernel runs plugin code it does not
  trust, and the sandbox is what stands between a plugin and the host filesystem.

  A patch bump inside the existing `wasmtime = "47"` range, so only `Cargo.lock`
  moves. `cargo audit` reports no vulnerabilities afterwards, and the whole
  plugin and runtime suite — 116 tests across the invocation, API, mail, queue,
  recovery, contact and assistant paths — passes against the new runtime.

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
