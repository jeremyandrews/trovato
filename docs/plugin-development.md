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
13. [Testing](#testing)
14. [Deployment](#deployment)
15. [Best Practices](#best-practices)

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
| `display.routes` path with `?format=json`, `/api/query/{query_id}`, `/api/query/{query_id}/execute` | 403, JSON `error` | 403, JSON `error` |
| `display.feed` path | 403 | 403 |
| `/api/queries` | the gather is left out | the gather is left out |
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
| Reference | `FieldType::RecordReference(target_type)` | Reference to another record |

### Field Options

Every field definition, in Rust or in an `item_type.*.yml` config file, takes
these options besides its name, type and label:

| Option | Default | Meaning |
|--------|---------|---------|
| `required` | `false` | The form refuses a save without a value |
| `cardinality` | `1` | How many values: `1` single, `-1` unlimited, `n` at most `n`. The form shows each stored value plus one empty slot and an "Add another" button, and refuses more than `n` |
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

ABI: a new function, `load-users`, in the existing `user-api` interface and a
new manifest capability, `user_directory` (B73, after 0.104.0). No existing
signature changed.

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

// List what the user may view.
let pages = host::query_items(&json!({ "type": "page", "limit": 20 }))?;
```

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
the item forms apply: `plain_text` always, `filtered_html` with
`use filtered_html`, `full_html` with `use full_html`. Anything else is an error,
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

---

## Database Operations

### Structured Queries (Recommended)

Use structured queries to prevent SQL injection:

```rust
let results = host::db::select(json!({
    "table": "item",
    "fields": ["id", "title", "created"],
    "conditions": [
        {"field": "type", "op": "=", "value": "blog"},
        {"field": "status", "op": "=", "value": 1}
    ],
    "order_by": [{"field": "created", "direction": "DESC"}],
    "limit": 10
}))?;
```

### Supported Operators

| Operator | Example |
|----------|---------|
| `=`, `!=`, `>`, `<`, `>=`, `<=` | `{"field": "status", "op": "=", "value": 1}` |
| `LIKE` | `{"field": "title", "op": "LIKE", "value": "%search%"}` |
| `IN` | `{"field": "type", "op": "IN", "value": ["blog", "article"]}` |
| `IS NULL` | `{"field": "deleted", "op": "IS NULL"}` |
| `BETWEEN` | `{"field": "created", "op": "BETWEEN", "value": [start, end]}` |

### Insert/Update/Delete

```rust
// Insert
let id = host::db::insert("my_table", json!({
    "name": "Example",
    "value": 42
}))?;

// Update
let affected = host::db::update("my_table",
    json!({"value": 100}),
    json!([{"field": "id", "op": "=", "value": id}])
)?;

// Delete
let deleted = host::db::delete("my_table",
    json!([{"field": "id", "op": "=", "value": id}])
)?;
```

### Raw SQL (Requires Permission)

Only use when structured queries aren't sufficient:

```rust
// Query
let results = host::db::query_raw(
    "SELECT * FROM my_table WHERE name = $1",
    json!(["Example"])
)?;

// Execute
let affected = host::db::execute_raw(
    "UPDATE my_table SET counter = counter + 1 WHERE id = $1",
    json!([id])
)?;
```

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
