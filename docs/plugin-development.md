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
12. [Testing](#testing)
13. [Deployment](#deployment)
14. [Best Practices](#best-practices)

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
pub struct Item {
    pub id: Uuid,
    pub item_type: String,
    pub title: String,
    pub fields: HashMap<String, Value>,
    pub status: i32,              // 0 = unpublished, 1 = published
    pub author_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub stage_id: Option<String>, // None = live
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
    let item = &input.item;
    let user_id = host::user::current_user_id();

    // Published items visible to all
    if item.status == 1 {
        return AccessResult::Grant;
    }

    // Unpublished only visible to author
    if item.author_id == user_id {
        return AccessResult::Grant;
    }

    // Admins can see everything
    if host::user::has_permission("administer content") {
        return AccessResult::Grant;
    }

    AccessResult::Deny
}
```

### AccessResult Values

| Value | Meaning |
|-------|---------|
| `Grant` | Explicitly allow access |
| `Deny` | Explicitly deny (wins over Grant) |
| `Neutral` | No opinion (let other plugins decide) |

**Aggregation rule:** Deny > Grant > Neutral. If all plugins return Neutral, access is denied.

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

#[plugin_tap]
fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    let item = &input.item;

    if item.item_type != "blog" {
        return AccessResult::Neutral;
    }

    // Published posts visible to all
    if item.status == 1 {
        return AccessResult::Grant;
    }

    // Unpublished only to author or admin
    let user_id = host::user::current_user_id();
    if item.author_id == user_id || host::user::has_permission("administer content") {
        return AccessResult::Grant;
    }

    AccessResult::Deny
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

#[plugin_tap]
fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("create blog content", "Create blog posts"),
        PermissionDefinition::new("edit own blog content", "Edit own blog posts"),
        PermissionDefinition::new("edit any blog content", "Edit any blog post"),
        PermissionDefinition::new("delete own blog content", "Delete own blog posts"),
        PermissionDefinition::new("delete any blog content", "Delete any blog post"),
    ]
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
