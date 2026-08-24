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
fn tap_item_insert(input: ItemInput) -> Result<(), String> {
    if input.item.title.is_empty() {
        return Err("Title is required".to_string());
    }
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
| `tap_item_insert` | `ItemInput` | `Result<(), String>` | Pre-insert validation |
| `tap_item_update` | `ItemInput` | `Result<(), String>` | Pre-update validation |
| `tap_item_delete` | `ItemDeleteInput` | `Result<(), String>` | Pre-delete hook |
| `tap_item_access` | `ItemAccessInput` | `AccessResult` | Control item visibility |

#### Forms

| Tap | Input | Output | Description |
|-----|-------|--------|-------------|
| `tap_form_alter` | `FormAlterInput` | `FormDefinition` | Modify form structure |
| `tap_form_validate` | `FormValidateInput` | `Result<(), String>` | Validate submission |
| `tap_form_submit` | `FormSubmitInput` | `Result<(), String>` | Handle submission |

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

// Check permission
if host::user::has_permission("administer site") {
    // Admin-only logic
}
```

### Item Operations

```rust
// Load item
let item = host::item::get(item_id)?;

// Save item
let saved = host::item::save(&item)?;

// Delete item
host::item::delete(item_id)?;
```

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
