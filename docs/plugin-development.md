# Trovato Plugin Development Guide

This guide covers everything you need to know to develop plugins for Trovato CMS.

## Table of Contents

1. [Quick Start](#quick-start)
2. [Plugin Structure](#plugin-structure)
3. [The Tap System](#the-tap-system)
4. [Content Types and Fields](#content-types-and-fields)
5. [Rendering Output](#rendering-output)
6. [Host Functions](#host-functions)
7. [Access Control](#access-control)
8. [Menus and Permissions](#menus-and-permissions)
9. [Database Operations](#database-operations)
10. [Caching](#caching)
11. [Inter-Plugin Communication](#inter-plugin-communication)
12. [Letting an assistant configure your plugin](#letting-an-assistant-configure-your-plugin)
13. [Owned vocabularies](#owned-vocabularies)
14. [Page builder components](#page-builder-components)
15. [Testing](#testing)
16. [Deployment](#deployment)
17. [Best Practices](#best-practices)

---

## Quick Start

### Prerequisites

- Rust toolchain with `wasm32-wasip1` target
- Running Trovato kernel with PostgreSQL and Redis

Install the WASM target:

```bash
rustup target add wasm32-wasip1
```

### Create a New Plugin

1. Create the plugin directory:

```bash
mkdir -p plugins/my_plugin/src
```

2. Create `plugins/my_plugin/Cargo.toml`:

```toml
[package]
name = "my_plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
trovato-sdk = { path = "../../crates/plugin-sdk" }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

3. Create `plugins/my_plugin/my_plugin.info.toml`:

```toml
name = "my_plugin"
description = "My first Trovato plugin"
version = "0.1.0"

[taps]
implements = ["tap_item_info"]
weight = 0
```

4. Create `plugins/my_plugin/src/lib.rs`:

```rust
use trovato_sdk::prelude::*;

#[plugin_tap]
fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "my_type".to_string(),
            label: "My Content Type".to_string(),
            description: "A custom content type".to_string(),
            fields: vec![
                FieldDefinition::new("body", FieldType::TextLong)
                    .label("Body")
                    .required(),
            ],
        }
    ]
}
```

5. Build the plugin:

```bash
cargo build -p my_plugin --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/my_plugin.wasm plugins/my_plugin/
```

6. Restart the kernel to load your plugin.

---

## Plugin Structure

### Directory Layout

```
plugins/my_plugin/
├── Cargo.toml           # Rust package configuration
├── my_plugin.info.toml  # Plugin metadata and tap declarations
├── my_plugin.wasm       # Compiled WASM binary (generated)
└── src/
    └── lib.rs           # Plugin source code
```

### Plugin Metadata (`.info.toml`)

Every plugin requires an info file declaring its metadata and capabilities:

```toml
name = "blog"
description = "Provides a blog content type with tags"
version = "1.0.0"
dependencies = ["categories"]  # Optional: plugins that must load first

[taps]
implements = [
    "tap_item_info",
    "tap_item_view",
    "tap_item_access",
    "tap_menu",
    "tap_perm",
]
weight = 0  # Lower weight = earlier execution
```

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Machine name (lowercase, matches directory) |
| `description` | Yes | Human-readable description |
| `version` | Yes | Semantic version (e.g., "1.0.0") |
| `dependencies` | No | Array of required plugin names |
| `[taps].implements` | Yes | Array of tap function names |
| `[taps].weight` | No | Execution order (default: 0) |
| `[migrations].files` | No | Array of SQL migration file paths |
| `[migrations].depends_on` | No | Plugin names whose migrations run first |
| `[capabilities]` | No | What the plugin may reach: host interfaces, tables, `raw_sql` and the background grants. Absent means nothing. See [plugin-capabilities.md](plugin-capabilities.md) |
| `[config].tables` | No | The plugin's own tables that hold site configuration, carried by `config export` and `config import` (API 0.106). See [Configuration Tables](#configuration-tables) |
| `[config].secret_columns` | No | Per config table, the columns that hold a secret, a password, a token, a key or a hash of one; never exported (API 0.107). See [Secret Columns](#secret-columns) |

### Configuration Tables

A table your migrations create that holds site configuration (a list of
redirects, image styles, API clients) should travel with the site's other
configuration. Name it under `[config]`:

```toml
[config]
tables = ["redirect"]
```

`trovato config export` then writes each row as `redirect.<primary key>.yml`,
a map of column to value, after the kernel's own config entities, and
`trovato config import` upserts the rows by primary key, in the order the
plugins' migrations run. A row whose other unique key (a `name`, say) is held
under a different primary key, as a row your migration seeded with a random id
would be, takes that row over. Import refuses a row whose plugin would not be
enabled, that names a column the table lacks, or whose value does not fit its
column, before it writes anything.

Name only configuration: not content, not a log, a queue, a cache or anything
else that changes while the site runs. The table must be one your migrations
create and the table needs a primary key. Whether the plugin is enabled travels
too, as `plugin_status.<name>.yml`.

### Secret Columns

A config set is meant to be committed to a repository, so it never carries a
secret. Name every column of a config table that holds a secret, a password, a
token, an API or signing key, or a hash of one:

```toml
[config]
tables = ["webhook"]
secret_columns = { webhook = ["secret"] }
```

Export writes such a column as `env:TROVATO_SECRET_<TABLE>_<PK>_<COLUMN>` (upper
case, every other character `_`), or leaves it out when it is empty. Import
keeps the value the target row already holds; failing that it reads the named
variable from its environment; failing that it refuses the row and writes
nothing. A file holding a value in the clear there is refused. A column whose
name ends in `_hash` stores an Argon2id hash in PHC form: its variable holds
the plaintext, which import hashes (verify it with `argon2`'s
`PasswordVerifier`, as the OAuth token endpoint does). A secret column holds
text.

A column whose name says it is a credential (`secret`, `password`, `api_key`,
`access_token`, ...) is treated as secret even if you forget to name it, and
export warns. Name it anyway: the declaration is the contract, the name check
is the backstop.

### SQL Migrations

Plugins can include SQL migrations that run at startup. Add a `[migrations]` section:

```toml
[migrations]
files = [
    "migrations/001_gather_queries.sql",
    "migrations/002_roles.sql",
]
```

**Execution ordering:** Kernel tables are created first, then plugin migrations run in dependency order (topological sort). This means your migrations can safely reference kernel tables (`item`, `gather_query`, `url_alias`, `roles`, `role_permissions`).

**Forward-only:** Migrations have no rollback mechanism. Each file runs exactly once, tracked in the `plugin_migration` table. Use idempotent SQL patterns (`ON CONFLICT ... DO UPDATE/DO NOTHING`) so migrations are safe to re-run if the tracking state is lost.

**Gather query field references:** Filter and sort field paths (e.g., `"fields.display_name"`) are not validated against content type definitions at registration time. Double-check that field names in your gather query JSON match the `field_name` values in your `tap_item_info` definitions — a typo will silently produce NULL comparisons at query time.

### Gathers that require a permission

A gather is public unless it says otherwise: its page, its route aliases, its
feed and the JSON API serve its rows to whoever asks, filtered only by item
access. A gather over your own tables, or one listing things only some roles
should see, declares the permission a viewer needs in its definition:

```json
"access": { "permission": "view network devices" }
```

Seeded from a migration, next to the definition it guards:

```sql
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_device_list',
    'Devices',
    'Every device seen on the network',
    '{
        "record_type": "ng_device_state",
        "access": { "permission": "view network devices" },
        "sorts": [{"field": "last_seen", "direction": "desc", "nulls": null}]
    }'::jsonb,
    '{"format": "table", "items_per_page": 50, "canonical_url": "/devices"}'::jsonb,
    'netgrasp',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display    = EXCLUDED.display,
    changed    = EXCLUDED.changed;
```

For a gather that already exists, add the key in a later migration rather than
rewriting the whole definition:

```sql
UPDATE gather_query
SET definition = jsonb_set(definition, '{access}', '{"permission": "view network devices"}'::jsonb)
WHERE query_id IN ('ng_device_list', 'ng_device_online');
```

Declare the permission itself in `tap_perm` (see
[Defining Permissions](#defining-permissions)) so an administrator can grant it.

What the kernel then does for a viewer without it, before the query runs:

| Path | Anonymous visitor | Signed-in user without it |
|------|-------------------|---------------------------|
| `/gather/{query_id}`, its `canonical_url` or a URL alias to it, each `display.routes` path | redirect to `/user/login` | 403 |
| `display.routes` path with `?format=json`, `/api/v1/query/{query_id}`, `/api/v1/query/{query_id}/execute` | 403, JSON `error` | 403, JSON `error` |
| `display.feed` path | 403 | 403 |
| `/api/v1/queries` | the gather is left out | the gather is left out |
| `menu_link` rows pointing at any of the addresses above | not drawn | not drawn |

Administrators pass, as they do everywhere, and an anonymous visitor passes if
the anonymous role holds the permission. Rows are still filtered by item access
for a viewer who passes: this is a gate in front of the query, not a
replacement for it. Only the top-level definition's `access` counts; an
include runs as part of its parent.

A gather over a lightweight-record type (`record_type`) that declares no
permission requires `access content`, the permission the published path of
item access already asks for, so it is never public by accident. Declare a
permission of your own for anything more specific.

A `menu_link` row has no permission of its own. It is drawn for a viewer only
when the address it points at is one they could open: not a gather they may not
run, and not a plugin route whose `tap_menu` permission they lack. A link under
one that is not drawn is not drawn either.

### Gather feeds

A gather declares a feed in its display config and gets an RSS 2.0 document at
the path it names and an Atom 1.0 document beside it:

```json
"feed": {
    "path": "/blog/feed.xml",
    "title": "Blog",
    "description": "Recent blog posts",
    "items": 20,
    "full_content": false
}
```

| Key | Default | Meaning |
|-----|---------|---------|
| `path` | required | Where the RSS document is served. Absolute; may carry one argument (below). |
| `paths` | none | More addresses for the same feed, under the same rules. |
| `title` | the gather's `label` | Channel and feed title. |
| `description` | the gather's `description` | Channel description and Atom subtitle. |
| `items` | 20 | Entries carried, at most 200. |
| `full_content` | `false` | Also carry the body as HTML (RSS `content:encoded`, Atom `<content type="html">`). |

The Atom document is at the path's `.atom` sibling: the last segment's extension
replaced, or `.atom` appended when it has none, so `/blog/feed.xml` is also
`/blog/feed.atom` and `/blog/feed` is also `/blog/feed.atom`. It is derived
rather than declared so every feed has Atom with no config change and there is
no second path to keep unique. Both are advertised on every page with
`<link rel="alternate">` (the `feeds` template variable: `path`, `atom_path`,
`title`).

**The channel link.** RSS `<link>` and Atom `<link rel="alternate">` point at
the page the feed is a feed of — the address a visitor actually uses, not the
feed's own URL (that is `rel="self"`). It is chosen in this order:

1. For a feed reached with an argument, the `display.routes` path taking that
   one argument, filled with its value — `/blog/author/{user}/feed.xml` links
   `/blog/author/jeremy`.
2. The gather's `canonical_url`, if it has one.
3. The outbound URL alias of `/gather/{query_id}`, if the `url_alias` table has
   one — so a gather reachable only through an alias (no `canonical_url`, the
   way `trovato_blog`'s `blog_listing` ships: `/blog` is a URL alias, not a
   `canonical_url`, to avoid a redirect from `/gather/blog_listing` back to an
   alias that already rewrites to it) still links the address visitors use.
   Resolved per request, cached, and cleared the moment an alias is written —
   no restart needed, unlike the first two tiers.
4. `/gather/{query_id}` itself, when none of the above apply.

Every entry carries:

- an id that never changes, `urn:uuid:<item id>` (RSS
  `<guid isPermaLink="false">`, Atom `<id>`), so moving an item to a new URL
  alias does not make every reader show it again as new;
- the absolute link to the item, its URL alias when it has one;
- the author's account name (`dc:creator`, Atom `<author><name>`). An account
  name that is an email address is left out, never published;
- every taxonomy term on the item (`<category>`, Atom `<category term>`);
- a plain-text summary: the first of `summary`, `field_summary`, `field_teaser`
  or `field_description` the item has, otherwise its body (`field_body` or
  `body`) with the markup stripped and cut at a word boundary near 300
  characters;
- with `full_content`, the body rendered through its text format, as the item
  page renders it.

Text is escaped exactly once, the rule the search snippet uses: extracted from
the stored markup, then escaped for XML.

Every absolute URL (links, the `rel="self"` links, the Atom feed id) is built
from `SITE_URL`, never from the request's `Host` header. The kernel logs one
warning at startup when `SITE_URL` is unset or names a loopback host.

**Access.** A feed is the requesting viewer's own execution of the gather, the
same as its page: an anonymous aggregator gets what an anonymous visitor would
see, and a gather that requires a permission answers 403 (see the table above).
Nothing is cached on the server between viewers.

**Polling.** Each response carries an `ETag` and a `Last-Modified`. The `ETag`
hashes the entries' ids, `changed` timestamps, links and author names, the
terms each entry carries, the resolved channel link, and the feed's config, so
it moves on any of those. `Last-Modified` is the latest of: the newest entry's
`changed` (or `created`); the gather's own `changed`, so editing the feed's
title, description or other config moves it too, not only the `ETag`; and,
when the channel link resolves through a `url_alias` row rather than a fixed
path (tier 3 above), that row's own `changed`, so repointing the link moves it
as well. A request whose `If-None-Match` (or, without one, `If-Modified-Since`)
still matches gets `304 Not Modified` before terms are loaded or anything is
rendered. The query itself still runs, as the viewer, because that is what
decides what the viewer may see. An anonymous response is
`Cache-Control: public, max-age=300`; a response to a signed-in session is
`private, no-cache`, since it ran with that viewer's access.

An item's title, summary and body are not hashed directly — they can only
change through an edit to the item, the same write that bumps `changed` — but
an author's account name and a term's label are edited on a different row, so
they are hashed explicitly: the account name actually fetched for the entry
(already loaded for `dc:creator` / `<author><name>`), and each term the entry
prints, by id and label. The labels are read in one query for the whole
response, before the `304` check, never one per entry or per term. So a
renamed account or a relabelled term invalidates the `ETag` of exactly the
feeds that print it, at once, rather than waiting for the entry's next edit.

#### A feed per author, per tag, per anything the gather takes

A feed path may carry one argument, as a whole segment before the last:
`/blog/author/{user}/feed.xml`, `/tag/{term}/rss.xml`. The argument is bound
exactly the way the gather's own page binds it: by the `display.routes`
parameter mapping whose `segment` has the same name. There is no separate
argument config for feeds, and the lookup is the same code, so `tag_category`
resolves a tag slug, `user: true` resolves an account name, and anything else
is passed through.

`paths` lists more addresses for the same feed, so one declaration serves the
whole listing and each of its arguments. The stock blog gather
(`trovato_blog`, migration `002_author_and_tag_feeds.sql`) is the worked
example:

```json
"routes": [
    {"path": "/blog/author/{user}", "params": [{"segment": "user", "param": "author", "user": true}]},
    {"path": "/blog/tag/{tag}", "params": [{"segment": "tag", "param": "tag", "tag_category": "tags"}]}
],
"feed": {
    "path": "/blog/feed.xml",
    "paths": ["/blog/author/{user}/feed.xml", "/blog/tag/{tag}/feed.xml"]
}
```

with the definition filtering `author_id` on `{"url_arg": "author"}` and
`fields.field_tags` (`has_tag`) on `{"url_arg": "tag"}`. That gives
`/blog/feed.xml`, `/blog/author/jeremy/feed.xml` and `/blog/tag/rust/feed.xml`,
each with its `.atom` sibling.

An argument that names nothing answers 404, not an empty feed: an unknown tag
slug or account name fails the lookup, and a passed-through value that matches
no row the viewer can see is treated the same way. A feed with an argument is
not in every page's head; the page reached with the same argument
(`/blog/author/jeremy`) advertises its own (`/blog/author/jeremy/feed.xml`).
A path with two arguments, an argument inside a segment (`/rss/{slug}.xml`),
or an argument no route binds is skipped at startup with a warning.

---

## The Tap System

Taps are the primary way plugins interact with the kernel. Each tap is a named hook that plugins can implement.

### Implementing a Tap

Use the `#[plugin_tap]` macro to mark a function as a tap implementation:

```rust
use trovato_sdk::prelude::*;

#[plugin_tap]
fn tap_item_info() -> Vec<ContentTypeDefinition> {
    // Return content type definitions
    vec![]
}
```

For taps that can fail, use `#[plugin_tap_result]`:

```rust
#[plugin_tap_result]
fn tap_cron(_input: serde_json::Value) -> Result<(), String> {
    // An `Err` is logged by the kernel with the plugin and tap named.
    Ok(())
}
```

### Tap and route size limit

A tap or a plugin-served route (`tap_api`) may send and receive up to
**8 MiB** each way — the input the kernel hands it and the JSON (or, for
`tap_api`, the body) it returns. Both directions share one ceiling,
`trovato_sdk::abi::PLUGIN_IO_MAX_BYTES`. Send or return more than that and the
kernel refuses the call with a documented error rather than truncating it
silently.

8 MiB is comfortably above any real page — the motivating case was a 54 KB
field the kernel's old 64 KB cap already refused to render — and well inside
the 64 MiB a plugin may use in total for its own code, data and working set in
one call, so a call that uses the whole ceiling still leaves room to run.
There is nothing to opt into: every tap and `tap_api` route gets the same
limit, and a plugin built against the current SDK gets it automatically (see
`crates/plugin-sdk/src/abi.rs` for plugins built before this limit existed,
which keep working at the old 64 KB ceiling until rebuilt).

If a tap legitimately needs to send more than 8 MiB — a large export, for
instance — paginate it across multiple calls rather than asking for a larger
limit; nothing in the protocol changes to accommodate one call that large.

### Available Taps

#### Content Type Definition

| Tap | Input | Output | Description |
|-----|-------|--------|-------------|
| `tap_item_info` | None | `Vec<ContentTypeDefinition>` | Register content types and fields |

#### Item Lifecycle

| Tap | Input | Output | Description |
|-----|-------|--------|-------------|
| `tap_item_view` | `ItemViewInput` | `RenderElement` | Render item content |
| `tap_item_view_alter` | `ItemViewAlterInput` | `RenderElement` | Modify rendered output |
| `tap_item_presave` | `{item_type, title, fields, status}` | `{"fields": {...}}` or `{}` | Before a create or update is written: the one tap that can change a write. Returned fields are merged into the item, last plugin wins per field |
| `tap_item_insert` | the created item | ignored | Notification after the row exists. It cannot refuse the write, and what it returns, an error included, is logged at most and never read |
| `tap_item_update` | the updated item | ignored | Notification after the update, as `tap_item_insert` |
| `tap_item_update_index` | the saved item | ignored | Notification after a create or update, for a plugin that keeps its own index |
| `tap_item_delete` | the item being deleted | ignored | Notification before the row is removed. It cannot abort the delete |
| `tap_item_access` | `ItemAccessInput` | `AccessResult` | Control item visibility |

#### Forms

| Tap | Input | Output | Description |
|-----|-------|--------|-------------|
| `tap_form_alter` | the form as JSON | the form as JSON | Change a form before it is rendered |
| `tap_form_validate` | `{form_id, values}` | `{}` or `{"errors": [...]}` | Refuse a submission, on a field or the whole form |
| `tap_form_submit` | `{form_id, values, item_id}` | ignored | Notification after a successful save, once |
| `tap_form_ajax` | `{trigger, form_id, values}` | `{"commands": [...]}` or `{}` | Answer an AJAX trigger the kernel does not handle itself |

Every content and profile form goes through these taps: `item_add_{type}` and
`item_edit_{type}` (both on `/item/...` and on `/admin/content/...`), and
`user_profile_form`.

**`tap_form_alter`** receives the whole form and returns it. Plugins run in
weight order and each receives what the previous one returned, so alterations
compose. The form is `{form_id, action, method, elements, libraries, ...}`;
`elements` is an object keyed by input name, and each element has a `type`
(`textfield`, `textarea`, `select`, `checkbox`, `hidden`, `markup`, `input`,
`widget`, `container`, ...), `weight`, and optional `title`, `description`,
`default_value`, `prefix`, `suffix`, `attributes`, `children`. To add a script
or stylesheet, push the name of a kernel asset library into `libraries`
(`ai_assist`, `block_editor`, `compound_field`, `file_upload`, `record_ref`);
the renderer attaches it as a file, so no inline script is needed and the page's
CSP does not change. `form_id`, `form_build_id` and the CSRF token are the
kernel's and are kept whatever a plugin returns. An input a plugin adds is
submitted and handed to `tap_form_validate`, but a content form only stores the
item's own declared fields, so store anything else from `tap_form_submit`.

```rust
#[plugin_tap]
pub fn tap_form_alter(mut form: serde_json::Value) -> serde_json::Value {
    if form["form_id"] != "item_edit_article" {
        return form;
    }
    form["elements"]["my_notice"] = serde_json::json!({
        "type": "markup", "value": "<p>Reviewed weekly.</p>", "weight": 990,
    });
    form
}
```

Declare the form taps with `serde_json::Value` (or a typed struct) in and out,
never `String`: `#[plugin_tap]` deserializes the input as the parameter's type
and serializes the return value, so a `String` parameter expects a JSON string
and refuses the object the kernel sends.

**`tap_form_validate`** receives `{form_id, values}`: the posted value of every
input the form rendered (after alteration), a multi value field as an array.
Form machinery, inputs the form did not render, and passwords are not passed.
Return `{}` to accept, or `{"errors": [...]}` where each error is either a
string (shown at the top of the form) or `{"field": "field_x", "message":
"..."}` (shown beside that input). Any error refuses the save; nothing is
written and the editor sees the form again with what they typed.

**`tap_form_ajax`** answers an element whose `ajax` callback names a trigger
the kernel does not handle (the kernel's own is `add_{field}`, "Add another").
`POST /system/ajax` checks the CSRF header, that the form was built for this
user and that they may still open it (edit access to the item, or the type's
create permission), then dispatches with the full host API, so a handler can
read its tables. `values` are the form's current values. Return AJAX commands
(`replace`, `append`, `remove`, `add_class`, ... each with a `selector`) or
`{}`; every handler's commands are applied in order.

**`tap_form_submit`** receives `{form_id, values, item_id}` after the save
succeeded, once per save. `item_id` is the item created or updated, or `null`
for the profile form. Its return value is ignored. It cannot refuse the save;
use `tap_form_validate` for that.

#### System

| Tap | Input | Output | Description |
|-----|-------|--------|-------------|
| `tap_menu` | None | `Vec<MenuDefinition>` | Register routes |
| `tap_perm` | None | `Vec<PermissionDefinition>` | Define permissions |
| `tap_admin_nav` | None | `Vec<AdminNavDefinition>` | Admin sidebar links and dashboard cards ([below](#admin-sidebar-links-and-dashboard-cards)) |
| `tap_cron` | None | `Result<(), String>` | Background tasks |
| `tap_queue_info` | None | `[{name, concurrency}]` | Declare owned queues + worker concurrency ([queue docs](plugin-queue.md)) |
| `tap_queue_worker` | Job payload | Any (return) / trap (fail) | Process one queued job — **must be idempotent** ([queue docs](plugin-queue.md)) |
| `tap_install` | None | `Result<(), String>` | First-time setup |
| `tap_enable` | None | `Result<(), String>` | On plugin enable |
| `tap_disable` | None | `Result<(), String>` | On plugin disable |

---

## Content Types and Fields

### Defining a Content Type

```rust
#[plugin_tap]
fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "article".to_string(),
            label: "Article".to_string(),
            description: "News articles and blog posts".to_string(),
            fields: vec![
                FieldDefinition::new("body", FieldType::TextLong)
                    .label("Body")
                    .required(),
                FieldDefinition::new("summary", FieldType::Text { max_length: Some(500) })
                    .label("Summary"),
                FieldDefinition::new("tags", FieldType::RecordReference("category_term".into()))
                    .label("Tags")
                    .cardinality(-1),  // Unlimited
                FieldDefinition::new("featured", FieldType::Boolean)
                    .label("Featured"),
            ],
        }
    ]
}
```

### Field Types

| Type | Rust Definition | Description |
|------|-----------------|-------------|
| Text | `FieldType::Text { max_length: Option<usize> }` | Single-line text |
| TextLong | `FieldType::TextLong` | Multi-line text with format |
| Integer | `FieldType::Integer` | Whole numbers |
| Float | `FieldType::Float` | Decimal numbers |
| Boolean | `FieldType::Boolean` | True/false |
| Date | `FieldType::Date` | Date value |
| Email | `FieldType::Email` | Email address |
| File | `FieldType::File` | File upload |
| Reference | `FieldType::RecordReference(target_type)` | Reference to another item, of one content type |
| Category reference | `FieldType::CategoryReference(category_id)` | Tags of one category: a JSON array of tag uuid strings, whatever the cardinality. Every write refuses a value that is not a tag of that category. Added in B72 (0.107.0). `FieldType` is `#[non_exhaustive]`, so a plugin that matches on it needs a `_` arm |

### Field Options

Every field definition, in Rust or in an `item_type.*.yml` config file, takes
these options besides its name, type and label:

| Option | Default | Meaning |
|--------|---------|---------|
| `required` | `false` | The form refuses a save without a value |
| `cardinality` | `1` | How many values: `1` single, `-1` unlimited, `n` at most `n`. The form shows each stored value plus one empty slot and an "Add another" button. Every write path refuses more than `n` (the item form, the item API, `save-item`, config import): a JSON array counts its elements, any other value counts as one |
| `settings` | `{}` | Type specific settings, and `visible_when` (below) |
| `personal_data` | `false` | Included in data exports and anonymised on account deletion |

`settings.visible_when: {field, equals}` shows the field only while `field`
(in the same type) has the value `equals`. It lives in `settings` so that a
definition without it, and a plugin built against an earlier SDK, are
unaffected. It compares the controlling field's value as the form posts it:
the option key of a select, the text of a text field, `"1"` for a checked
Boolean and `""` for an unchecked one. While it is false the form hides the
field, a save keeps the field's stored value whatever was posted for it, and
`required` is not checked. The form shows and hides it as the editor types
(`static/js/form-states.js`); without JavaScript every field starts as the
stored values say and the server applies the rule on save.

```rust
FieldDefinition::new("field_stream_url", FieldType::Text { max_length: None })
    .label("Stream URL")
    .required()
    .visible_when("field_online", "1")
```

```yaml
    - field_name: field_stream_url
      field_type:
        Text:
          max_length: null
      label: Stream URL
      required: true
      settings:
        visible_when:
          field: field_online
          equals: "1"
```

### Working with Items

```rust
#[plugin_tap]
fn tap_item_view(input: ItemViewInput) -> RenderElement {
    let item = &input.item;

    // Get typed field values
    let body: Option<TextValue> = item.get_text_value("body");
    let tags: Option<Vec<RecordRef>> = item.get_field("tags");
    let featured: Option<bool> = item.get_field("featured");

    // Build render output
    render::container()
        .class("article")
        .child("title", render::markup("h1", &item.title).build())
        .child("body", render::filtered_markup(
            &body.map(|b| b.value).unwrap_or_default(),
            &body.map(|b| b.format).unwrap_or_else(|| "plain_text".into())
        ).build())
        .build()
}
```

### Item Structure

```rust
/// SYNC: field names and types must match `crates/kernel/src/models/item.rs`.
pub struct Item {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub item_type: String,
    pub title: String,
    #[serde(default)]
    pub fields: HashMap<String, Value>,
    pub status: i32,              // 0 = unpublished, 1 = published
    pub author_id: Uuid,
    pub current_revision_id: Option<Uuid>,
    pub stage_id: Uuid,           // defaults to live stage UUID
    pub created: i64,             // Unix timestamp
    pub changed: i64,
}
```

---

## Rendering Output

Plugins return `RenderElement` trees that the kernel converts to HTML.

### Element Types

| Type | Description | Key Properties |
|------|-------------|----------------|
| `container` | Wrapper element | Children |
| `markup` | HTML content | `#value`, `#tag` |
| `table` | Data table | Rows, headers |
| `list` | Ordered/unordered list | Items |
| `link` | Anchor element | `href`, text |

### Using the Builder API

```rust
use trovato_sdk::render;

// Container with children
let element = render::container()
    .class("my-component")
    .attr("data-id", "123")
    .child("header", render::markup("h2", "Title").weight(-10).build())
    .child("content", render::markup("div", "Body text").build())
    .build();

// Filtered HTML (sanitized)
let body = render::filtered_markup(&html_content, "filtered_html")
    .class("content")
    .build();

// Links
let link = render::link("/path/to/page", "Click here")
    .class("button")
    .build();
```

### Weight-Based Ordering

Children are rendered in weight order (lower first):

```rust
render::container()
    .child("footer", render::markup("footer", "...").weight(100).build())
    .child("header", render::markup("header", "...").weight(-100).build())
    .child("main", render::markup("main", "...").weight(0).build())
    .build()
// Renders: header, main, footer
```

### RenderElement JSON Structure

Internally, RenderElements are JSON with `#`-prefixed metadata:

```json
{
    "#type": "container",
    "#attributes": {"class": "article"},
    "title": {
        "#type": "markup",
        "#tag": "h1",
        "#value": "My Title",
        "#weight": -10
    },
    "body": {
        "#type": "markup",
        "#tag": "div",
        "#value": "<p>Content here</p>",
        "#format": "filtered_html"
    }
}
```

---

## Host Functions

Plugins access kernel services through host functions.

### Logging

```rust
use trovato_sdk::host;

host::log(LogLevel::Info, "my_plugin", "Processing item");
host::log(LogLevel::Error, "my_plugin", "Something went wrong");
```

### Persistent Variables

Store configuration that persists across requests:

```rust
// Get with default
let value = host::variable::get("my_plugin_setting", "default_value");

// Set value
host::variable::set("my_plugin_setting", "new_value")?;
```

### Request Context

Share data within a single request:

```rust
// Set value for this request
host::context::set("my_key", "my_value");

// Get value (returns Option)
let value = host::context::get("my_key");
```

### Current User

```rust
// Get current user ID
let user_id = host::user::current_user_id();

// Check permission. This is the *effective* permission: a site administrator
// holds everything, so a plugin never needs an administrator case of its own.
// `administer site` is an ordinary permission (it gates the structure and
// configuration screens) and does not mean "is an administrator" — ask for the
// permission your plugin actually declared.
if host::user::has_permission("administer my_plugin") {
    // Logic for someone who may administer this plugin.
}
```

### Other Users

`host::load_users` answers public facts about users the plugin is not serving,
by id: name, status (`"active"` or `"blocked"`), timezone, language and when
the account was created. Never an address, a password hash or the user's
`data`; no capability grants an address. It needs `user-api` in
`host_interfaces` **and** `user_directory = true` under `[capabilities]`:
declaring `user-api` alone does not grant it, and the call is refused with
`ERR_USER_DIRECTORY_DENIED`.

```toml
[capabilities]
host_interfaces = ["user-api"]
user_directory = true
```

```rust
// Up to 100 ids a call, one database query for the whole batch. Ids that
// name no user are left out; a malformed id or more than 100 is refused.
let users = host::load_users(&["0190...", "0190..."])?;
for user in users.iter().filter(|u| u.status == "active") {
    // user.name, user.timezone, user.language, user.created
}
```

Look a digest's recipients up in batches of 100, not one at a time.

`host::find_user_by_name` and `host::find_users_by_name_prefix` answer the
same shape, by name instead of by id, so a plugin can resolve a name a person
typed to the account it names — the groups plugin's "add member by name"
could not do this before B179: it could only match names it had already seen
in its own tables, so an account created straight on the site answered "no
such user". Both need the same capability as `load_users`; no new manifest key.

```rust
// Exact match, case-insensitive the same way login matches a name.
match host::find_user_by_name("jsmith") {
    Ok(Some(user)) => { /* user.id, user.name, ... */ }
    Ok(None) => { /* no such user */ }
    Err(code) => { /* refused; see plugin-error-codes.md */ }
}

// A short, sorted list for an autocomplete widget as the plugin's user
// types; at most 10 results, not caller-configurable.
let matches = host::find_users_by_name_prefix("js")?;
```

Neither call filters a blocked account or the anonymous account out of its
answer: the visibility rule is the one `load_users` already has — nothing
beyond the `user_directory` capability gate itself, the same rule the
kernel's own `/admin/people` listing uses for an administrator. A plugin that
only wants to offer active accounts checks `user.status == "active"` itself,
the same way the `load_users` example above does.

ABI: `load-users` is in the existing `user-api` interface, with a new
manifest capability, `user_directory` (B73, after 0.104.0). `find-user-by-name`
and `find-users-by-name-prefix` are two more functions in the same interface,
sharing that capability (B179, after 0.107.0). No existing signature changed.

### Mailing a Member

`host::mail_send_to_user` sends a plain text message to one of the site's
members, by user id. The kernel looks the address up: the plugin never sees
it, in a return value, an error or a log line. It needs `mail` in
`host_interfaces` **and** `mail_users = true` under `[capabilities]`;
declaring `mail` alone still means the site contact address only, and this
call is refused with `ERR_MAIL_USERS_DENIED`.

```toml
[capabilities]
host_interfaces = ["mail"]
mail_users = true
```

```rust
match host::mail_send_to_user(&user_id, "Your weekly digest", &body) {
    Ok(()) => {}
    // The member unsubscribed from this plugin's mail: do not retry.
    Err(trovato_sdk::host_errors::ERR_MAIL_OPTED_OUT) => {}
    // Unknown, blocked or no address: do not retry either.
    Err(trovato_sdk::host_errors::ERR_MAIL_RECIPIENT_UNAVAILABLE) => {}
    // A bucket is spent: try again later.
    Err(trovato_sdk::host_errors::ERR_MAIL_RATE_LIMITED) => {}
    Err(code) => return Err(format!("mail failed: {code}")),
}
```

Every message goes from the site's `from` address and ends with a footer
linking to `/user/mail/unsubscribe/{token}`; the same link is in its
`List-Unsubscribe` header, with `List-Unsubscribe-Post:
List-Unsubscribe=One-Click`, so a mail client's unsubscribe button works. The
opt out is per member and per plugin. A member sees what they turned off on
their account page and can resubscribe there; the plugin is only ever told
`ERR_MAIL_OPTED_OUT`.

Two buckets apply, and a refusal from either is `ERR_MAIL_RATE_LIMITED`:
`mail_users`, 1000 messages an hour per plugin, and `mail_recipient`, 10 an
hour per plugin and member.

**Many recipients: enqueue one job per recipient.** Do not loop over members
inside `tap_cron`. Every plugin's `tap_cron` shares one dispatch that the
kernel stops after 150 seconds, and each message is an SMTP round trip, so a
long loop is cut off partway with no record of who was mailed. Instead,
`host::queue_push` one job per recipient from `tap_cron` and send in
`tap_queue_worker`. A failure retries that one recipient, not the whole
list, and the work spreads over as many cron cycles as it needs. Make the
worker idempotent, as every queue worker must be. Look recipients up with `host::load_users` in batches of
100 first if the plugin needs their names or languages.

ABI: a new function, `send-to-user`, in the existing `mail` interface and a
new manifest capability, `mail_users` (B73, after 0.104.0). No existing
signature changed.

### Item Operations

Declare `item-api` in `host_interfaces`. Every call acts as the user the plugin
is handling a request for, with that user's answer; from cron or a queue worker
the plugin needs `item_background = true`, and then acts with kernel authority.

```rust
use trovato_sdk::host;
use serde_json::json;

// Load: `None` when the item is missing or the user may not view it.
let item = host::get_item(&id)?;

// Create (needs `type`) or update (a non-nil `id`).
let created = host::save_item(&json!({ "type": "page", "title": "Hello", "language": "en" }))?;
let updated = host::save_item(&json!({ "id": id, "title": "Hello again" }))?;

// Move to a stage by machine name, by the editorial workflow's rule: an
// unknown name is `ERR_PARAM_DESERIALIZE`, a move the user may not make is
// `ERR_ITEM_ACCESS_DENIED`. Without `stage`, a create lands on the default
// stage and an update stays where it is.
let moved = host::save_item(&json!({ "id": id, "stage": "curated" }))?;

// Delete: `Err(ERR_ITEM_NOT_FOUND)` when no item has that id.
host::delete_item(&id)?;

// Load what the user may view, in full, on the request's stage.
let pages = host::query_items(&json!({ "type": "page", "limit": 20 }))?;

// Translations: read many items' at once, write one language at a time.
let translations = host::list_translations(&[&id_a, &id_b])?;   // ≤ 100 ids
let saved = host::save_translation(&json!({
    "item_id": id, "language": "it", "title": "Ciao", "fields": {"body": {"value": "…", "format": "plain_text"}}
}))?;
let removed = host::delete_translation(&id, "it")?;              // Ok(false): there was none
```

For a listing, use a query (below): `query_items` takes one type, one order and
no joins, and returns whole items rather than the columns a list shows.

A translation write is an edit of the item. It needs `edit` access, or
`translate content` and `view`, as the admin translation screen does. The
field write gate applies, `tap_item_update` fires on the other plugins, and
the item cache drops the item. A language the site does not have, or the item's
own language, is `ERR_PARAM_DESERIALIZE`. A plugin does not need
`item_translation` in `db_tables`, and should not declare it: a `db` write
there skips access, the taps, the cache and the menu label refresh.

A write is the same write a person makes on the edit form. It goes through the
kernel's item service, so the other plugins' `tap_item_presave`,
`tap_item_insert` or `tap_item_update`, `tap_item_update_index` and
`tap_item_delete` fire, the field and publish gates apply, a revision is
recorded, and the item's page shows the change at once.

A tap that handles one of those writes may write items too. The nested write
fires its taps on every plugin except those whose write tap is already running
around it, so a plugin never sees its own write come back into the handler that
made it, and a write nested four write taps deep is refused with
`ERR_ITEM_WRITE_DEPTH`. Every error code is listed in
[plugin-error-codes.md](plugin-error-codes.md#item-api-errors).

### Queries

Declare `query` in `host_interfaces`. `host::query` runs a Gather definition,
the JSON a site's gather is stored as, through the same pipeline as a gather
page, for the user the plugin is acting for:

```rust
use trovato_sdk::host;
use serde_json::json;

// The newest blog and news posts, each with the URL it is served at.
let page = host::query(
    &json!({
        "base_table": "item",
        "filters": [
            {"field": "type", "operator": "in", "value": ["blog", "news"]},
            {"field": "status", "operator": "equals", "value": 1}
        ],
        "fields": [{"field_name": "id"}, {"field_name": "title"}, {"field_name": "created"}],
        "sorts": [{"field": "created", "direction": "desc"}]
    }),
    &json!({"per_page": 10}),
)?;
for row in &page.items {
    let (title, url) = (&row["title"], &row["url"]); // url: the alias, or /item/{id}
}

// One field of one item, found by the address it is served at.
let doc = host::query(
    &json!({
        "base_table": "item",
        "item_type": "docs",
        "filters": [{"field": "url", "operator": "equals", "value": "/learn/install"}],
        "fields": [{"field_name": "fields.markdown", "label": "markdown"}]
    }),
    &json!({"per_page": 1}),
)?;
```

The kernel decides what the query sees, and the plugin cannot change it: an
item query reads only what the viewer may view, on the request's stages, in its
language, with the fields they may not see removed, and `stage_aware = false`
is refused. A query over one of the plugin's own tables (migration-owned or in
`db_tables`) reads it as it is. An item query may join the kernel's join tables
and the plugin's own tables; a query over the plugin's own table may not join
`item`. A page holds at most 100 rows (`has_next` says whether there is more),
and a definition carries at most 32 filters, 8 sorts and 3 relationships.
From cron or a queue worker an item query needs `item_background`. Every error
code is in [plugin-error-codes.md](plugin-error-codes.md#query-errors), and
the design is [plugin-query-surface.md](design/plugin-query-surface.md).

`filter_groups` add `or` (and nested `and`) to a definition, at most three deep.
Every grouped filter counts toward the 32. Over the plugin's own table,
`group_by`, `aggregates` (`count`, `sum`, `min`, `max`, `avg`), `having` and
`distinct` summarise rows; they are refused on content (-80), where they would
count rows the viewer may not see:

```rust
// Votes per item, for items with at least two.
let tally = host::query(
    &json!({
        "base_table": "my_votes",
        "group_by": ["item_id"],
        "aggregates": [{"function": "count", "label": "votes"},
                       {"function": "sum", "field": "score", "label": "total"}],
        "having": [{"field": "votes", "operator": "greater_or_equal", "value": 2}],
        "sorts": [{"field": "total", "direction": "desc"}]
    }),
    &json!({}),
)?;
```

Both are described in
[Design-Query-Engine.md](design/Design-Query-Engine.md#filter-groups-and-aggregates).

### Text Formats

Declare `text-format` in `host_interfaces`. **A plugin that renders HTML a user
wrote must pass it through `host::filter_text` before returning it.** The
kernel filters the fields it renders itself; it does not filter what a plugin
returns, so user HTML a plugin echoes unfiltered is a stored XSS.

```rust
use trovato_sdk::host;

// The same filter the kernel applies when it renders a field in that format.
let safe = host::filter_text(&comment_body, "filtered_html")?;
```

The format must be one the user the plugin is acting for may use, by the rule
the item forms apply: `plain_text` always, and any other format with its
`use <format>` permission (`use filtered_html`, `use full_html`, or that of a
format the site added at `/admin/config/formats`). Anything else is an error,
never the text unfiltered: `ERR_TEXT_FORMAT_NOT_PERMITTED` (-70) for a format
the user may not use, `ERR_TEXT_FORMAT_UNKNOWN` (-71) for one that does not
exist, `ERR_TEXT_FORMAT_TOO_LARGE` (-72) when the result is larger than the
output buffer. Filter with the format the text was written in, and when the
author's format is not one the viewer's request may use, fall back to
`plain_text`, which is always allowed and escapes everything.

---

## Access Control

### Implementing Access Control

```rust
#[plugin_tap]
fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    // ItemAccessInput provides lightweight access fields (not the full Item):
    //   item_id, item_type, author_id, operation, user_id

    // Only handle our own content types
    if input.item_type != "my_plugin_type" {
        return AccessResult::Neutral;
    }

    // Example: author can always access their own items
    if input.user_id == input.author_id {
        return AccessResult::Grant;
    }

    // Defer to kernel's permission fallback ("{operation} {type} content")
    AccessResult::Neutral
}
```

**Note:** The kernel already handles published-item access (checking `"access content"` permission) and has a permission fallback that checks `"{operation} {type} content"`. Most plugins should return `Neutral` and rely on this built-in behavior. Only implement `tap_item_access` if you need custom logic beyond standard permission checks.

### AccessResult Values

| Value | Meaning |
|-------|---------|
| `Grant` | Explicitly allow access |
| `Deny` | Explicitly deny (wins over Grant) |
| `Neutral` | No opinion (let other plugins decide) |

**Aggregation rule:** Deny > Grant > Neutral. If all plugins return Neutral, the kernel falls back to checking the `"{operation} {type} content"` permission.

---

## Menus and Permissions

### Registering Routes

```rust
#[plugin_tap]
fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/blog", "Blog")
            .callback("blog_listing")
            .permission("access content"),
        MenuDefinition::new("/blog/{slug}", "View Post")
            .callback("blog_view")
            .permission("access content"),
        MenuDefinition::new("/admin/blog", "Manage Blog")
            .callback("blog_admin")
            .permission("administer blog")
            .parent("/admin"),
    ]
}
```

### Defining Permissions

```rust
#[plugin_tap]
fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new(
            "create blog content",
            "Allows users to create new blog posts"
        ),
        PermissionDefinition::new(
            "edit own blog content",
            "Allows users to edit their own blog posts"
        ),
        PermissionDefinition::new(
            "administer blog",
            "Full administrative access to blog settings"
        ),
    ]
}
```

For plugins with multiple content types, use the `crud_for_type` helper to generate
standard create/edit/delete permissions matching the kernel's fallback format:

```rust
#[plugin_tap]
fn tap_perm() -> Vec<PermissionDefinition> {
    let types = ["my_article", "my_comment"];
    types.iter().flat_map(|t| PermissionDefinition::crud_for_type(t)).collect()
}
```

### Admin sidebar links and dashboard cards

A plugin's own `/admin/...` page renders inside the kernel's `page--admin.html`,
with the same sidebar and dashboard every kernel admin page gets — filtered to
what the signed-in viewer may actually open. A plugin adds its own entry to
that sidebar and to the `/admin` dashboard through `tap_admin_nav`, the same
shape `tap_menu` and `tap_perm` already use: a declaration collected once at
boot, not queried per request.

```rust
#[plugin_tap]
fn tap_admin_nav() -> Vec<AdminNavDefinition> {
    vec![
        AdminNavDefinition::new(
            "Structure",             // section heading — joins an existing
                                      // one by this name, or starts a new one
            "Groups",                // link text, and the card's title
            "/admin/groups",         // must start with /admin
            "administer groups",     // permission that gates the link and card
        )
        .card("Manage user groups."), // optional: also show a dashboard card
    ]
}
```

`permission` is display filtering only, exactly like every kernel sidebar
link: a viewer who does not hold it does not see the link or the card, but
the route itself still has to check the permission on every request — the
sidebar never substitutes for that check. Gate the two on the same
permission, as above, so they cannot disagree about who may reach the screen.

A disabled plugin contributes no link and no card. The declaration is read
when the server starts, from the plugins enabled then, so enabling or
disabling a plugin (from `/admin/plugins` or `trovato plugin enable|disable`)
changes its link and card at the next restart, the same moment its routes
appear or go. The permission, by contrast, is checked against the viewer on
every request, exactly as for the kernel's own links.

`href` must be the plugin's own path below `/admin/` (`/admin/groups`, not
`/administrator` or `/admin/../groups`: no empty, `.` or `..` segments, encoded or
not, and no backslash), and distinct from every kernel link, every prefix a
kernel link already highlights for (Categories owns `/admin/structure/tags`),
and every other plugin's declared href. A declaration that breaks any of these
is dropped (logged, not fatal) rather than sending the viewer somewhere else or
shadowing an existing screen.

For the admin page itself to pick up the kernel's chrome, serve it from a
`tap_api` route under `/admin` and answer with `ApiResponse::themed(title,
body)`, the same way any themed plugin page does. Gate the route on the same
permission the link declares:

```rust
#[plugin_tap]
fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("GET", "/admin/groups", "groups_admin")
            .title("Groups")
            .permission("administer groups"),
    ]
}

#[plugin_tap]
fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "groups_admin" => ApiResponse::themed("Groups", "<p>…manage groups here…</p>"),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}
```

List the tap in the plugin's info file next to the others, or the kernel never
calls it:

```toml
[taps]
implements = ["tap_perm", "tap_menu", "tap_api", "tap_admin_nav"]
```

A page is an admin page when its path starts with `/admin`
(`ThemeEngine::is_admin_path`, the rule the kernel already uses to pick
`page--admin.html` and to theme an admin error page). Any such page, kernel or
plugin, gets the sidebar; a plugin page elsewhere does not.

---

### Serving a Request Directly (`tap_api`), and Accepting a File

A `MenuRoute` with `handler_type = "api"` and a `callback` is not a page the
theme renders — it is a request the kernel dispatches straight to your
plugin's `tap_api`, with the method, path, query, params, body and caller
already resolved:

```rust
#[plugin_tap]
fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("POST", "/my-plugin/upload", "upload_logo")
            .title("Upload a logo")
            .permission("create my_article content"),
    ]
}

#[plugin_tap]
fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "upload_logo" => upload_logo(&request),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}
```

CSRF is already enforced before your tap runs: a state-changing method needs
an `X-CSRF-Token` header or a `_token` field (multipart or form-urlencoded),
unless the caller authenticated with an `Authorization: Bearer` token. The
body size cap (`MAX_REQUEST_BODY`, the 8 MiB plugin I/O ceiling) applies to
this case exactly as it always has, because `request.body` still crosses the
WASM boundary.

**A file in the form is different.** When the request's `Content-Type` is
`multipart/form-data`, the kernel parses it itself before dispatch: every part
carrying a filename is stored through the same `FileService` `/file/upload`
uses — the same size limit, MIME allowlist and magic-byte check — and handed
to you in `request.files: Vec<UploadedFile>` as an id and metadata, never raw
bytes. The form's other fields arrive in `request.body`, re-encoded as a JSON
object, so `request.json::<T>()` works the same whether the caller posted JSON
or a multipart form.

```rust
fn upload_logo(request: &ApiRequest) -> ApiResponse {
    let Some(upload) = request.files.first() else {
        return ApiResponse::error(400, "no file uploaded");
    };
    // The id is what a File field holds. Attaching it through `save-item` is
    // what promotes the file out of `Temporary` — do not touch file storage
    // yourself, there is no host call for it and this is why.
    match host::save_item(&serde_json::json!({
        "id": request.params.get("id"),
        "fields": { "field_logo": upload.id },
    })) {
        Ok(Some(item)) => ApiResponse::json(&serde_json::json!({ "item_id": item.id })).unwrap(),
        Ok(None) => ApiResponse::error(404, "item not found"),
        Err(code) => ApiResponse::error(500, &format!("save failed: {code}")),
    }
}
```

A file your route stores but never references from an item stays `Temporary`
and is removed by the kernel's temporary-file cleanup — a plugin cannot leak
storage by accepting an upload nobody attaches.

---

## Database Operations

The `db` calls work on the plugin's own tables: the ones its migrations
create, and any it lists in `db_tables`. Each call is checked against that
allowlist, and the kernel's protected tables are refused whatever the
manifest says (`-16`). Content does not go through `db`. Read it with
[Queries](#queries), and write it with `host::save_item` and
`host::save_translation`, which decide access, fire the item taps and keep the
item cache right. A `db` write to `item` or `item_translation` would do none
of that.

### Structured Calls

```rust
use trovato_sdk::host::db;
use serde_json::json;

// Read: `where` is a map of equalities here.
let rows = db::select(&json!({
    "table": "my_votes",
    "columns": ["item_id", "score"],
    "where": {"voter": voter},
    "order": [{"column": "score", "direction": "desc"}],
    "limit": 10
}))?;

// Insert one row; the row comes back.
let row = db::insert("my_votes", &json!({"item_id": id, "voter": voter, "score": 1}))?;

// Update and delete take a map, or an array of conditions, ANDed.
let changed = db::update(
    "my_votes",
    &json!({"score": 0}),
    &json!([{"field": "voter", "op": "=", "value": voter},
            {"field": "score", "op": ">", "value": 5}]),
)?;
let removed = db::delete("my_votes", &json!([{"field": "voter", "op": "in", "value": ["a", "b"]}]))?;

// Insert, or update on a conflict over the key; the row comes back.
let row = db::upsert(
    "my_votes",
    &json!({"item_id": id, "voter": voter, "score": 3}),
    &json!({"target": ["item_id", "voter"]}),   // "update": [cols] to narrow the set
)?;

// Up to 500 rows in one statement: all or none (-82 over the bound).
let written = db::insert_many("my_votes", &rows_to_write)?;
```

A condition's `op` is one of `=`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `not_in`,
`is_null` or `is_not_null`. An empty `where`, an unknown op or an empty `in`
list is refused with `-14`, so none of them can turn into "every row". Values
bind as their JSON type, and a string binds as text. To compare a `uuid`
column, store the key as text or filter on it in a [query](#queries).

To count, sum or group your own rows, use `host::query` with `group_by` and
`aggregates` ([Queries](#queries)) rather than raw SQL.

### Raw SQL (Requires `raw_sql`)

`raw_sql = true` under `[capabilities]` lets a plugin read any table outside
the protected floor, with no viewer, stage or language. Keep it for what the
structured calls and `host::query` cannot express: recursive CTEs, window
functions, extension operators, locking reads.

The admin plugin listing marks every plugin holding it and boot logs one line
naming them all; [plugin-capabilities.md](plugin-capabilities.md#raw_sql) is
what to tell an administrator the grant means.

```rust
let rows = host::query_raw("SELECT * FROM my_table WHERE name = $1", &[json!("Example")])?;
let affected = host::execute_raw(
    "UPDATE my_table SET counter = counter + 1 WHERE id = $1",
    &[json!(id)],
)?;
```

### The static search index: queue what you change

The site's static search index (Pagefind, under `/pagefind/`) is updated item by
item. Each change to what an item's search page is built from queues that item,
and the next cron run rewrites only the queued items' part of the index.
Nothing polls for changes, so a change that queues nothing is never seen.

**Writes to items, comments and URL aliases are queued by the database.**
Triggers on the `item`, `comment` and `url_alias` tables queue the item a write
changes, in the write's own transaction. That holds for `host::save_item`, the
kernel's services and a plugin's raw SQL alike. A write that commits is
queued, and one that rolls back is not.

```rust
host::execute_raw(
    "UPDATE item SET fields = jsonb_set(fields, '{field_rating}', $1) WHERE id = $2::uuid",
    &[json!(rating), json!(id)],
)?;
// Queued already: nothing more to do.
```

**The rule:** when your plugin changes something else that decides what an
item's search page holds, call `host::search_index_enqueue` afterwards with the
ids of the items it affects. The usual case is data of your own that your
`tap_field_access` reads: the index is built as the anonymous visitor, so a
change to which fields that visitor may see changes the page.

```rust
host::execute_raw("UPDATE my_plugin_field_embargo SET lifted = true WHERE item_id = $1::uuid", &[json!(id)])?;
host::search_index_enqueue(&[id.as_str()])?;   // Ok(rows queued)
```

* Queuing twice is harmless. Queuing an item that is not searchable (a draft, a
  workspace item, one anonymous visitors cannot see) is also harmless: the
  updater takes it out of the index or leaves it out.
* It needs the `db` host interface and takes up to 10,000 ids per call. An id
  that is not a UUID is `-14` (`ERR_PARAM_DESERIALIZE`), and more than 10,000
  ids is `-82` (`ERR_DB_BATCH_TOO_LARGE`).
* The queue row records the reason as `plugin:<your plugin name>`, so an
  operator can see who queued what.

Without the call, such a change reaches search only at the next full rebuild.
That happens after compaction or when an operator runs `trovato search
rebuild`, which may be days away.

---

## Caching

### Cache Operations

```rust
// Get cached value
if let Some(cached) = host::cache::get("my_bin", "my_key") {
    return Ok(cached);
}

// Compute and cache
let result = expensive_computation();
host::cache::set("my_bin", "my_key", &result, json!(["tag:items", "tag:blog"]));

// Invalidate by tag
host::cache::invalidate_tag("tag:blog");
```

### Cache Tags

Use tags to group related cache entries for bulk invalidation:

```rust
// Cache with multiple tags
host::cache::set("views", "blog_listing", &html, json!([
    "tag:items",
    "tag:blog",
    "tag:listing"
]));

// When a blog post is updated, invalidate all related caches
host::cache::invalidate_tag("tag:blog");
```

---

## Inter-Plugin Communication

### Calling Another Plugin

```rust
// Check if plugin exists
if host::plugin::exists("other_plugin") {
    // Invoke a function
    let result = host::plugin::invoke(
        "other_plugin",
        "some_function",
        json!({"key": "value"})
    )?;
}
```

---

## Letting an assistant configure your plugin

A person can configure your plugin by talking to it. You declare what is
configurable, describe the thing being configured, and answer the model's tool
calls; the kernel runs the conversation, and **every change the model wants to
make becomes a proposal a person has to apply**.

That last part is the whole design. A write tool is never executed because a
model asked for it. The kernel calls it in `Describe` mode to find out what it
would do, records a proposal, and only calls it in `Execute` mode when somebody
clicks Apply on a card they have read. Your write tool has to honour that: in
`Describe` mode it must change nothing.

Three taps, all optional, all declared in your manifest:

```toml
[taps]
implements = ["tap_assistant_scopes", "tap_assistant_context", "tap_assistant_tool"]

[capabilities]
# `user-api` so a tool can check the caller's permission at the moment of the
# change, which is where the change happens.
host_interfaces = ["variables", "user-api", "logging"]
```

A tap the manifest does not list is never dispatched, so an exported tap missing
from `implements` is silently dead. The whole example below is
`plugins/test_assistant_scope`.

### 1. Declare what can be configured

`tap_assistant_scopes` runs once at startup, without services. Return a scope per
configurable thing:

```rust
#[plugin_tap]
pub fn tap_assistant_scopes() -> Vec<AssistantScope> {
    vec![
        AssistantScope::new(
            "test_widget",                 // machine name, [a-z0-9_]+, unique site-wide
            "Test widget",
            "configure test widget",       // the permission that opens it
            AssistantIdKind::String,       // Item, String or None
        )
        .description("Configure a test widget")
        .prompt("You configure a test widget.")
        .suggestions(["What colour is the widget?", "Make it teal"])
        .tool(AssistantTool::read("read_widget", "Read the widget's current colour."))
        .tool(
            AssistantTool::write(
                "set_widget_color",
                "Set the widget's colour.",
                AssistantRisk::Normal,
            )
            .parameters(serde_json::json!({
                "type": "object",
                "required": ["color"],
                "properties": {"color": {"type": "string"}}
            })),
        ),
    ]
}
```

`id_kind` decides what the `{scope_id}` in `/ai/assistant/{scope}/{scope_id}`
means. `Item` takes a Trovato item id and the kernel checks it exists and is one
of the types in `item_types` — and puts a launcher link on that item's page for
you. `String` takes any opaque string of at most 128 bytes. `None` is a site-wide
scope with no id.

The registry validates each scope and **drops an invalid one with a warning
rather than failing startup**: names must match `[a-z0-9_]+`, a scope name must
be unique across every plugin, `parameters` must be a JSON Schema object with
`"type": "object"`, and there are caps (32 tools, 6 suggestions, an 8 KiB
prompt). Dropped scopes are listed at `/admin/system/ai-assistant`, so check
there first if a scope does not appear.

### 2. Describe what is being configured

`tap_assistant_context` runs when a conversation opens or is reset, with services
and the caller's real permissions. The `snapshot` is the model's whole view of
your domain: write plain labelled lines, current and complete enough that most
questions need no tool call at all.

```rust
#[plugin_tap]
pub fn tap_assistant_context(request: AssistantContextRequest) -> AssistantContext {
    let id = request.scope_id.unwrap_or_default();
    AssistantContext::new(
        format!("Widget {id}"),
        format!("Widget {id} has color {}.", current_color()),
    )
    .link("View widget", format!("/widget/{id}"))
}
```

Keep the whole tap result under the 64 KiB tap output buffer — a larger one is
replaced with an error, not truncated. The kernel then truncates the snapshot
again to the site's configured cap, at a line boundary, and appends
`[snapshot truncated]`.

### 3. Answer tool calls

`tap_assistant_tool` runs with services and the caller's real permissions. It is
called for a read tool as soon as the model asks, and for a write tool twice:
once to describe, once to apply.

```rust
#[plugin_tap]
pub fn tap_assistant_tool(call: AssistantToolCall) -> AssistantToolResult {
    // The kernel checked the scope's permission before opening the
    // conversation. Check again here, because this is where the change happens.
    if !host::current_user_has_permission(PERM) {
        return AssistantToolResult::failed("You do not have permission to configure the widget.");
    }

    match call.tool.as_str() {
        "read_widget" => AssistantToolResult::data(
            serde_json::json!({"color": current_color()}).to_string(),
        ),
        "set_widget_color" => {
            let color = call.arguments["color"].as_str().unwrap_or_default();
            match call.mode {
                // Describe changes NOTHING. The summary is the proposal card.
                AssistantToolMode::Describe => AssistantToolResult::ok(
                    format!("Would set the widget colour to {color}."),
                    format!("Set widget color to {color}"),
                ),
                AssistantToolMode::Execute => {
                    host::variables_set("color", color).ok();
                    AssistantToolResult::ok(
                        format!("The widget colour is now {color}."),
                        format!("Widget color is now {color}"),
                    )
                }
            }
        }
        other => AssistantToolResult::failed(format!("no such tool: {other}")),
    }
}
```

`content` is what the model reads; `summary` is the one sentence a person reads
in the transcript, and a `Describe` **must** set it. The kernel truncates
`content` to the site's cap and appends `[result truncated]`.

A tool the scope did not declare is never dispatched. Arguments are checked
against your schema for required keys and declared scalar types before you see
them; anything deeper is yours to validate. A trap, a timeout or an unreadable
result becomes `ok: false` with a generic message to the model and the detail in
the log, so one broken tool does not end a conversation.

### Writing a good scope

- **Name a permission that means something.** It is the gate; an administrator
  passes it, and everybody else needs it plus `use ai` and `use ai assistant`.
- **Prefer reads that answer one question.** A model given one broad tool will
  call it repeatedly; a model given `device_history(days)` will ask for what it
  needs.
- **Make every Describe string name the thing and the change**, including the
  value it replaces: "Assign Amazon tablet (02:00:5e:00:00:04) to Jamie
  (currently Arlo)". That sentence is the entire basis on which a person decides.
- **Refuse in Execute, not only in Describe.** Time passes between the two.
- **Put your launcher where the thing is.** Include the kernel's partial with a
  literal scope from your own template:
  `{% include "assistant/launcher.html" %}` with `scope` (and `scope_id`) set.
  It renders nothing when the assistant is off.

---

## Owned vocabularies

A vocabulary (a category) normally belongs to the site: one list of terms,
administered under `/admin/structure/categories` by whoever holds `administer
categories`. A site plugin that models groups, clubs or projects can give each
of them **their own vocabularies**, administered by that owner's managers with
the kernel's own term list and term form (B108). A vocabulary with no owner
behaves exactly as before.

A plugin does three things.

### 1. Declare the kind of owner

In the manifest:

```toml
[[vocabulary_owners]]
owner_type = "group"                         # [a-z][a-z0-9_]*, at most 64
label = "Group"                              # what the screens call one owner
path = "/group/{owner}/categories"           # where the kernel serves them
permission = "administer group categories"   # optional, see below
```

`path` is three segments with `{owner}` in the middle. Its first segment may not
be one the kernel serves (`admin`, `api`, `user`, `item`, `static`, …); the
manifest is refused if it is. An owner type or path another enabled plugin
already declared is skipped with a warning (the first plugin by name keeps it).
Declare `permission` with `tap_perm` too if you name one.

The kernel mounts, under `path`:

| Route | |
|---|---|
| `GET /group/{owner}/categories` | the owner's vocabularies |
| `GET …/{vocabulary}` | the term list |
| `GET`, `POST …/{vocabulary}/add` | the add-term form |
| `GET`, `POST …/{vocabulary}/terms/{term}/edit` | the edit-term form |
| `POST …/{vocabulary}/terms/{term}/delete` | delete a term |

Link to them from your own pages; the kernel adds no menu entry.

### 2. Create the owner's vocabularies

With the `vocabulary` host interface (`host_interfaces = ["vocabulary", …]`),
usually when the owner is created:

```rust
use trovato_sdk::host;
use trovato_sdk::types::OwnedVocabularyRequest;

let request = OwnedVocabularyRequest::new("group", &group_id, format!("group_{group_id}_topics"), "Topics")
    .description("What this group writes about")
    .hierarchy(1);
let vocabulary = host::create_owned_vocabulary(&request)?;
let all = host::list_owned_vocabularies("group", &group_id)?;
```

The id is a site-wide machine name (lowercase letters, digits and underscores,
at most 32), so put the owner in it. A plugin may only create and list
vocabularies for owner types it declares (`-90`); a bad id or label is `-91`;
an id in use is `-92`.

### 3. Decide who administers an owner's terms

Implement `tap_vocabulary_owner_access` (list it under `[taps] implements`). The
kernel calls it, on the declaring plugin only, for every request to an owner
route by a signed-in user:

```rust
use trovato_sdk::types::{VocabularyOwnerAccess, VocabularyOwnerAccessRequest};

#[plugin_tap]
pub fn tap_vocabulary_owner_access(request: VocabularyOwnerAccessRequest) -> VocabularyOwnerAccess {
    // `owner_key` is the `{owner}` path segment: a slug, an id, whatever your links use.
    let Some(group) = find_group(&request.owner_key) else {
        return VocabularyOwnerAccess::not_found();
    };
    if is_manager(&group, &request.user_id) {
        VocabularyOwnerAccess::grant(group.id)
    } else {
        VocabularyOwnerAccess::neutral(group.id)
    }
}
```

The kernel decides:

- an anonymous user is sent to sign in, whatever the tap says;
- `not_found()` (no owner id) is a 404;
- `deny` refuses everyone but a superuser;
- `grant` admits;
- `neutral` admits a superuser, or a holder of the declared `permission`, and
  refuses everyone else.

A plugin that does not implement the tap treats the `{owner}` segment as the owner
id, and only the declared permission (or a superuser) admits.

A vocabulary, or a term, that belongs to another owner is a 404 under this
owner's route.

### Limiting a reference field to the item's owner

A term reference field (`FieldType::RecordReference("category_term")`) can be
limited to the vocabularies the item's owner owns, through a field setting:

```rust
let mut field = FieldDefinition::new("field_topics", FieldType::RecordReference("category_term".into()))
    .cardinality(-1)
    .label("Topics");
field.settings = serde_json::json!({
    "vocabulary_owner": { "owner_type": "group", "owner_field": "field_group" }
});
```

The item's owner is the value of `owner_field` on the same item: an id, or a
reference `{"target_id": …}`. Every save, whether from a form, the API or a
plugin, then refuses a term from any other vocabulary, a site-wide one included,
and any term at all on an item with no owner. A form shows the refusal at the
field; the API answers 422. A value the save does not change is not checked
again.

The field's autocomplete offers the same terms the save accepts: only the
vocabularies of the owner named in the form's `owner_field` (or stored on the
item, when the editor may not change that field). An unlimited term field offers
terms from site-wide vocabularies only. Either way, suggestions go only to a user
who may edit the field.

---

## Page builder components

A plugin can add components to the page builder (the `trovato_page_builder`
editor and the `PageBuilder` field type) without touching the kernel. A
component is three files in the plugin directory: a declaration, a Tera
template, and optionally a stylesheet. No WebAssembly is involved; the kernel
reads the files at boot.

```
plugins/my_site/
├── my_site.info.toml
├── my_site.wasm
├── components/
│   └── promo-cards.component.toml   # the declaration
├── templates/
│   └── pb/
│       └── promo-cards.html         # the template
└── static/
    └── promo-cards.css              # the stylesheet
```

The kernel's own components (Hero, Columns, Card Grid and the rest) are
declared the same way, in
`crates/kernel/src/content/page_builder_components/standard/`. They are the
best set of examples there is.

### How the registry is built

At boot the kernel builds one component registry: the standard set, then every
`components/*.component.toml` of every **enabled** plugin, plugins in name
order and files in name order. Everything reads from it:

- the editor, which builds its component list and every field form from
  `GET /api/v1/page-builder/components`;
- validation, which checks a page builder field on every create and update
  that goes through the item service (the item forms, the JSON API, plugins);
- rendering, which hands each component's template the fields it declares.

A file that does not parse, a field type the kernel does not know, a key a
field type does not take, a missing template or stylesheet, or a type name two
plugins both declare stops the server at boot, with an error naming the file
(and, for a duplicate, both plugins). A plugin may not replace a standard
component either: declare a new type instead.

Like the rest of a plugin, components are restart bound: enabling, disabling or
editing a plugin's components takes effect when the server restarts.

### Why TOML

The declaration is TOML, the format of `.info.toml`, rather than JSON or a
Rust tap: a plugin author already writes it, it allows comments (a component
file explains its choices), the kernel already parses it, and it is read with
unknown keys refused, so a misspelt `max_lenght` is an error at boot instead of
a limit that silently never applies. A declaration rather than a tap also means
the registry needs no plugin code to run before the first request, and the
editor gets the same schema the server validates with.

### The declaration

```toml
# components/promo-cards.component.toml
type = "PromoCards"            # unique across the site; PascalCase
label = "Promo cards"          # what the editor's component list shows
category = "marketing"         # the list's group; new names make new groups
description = "A heading over two to six cards."
template = "pb/promo-cards.html"     # optional; default pb/<kebab-case type>.html
stylesheets = ["promo-cards.css"]    # optional; paths under the plugin's static/
classes = ["promo-cards--two", "promo-cards--three"]  # every class a select may add
# zones = ["main"]             # optional: named areas other components drop into
# in_editor = false            # optional: validate and render it, but do not offer it

[[fields]]
name = "heading"
type = "text"
label = "Heading"
max_length = 80
default = "Why teams choose us"
```

Every field takes `name` (letters, digits and underscores, starting with a
letter; `id` is reserved), `type`, and optionally `label`, `help` (a line under
the field in the editor), `required`, `default` (what a newly dropped component
starts with; it is not applied at render time), `hidden` (kept and validated,
not shown in the editor) and `deprecated` (a sentence saying what replaced it;
implies the field is on its way out).

### Field types

Every rule below is checked on the server when the page is saved. A page that
breaks one is refused with an error naming the component and the field, for
example `field_layout.content[2].cards[1].title: is required`; the item form
shows it above the editor, and the JSON API returns it in a 422.

| Type | Stored as | Keys | Checked on save | The template receives |
|------|-----------|------|-----------------|-----------------------|
| `text` | string | `max_length`, `min_length`, `multiline` (a textarea in the editor) | length in characters | the string, autoescaped by Tera |
| `rich_text` | Markdown string | `max_length`, `min_heading` (default 2) | length of the Markdown | the string, and `<name>_html`: the kernel's sanitized rendering, headings shifted to start at `min_heading` |
| `number` | number | `min`, `max`, `integer` | range, whole number | the number |
| `heading_level` | 2 to 6 | none | 2 to 6 (the page title is the level 1 heading) | the number |
| `url` | string | none | relative, or `http`, `https`, `mailto`, `tel`; no whitespace | the URL, or nothing if unsafe |
| `image` | `{ src, alt, decorative }` | `alt_max_length` (default 250) | `src` is a safe URL; `alt` is required unless `decorative` is true (WCAG 1.1.1) | `{ src, alt, decorative }`, with `alt` empty when decorative |
| `link` | `{ url, label, new_tab }` | `label_max_length` (default 120) | `url` is safe; a link has a label a screen reader can announce | `{ url, label, new_tab }` |
| `select` | string | `options = [{ value, label, class }]` | the value is one of the options | the value; its `class`, if any, goes into `variant_classes` |
| `boolean` | `true` / `false` | none | is a boolean | the boolean |
| `list` | array of objects | `fields` (the entry's own fields, any type but `list`), `min_items`, `max_items`, `default_item` | count, and every entry's fields by their own rules | an array of entries, each normalized the same way |
| `reference` | string id | `target = "item"` or `"gather"`; `item_type` (item); `limit` (gather, default 10, at most 50) | an item UUID, or a gather query id | item: `{ id, title, type, url }`; gather: `{ id, label, url, total, rows }` |

A reference is resolved when the page is viewed, as the viewer: an item the
viewer may not see, an item of another type than `item_type`, a gather the
viewer may not run, or one that no longer exists is simply absent, and the
template renders without it.

### Templates

A component's template is found on the kernel's template search path, the
same one every page template uses. An enabled plugin's `templates/` directory
is on it at the lowest precedence, below the kernel's `templates/` and any
`TEMPLATES_DIR`, so a theme can restyle a plugin's component by shipping a
template of the same name, and a plugin cannot replace a kernel template. Name
yours after the component (`pb/promo-cards.html`), not after something generic.

The template's context holds:

- the component's **declared fields only**, each normalized to its type: a
  select value outside its options, an unsafe URL or a value of the wrong type
  is left out, so the template's `default(...)` applies. Keys the page carries
  that the declaration does not name never reach the template.
- `variant_classes`: the classes the chosen select options add, space
  separated. The kernel takes them from the option's `class`, and an option's
  `class` must be listed in the component's `classes`, or the server will not
  start.
- `zones`, for a component that declares any: each zone's children, already
  rendered.

Everything a template outputs goes through the page builder's HTML sanitizer.
It has **no `style` attribute**: a component expresses a colour, a spacing or a
layout as a declared class, never as inline CSS from a prop. `class`,
`aria-label`, `role` and `lang` survive on any element, along with ammonia's
default structural tags and attributes (`href`, `src`, `alt` and the like) and
`section`, `article`, `aside`, `nav`, `figure`, `details` and `summary`;
scripts, event handlers and `javascript:` URLs do not. `target="_blank"` is
the one `target` kept, and such a link gets `rel="noopener noreferrer"`.

```html+jinja
{#- templates/pb/promo-cards.html -#}
{% set hlevel = headingLevel | default(value=2) %}
<section class="promo-cards {{ variant_classes }}">
  {% if heading %}<h{{ hlevel }} class="promo-cards__heading">{{ heading }}</h{{ hlevel }}>{% endif %}
  {% for card in cards | default(value=[]) %}
  <article class="promo-cards__card">
    <h{{ hlevel + 1 }}>{{ card.title | default(value="") }}</h{{ hlevel + 1 }}>
    {# SAFE: text_html is the kernel's sanitized Markdown rendering of a rich_text field. #}
    {% if card.text_html %}{{ card.text_html | safe }}{% endif %}
    {% if card.link %}<a href="{{ card.link.url }}">{{ card.link.label }}</a>{% endif %}
  </article>
  {% endfor %}
</section>
```

The kernel's accessibility checks apply to plugin components too: an `image`
field without alt text that is not marked decorative is refused on save, and
the component renders as an accessibility error rather than as an unlabelled
image. A page whose headings skip a level is logged as a warning.

### Stylesheets

`stylesheets` lists CSS files under the plugin's `static/` directory. The
kernel serves an enabled plugin's `static/` at `/static/plugins/<plugin>/`,
and a page links each stylesheet in its `<head>` once, and only if a component
that declares it is on the page. The standard components' styles,
`/static/css/page-builder.css`, are linked the same way. The editor loads a
component's stylesheets into the canvas too, so the preview matches the page.

Write the CSS against your own classes, and set colours on the component's
own elements: the theme colours headings and paragraphs, and a heading on a
dark background will take the theme's dark colour unless your rule is at least
as specific (`.promo-cards .promo-cards__title`).

### In the editor

The editor needs no change for a new component. Each field type has its
editor control (a textarea for `rich_text` with a server-rendered preview, a
media library picker with an alt text box and a decorative checkbox for
`image`, a search box for `reference`, a sortable list for `list`), and the
canvas shows a plugin component by asking the kernel to render it
(`POST /api/v1/page-builder/preview`) with the same template, sanitizer and
checks as the page, so what the editor shows is what the page will show.

### A complete example

`plugins/test_page_builder_components` is a working plugin with two
components: `PromoCards` (a heading over a list of cards, each with an
eyebrow, a title, rich text and a link, and a select of how many cards to a
row) and `FeatureBanner` (a select of tone and image position, an image, a
title, rich text and a call to action). Its integration test,
`crates/kernel/tests/page_builder_components_test.rs`, shows the registry
listing them, a valid page saving and rendering, and an invalid one refused
with an error at each field. The same plugin declares the two root fields
above in `components/page.root.toml` and ships the footer call to action tile
template, and the same test file renders a page in each footer mode.

### Page root fields

A page builder page has one built in page level field, its `title`. A plugin
can declare more, for settings that belong to the page rather than to any one
component: whether the site's footer call to action shows on this page, a
page's accent, a campaign code. They are declared in the same field format as
component fields, edited in the editor's page settings form, checked on every
save by the same rules, and handed to page, region and tile templates.

#### The file

Root fields live in `components/<name>.root.toml`, beside the components. A
plugin may ship more than one such file; they are read in file name order
after its components. The file holds only what a root field needs: a list of
`[[fields]]` and, optionally, `classes`.

```toml
# components/page.root.toml
classes = ["page--quiet"]        # optional: every class a root select may add

[[fields]]
name = "footer_cta_mode"
type = "select"
label = "Footer call to action"
help = "Keep the site's call to action, hide it on this page, or replace it."
default = "default"
options = [
  { value = "default", label = "Site default" },
  { value = "hide", label = "Hide", class = "page--quiet" },
  { value = "custom", label = "Custom" },
]

[[fields]]
name = "footer_cta_content"
type = "rich_text"
label = "Custom call to action"
max_length = 280
```

It is a file of its own, not a `[page_root]` table in a component file,
because a root field belongs to no component: it has no type name, template,
category or stylesheet, a component file must have most of those, and a page
needs its root fields whether or not that component is on it. The file is the
component format with everything but the fields taken away, so the parser,
the field types and their keys, limits and checks are the ones above, and it
is read with unknown keys refused in the same way.

Every field type is allowed, with the same keys. Three names are reserved:
`title` (built in), `props` (where current Puck versions nest root values) and
`variant_classes` (see below). A root field name is unique across the site:
two enabled plugins declaring the same one stop the server at boot, with an
error naming both plugins and both files, as a duplicate component type does.
A `default` the field's own rules refuse (a select default that is not one of
its options) is an error at boot too.

#### Defaults

Unlike a component field's, a root field's `default` is applied when the page
renders: a page with no value for the field (absent, `null` or empty text),
and every page with no page builder content at all, gets the declared default.
A theme or tile template can therefore rely on `page_root.footer_cta_mode`
always having a value when the field declares a default. The editor also
starts a new page's settings at the defaults.

#### Validation

On every create and update through the item service, a page builder field's
root is checked like a component's props: each declared root field by its
type and limits, `title` as text. An error is reported at
`<item field>.root.<field>`, for example
`field_layout.root.footer_cta_mode: 'blink' is not one of: default, hide, custom`,
on the item form and as a 422 from the JSON API.

A root key that no enabled plugin declares is refused on save, so a misspelt
name, or a value meant for a plugin that is now disabled, is not stored
silently. A stored page is never validated to render: such a key is simply
not passed to templates, and the page renders as before. The editor drops
undeclared root keys when it writes the page, since it offers no form in which
one could be removed.

Puck keeps the root's values under `root.props` in current versions and
directly under `root` in older ones. The editor stores them flat,
`{ "root": { "title": "…", "footer_cta_mode": "hide" } }`; the kernel reads
either shape, and a key in both takes the value under `props`.

#### Templates: `page_root`

Every page template (`page.html` and its suggestions), the item template, and
every tile template rendered on the same request receive `page_root`:

- `page_root.title`, the root title, when it is text;
- one key per declared root field: the page's value or the declared default,
  normalized as a component prop is (a value outside a select's options, an
  unsafe URL or a value of the wrong type is left out), and for a `rich_text`
  field `<name>_html`, the kernel's sanitized rendering;
- `page_root.variant_classes`, the classes the chosen select options add,
  each one listed in a root file's `classes`. As for components, a root value
  never becomes inline CSS: a variant is a declared class.

A page with no page builder field, or a request that is not an item at all,
gets the defaults. Region templates are tiles: a `menu` or `gather_query`
tile's template gets `page_root` beside its own variables, and a
`custom_html` tile is rendered through `elements/tile--custom_html--<machine
name>` or `elements/tile--custom_html` when either template exists, with
`tile`, `body` (its filtered HTML) and `page_root`. A tile whose template
renders nothing but whitespace is left off the page, wrapper and all.

A sitewide footer call to action that a page can keep, hide or replace, as a
custom HTML tile named `pb_footer_cta` in the `footer` region and this
template in the plugin's `templates/elements/tile--custom_html--pb_footer_cta.html`:

```html+jinja
{%- set mode = "default" -%}
{%- if page_root.footer_cta_mode is defined %}{% set mode = page_root.footer_cta_mode %}{% endif -%}
{%- if mode == "hide" -%}
{%- elif mode == "custom" and page_root.footer_cta_content_html is defined -%}
{# SAFE: footer_cta_content_html is the kernel's sanitized Markdown rendering of a rich_text root field. #}
<div class="footer-cta footer-cta--custom">{{ page_root.footer_cta_content_html | safe }}</div>
{%- else -%}
{# SAFE: body is the tile's body, already through its text format's filter pipeline. #}
<div class="footer-cta footer-cta--default">{{ body | safe }}</div>
{%- endif -%}
```

#### In the editor

`GET /api/v1/page-builder/components` serves the root fields beside the
components, as `root_fields`: each in the component field format plus its
`source`. The editor builds its page settings form (Puck's root fields, shown
under "Page" when no component is selected) from them: the title first, then
each declared root field in load order, with the same control per type as a
component field.

---

## Testing

### Unit Testing

Create tests in your plugin's `src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_type_definition() {
        let types = tap_item_info();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].machine_name, "my_type");
    }
}
```

Run with:

```bash
cargo test -p my_plugin
```

### Integration Testing

For full integration tests, use the kernel's test utilities:

```rust
// In crates/kernel/tests/
use trovato_kernel::test_utils::TestApp;

#[tokio::test]
async fn test_plugin_route() {
    let app = TestApp::new().await;

    let response = app.request(
        Request::get("/my-plugin-route").body(Body::empty()).unwrap()
    ).await;

    assert_eq!(response.status(), StatusCode::OK);
}
```

---

## Deployment

### Building for Production

```bash
# Build optimized WASM
cargo build -p my_plugin --target wasm32-wasip1 --release

# Copy to plugins directory
cp target/wasm32-wasip1/release/my_plugin.wasm plugins/my_plugin/
```

### Plugin Loading

The kernel automatically loads plugins on startup:

1. Reads all `*.info.toml` files in `/plugins/`
2. Validates tap declarations against known taps
3. Resolves dependencies (topological sort)
4. Compiles WASM modules
5. Registers taps in the tap registry

### Enabling/Disabling

Plugins can be enabled or disabled through the admin UI or database. Disabled plugins are not loaded.

---

## Best Practices

### Do

- **Use the SDK prelude**: `use trovato_sdk::prelude::*;`
- **Return meaningful errors**: `Err("Specific error message".into())`
- **Use structured queries**: Prevents SQL injection
- **Cache expensive operations**: Use host cache functions
- **Check permissions**: Before sensitive operations
- **Handle missing data gracefully**: Use `Option` and provide defaults

### Don't

- **Don't panic**: Panics abort the tap; return errors instead
- **Don't use global mutable state**: Each request gets a fresh instance
- **Don't make direct HTTP calls**: Use host functions (when available)
- **Don't access the filesystem**: Plugins run in a sandbox
- **Don't assume execution order**: Use tap weights for ordering
- **Don't store secrets in code**: Use persistent variables

### Performance Tips

1. **Minimize cross-boundary calls**: Batch data access when possible
2. **Use caching**: Especially for database queries
3. **Keep payloads small**: Large JSON payloads add serialization overhead
4. **Avoid deep nesting**: In RenderElement trees
5. **Use weight ordering**: Instead of nested conditionals

### Security

1. **Always validate input**: Don't trust data from users
2. **Use structured queries**: Never concatenate SQL
3. **Check permissions**: Before modifying data
4. **Sanitize output**: Use `filtered_markup` for user content
5. **Don't expose sensitive data**: In error messages

---

## Appendix: Complete Example

Here's a complete blog plugin example:

```rust
//! Blog plugin for Trovato CMS

use trovato_sdk::prelude::*;

// === Content Type Definition ===

#[plugin_tap]
fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "blog".to_string(),
            label: "Blog Post".to_string(),
            description: "Blog posts with body and tags".to_string(),
            fields: vec![
                FieldDefinition::new("body", FieldType::TextLong)
                    .label("Body")
                    .required(),
                FieldDefinition::new("tags", FieldType::RecordReference("category_term".into()))
                    .label("Tags")
                    .cardinality(-1),
            ],
        }
    ]
}

// === Item View ===

#[plugin_tap]
fn tap_item_view(input: ItemViewInput) -> RenderElement {
    let item = &input.item;

    // Only handle blog items
    if item.item_type != "blog" {
        return render::container().build();
    }

    let body = item.get_text_value("body");

    render::container()
        .class("blog-post")
        .child("title",
            render::markup("h1", &item.title)
                .class("blog-title")
                .weight(-10)
                .build()
        )
        .child("meta",
            render::markup("div", &format!("Posted: {}", format_date(item.created)))
                .class("blog-meta")
                .weight(-5)
                .build()
        )
        .child("body",
            render::filtered_markup(
                &body.as_ref().map(|b| b.value.as_str()).unwrap_or(""),
                body.as_ref().map(|b| b.format.as_str()).unwrap_or("plain_text")
            )
            .class("blog-body")
            .build()
        )
        .build()
}

// === Access Control ===
// Most plugins don't need tap_item_access — the kernel handles published-item
// access and falls back to "{operation} {type} content" permission checks.
// Only implement this if you need custom logic (e.g., private items visible
// only to their author).

#[plugin_tap]
fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    if input.item_type != "blog" {
        return AccessResult::Neutral;
    }

    // Author can always access their own posts
    if input.user_id == input.author_id {
        return AccessResult::Grant;
    }

    // Defer to kernel permission fallback
    AccessResult::Neutral
}

// === Routes ===

#[plugin_tap]
fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/blog", "Blog")
            .callback("blog_listing")
            .permission("access content"),
        MenuDefinition::new("/blog/{id}", "View Post")
            .callback("blog_view")
            .permission("access content"),
    ]
}

// === Permissions ===
// Use crud_for_type() for standard view/create/edit/delete permissions matching
// the kernel fallback format. Author access ("own" semantics) is handled by
// tap_item_access above, not by permission strings.

#[plugin_tap]
fn tap_perm() -> Vec<PermissionDefinition> {
    PermissionDefinition::crud_for_type("blog")
}

// === Helpers ===

fn format_date(timestamp: i64) -> String {
    // Simple date formatting
    let secs = timestamp;
    format!("{}", secs) // Replace with proper formatting
}
```

With `blog.info.toml`:

```toml
name = "blog"
description = "Provides a blog content type with tags"
version = "1.0.0"
dependencies = ["categories"]

[taps]
implements = [
    "tap_item_info",
    "tap_item_view",
    "tap_item_access",
    "tap_menu",
    "tap_perm",
]
weight = 0
```
