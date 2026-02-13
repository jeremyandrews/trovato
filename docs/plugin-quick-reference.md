# Trovato Plugin Quick Reference

A condensed reference for Trovato plugin development.

---

## Project Setup

```bash
# Install WASM target
rustup target add wasm32-wasip1

# Build plugin
cargo build -p my_plugin --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/my_plugin.wasm plugins/my_plugin/
```

**Cargo.toml:**
```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
trovato-sdk = { path = "../../crates/plugin-sdk" }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

**my_plugin.info.toml:**
```toml
name = "my_plugin"
description = "Plugin description"
version = "1.0.0"
dependencies = []

[taps]
implements = ["tap_item_info", "tap_item_view"]
weight = 0
```

---

## Tap Functions

```rust
use trovato_sdk::prelude::*;

// Basic tap (no input)
#[plugin_tap]
fn tap_item_info() -> Vec<ContentTypeDefinition> { vec![] }

// Tap with input
#[plugin_tap]
fn tap_item_view(input: ItemViewInput) -> RenderElement {
    render::container().build()
}

// Tap that can fail
#[plugin_tap_result]
fn tap_item_insert(input: ItemInput) -> Result<(), String> {
    Ok(())
}
```

---

## Available Taps

| Category | Tap | Input | Output |
|----------|-----|-------|--------|
| **Content Types** | `tap_item_info` | - | `Vec<ContentTypeDefinition>` |
| **View** | `tap_item_view` | `ItemViewInput` | `RenderElement` |
| **View** | `tap_item_view_alter` | `ItemViewAlterInput` | `RenderElement` |
| **CRUD** | `tap_item_insert` | `ItemInput` | `Result<(), String>` |
| **CRUD** | `tap_item_update` | `ItemInput` | `Result<(), String>` |
| **CRUD** | `tap_item_delete` | `ItemDeleteInput` | `Result<(), String>` |
| **Access** | `tap_item_access` | `ItemAccessInput` | `AccessResult` |
| **Forms** | `tap_form_alter` | `FormAlterInput` | `FormDefinition` |
| **Forms** | `tap_form_validate` | `FormValidateInput` | `Result<(), String>` |
| **Forms** | `tap_form_submit` | `FormSubmitInput` | `Result<(), String>` |
| **System** | `tap_menu` | - | `Vec<MenuDefinition>` |
| **System** | `tap_perm` | - | `Vec<PermissionDefinition>` |
| **System** | `tap_cron` | - | `Result<(), String>` |
| **Lifecycle** | `tap_install` | - | `Result<(), String>` |
| **Lifecycle** | `tap_enable` | - | `Result<(), String>` |
| **Lifecycle** | `tap_disable` | - | `Result<(), String>` |

---

## Field Types

```rust
FieldType::Text { max_length: Some(255) }  // Single-line text
FieldType::TextLong                         // Multi-line with format
FieldType::Integer                          // Whole numbers
FieldType::Float                            // Decimal numbers
FieldType::Boolean                          // True/false
FieldType::Date                             // Date
FieldType::Email                            // Email address
FieldType::File                             // File upload
FieldType::RecordReference("category_term".into())  // Reference
```

---

## Field Definition

```rust
FieldDefinition::new("field_name", FieldType::TextLong)
    .label("Display Label")
    .required()              // Mark as required
    .cardinality(1)          // 1 = single, -1 = unlimited
```

---

## Content Type Definition

```rust
ContentTypeDefinition {
    machine_name: "article".to_string(),
    label: "Article".to_string(),
    description: "Description".to_string(),
    fields: vec![
        FieldDefinition::new("body", FieldType::TextLong)
            .label("Body")
            .required(),
    ],
}
```

---

## Render Elements

```rust
use trovato_sdk::render;

// Container
render::container()
    .class("my-class")
    .attr("data-id", "123")
    .child("name", element)
    .build()

// Markup
render::markup("h1", "Title")
    .class("title")
    .weight(-10)
    .build()

// Filtered HTML (sanitized)
render::filtered_markup(&content, "filtered_html")
    .build()

// Link
render::link("/path", "Link Text")
    .class("button")
    .build()
```

---

## Item Access

```rust
// Get field values from item
let text: Option<String> = item.get_text("field_name");
let text_value: Option<TextValue> = item.get_text_value("field_name");
let typed: Option<MyType> = item.get_field("field_name");

// Item properties
item.id           // Uuid
item.item_type    // String
item.title        // String
item.status       // i32 (0=unpublished, 1=published)
item.author_id    // Uuid
item.created      // i64 (unix timestamp)
item.changed      // i64 (unix timestamp)
```

---

## Access Results

```rust
AccessResult::Grant    // Allow access
AccessResult::Deny     // Deny (wins over Grant)
AccessResult::Neutral  // No opinion
```

**Rule:** `Deny > Grant > Neutral`. All Neutral = Deny.

---

## Host Functions

### Logging
```rust
host::log(LogLevel::Info, "plugin", "message");
host::log(LogLevel::Error, "plugin", "error");
```

### Variables (Persistent)
```rust
let val = host::variable::get("key", "default");
host::variable::set("key", "value")?;
```

### Request Context
```rust
host::context::set("key", "value");
let val = host::context::get("key");  // Option<String>
```

### User
```rust
let user_id = host::user::current_user_id();
let has_perm = host::user::has_permission("permission name");
```

### Items
```rust
let item = host::item::get(uuid)?;
let saved = host::item::save(&item)?;
host::item::delete(uuid)?;
```

### Cache
```rust
let cached = host::cache::get("bin", "key");
host::cache::set("bin", "key", &value, json!(["tag:items"]));
host::cache::invalidate_tag("tag:items");
```

### Database
```rust
// Structured query (recommended)
let results = host::db::select(json!({
    "table": "item",
    "fields": ["id", "title"],
    "conditions": [{"field": "status", "op": "=", "value": 1}],
    "order_by": [{"field": "created", "direction": "DESC"}],
    "limit": 10
}))?;

// Insert
let id = host::db::insert("table", json!({"name": "value"}))?;

// Update
let affected = host::db::update("table",
    json!({"name": "new"}),
    json!([{"field": "id", "op": "=", "value": id}])
)?;

// Delete
let deleted = host::db::delete("table",
    json!([{"field": "id", "op": "=", "value": id}])
)?;
```

### Inter-Plugin
```rust
if host::plugin::exists("other_plugin") {
    let result = host::plugin::invoke("other_plugin", "func", json!({}))?;
}
```

---

## Query Operators

| Op | Example |
|----|---------|
| `=`, `!=`, `>`, `<`, `>=`, `<=` | `{"field": "x", "op": "=", "value": 1}` |
| `LIKE` | `{"field": "x", "op": "LIKE", "value": "%term%"}` |
| `IN` | `{"field": "x", "op": "IN", "value": [1, 2, 3]}` |
| `IS NULL` | `{"field": "x", "op": "IS NULL"}` |
| `BETWEEN` | `{"field": "x", "op": "BETWEEN", "value": [1, 10]}` |

---

## Menu Definition

```rust
MenuDefinition::new("/path", "Title")
    .callback("function_name")
    .permission("access content")
    .parent("/admin")
```

---

## Permission Definition

```rust
PermissionDefinition::new(
    "permission name",
    "Human description"
)
```

---

## Common Patterns

### Check Item Type
```rust
fn tap_item_view(input: ItemViewInput) -> RenderElement {
    if input.item.item_type != "my_type" {
        return render::container().build();  // Empty for other types
    }
    // Handle my_type...
}
```

### Check Permissions
```rust
if !host::user::has_permission("edit content") {
    return Err("Permission denied".into());
}
```

### Cache Results
```rust
let cache_key = format!("item:{}", item_id);
if let Some(cached) = host::cache::get("views", &cache_key) {
    return Ok(cached);
}
let result = compute_expensive();
host::cache::set("views", &cache_key, &result, json!(["tag:items"]));
```

---

## Don't

- Panic (return errors instead)
- Use global mutable state
- Access filesystem
- Make HTTP calls directly
- Concatenate SQL strings
- Expose secrets in errors

## Do

- Use `trovato_sdk::prelude::*`
- Return descriptive errors
- Use structured queries
- Cache expensive operations
- Check permissions
- Handle missing data with `Option`
