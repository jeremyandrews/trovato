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

# Optional (API 0.106): tables of your own holding site configuration,
# carried by `trovato config export` / `config import`.
[config]
tables = ["my_plugin_setting"]
# Optional (API 0.107): columns holding a secret, password, token, key or a
# hash of one. Exported as env:TROVATO_SECRET_<TABLE>_<PK>_<COLUMN>, never as
# the value; import reads that variable.
secret_columns = { my_plugin_setting = ["api_key"] }
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

// Tap that can fail (an Err is logged by the kernel)
#[plugin_tap_result]
fn tap_cron(_input: serde_json::Value) -> Result<(), String> {
    Ok(())
}
```

**Size limit:** a tap or `tap_api` route may send and receive up to **8 MiB**
each way (`trovato_sdk::abi::PLUGIN_IO_MAX_BYTES`). Over that, the kernel
refuses the call with a documented error rather than truncating it.

---

## Available Taps

| Category | Tap | Input | Output |
|----------|-----|-------|--------|
| **Content Types** | `tap_item_info` | - | `Vec<ContentTypeDefinition>` |
| **View** | `tap_item_view` | `ItemViewInput` | `RenderElement` |
| **View** | `tap_item_view_alter` | `ItemViewAlterInput` | `RenderElement` |
| **CRUD** | `tap_item_presave` | `{item_type, title, fields, status}` | `{"fields": {...}}`, merged into the write |
| **CRUD** | `tap_item_insert` | the created item | ignored (notification) |
| **CRUD** | `tap_item_update` | the updated item | ignored (notification) |
| **CRUD** | `tap_item_update_index` | the saved item | ignored (notification) |
| **CRUD** | `tap_item_delete` | the item being deleted | ignored (notification; cannot abort) |
| **Access** | `tap_item_access` | `ItemAccessInput` | `AccessResult` |
| **Access** | `tap_vocabulary_owner_access` | `VocabularyOwnerAccessRequest` | `VocabularyOwnerAccess` (grant, deny, neutral, not_found) |
| **Forms** | `tap_form_alter` | the form as JSON (`form_id`, `elements`, `libraries`) | the form, altered; plugins chain in weight order |
| **Forms** | `tap_form_validate` | `{form_id, values}` (rendered inputs only, no passwords) | `{}` or `{"errors": ["msg" or {"field", "message"}]}` |
| **Forms** | `tap_form_submit` | `{form_id, values, item_id}` | ignored; fires once after a successful save |
| **Forms** | `tap_form_ajax` | `{trigger, form_id, values}` (CSRF, form access checked; full host API) | `{"commands": [...]}` or `{}` |
| **System** | `tap_menu` | - | `Vec<MenuDefinition>` |
| **System** | `tap_perm` | - | `Vec<PermissionDefinition>` |
| **System** | `tap_admin_nav` | - | `Vec<AdminNavDefinition>` |
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
    .visible_when("field_online", "1")  // shown only while field_online is checked
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

// Item properties (serde: item_type serializes as "type")
item.id                   // Uuid
item.item_type            // String — #[serde(rename = "type")]
item.title                // String
item.fields               // HashMap<String, Value> — #[serde(default)]
item.status               // i32 (0=unpublished, 1=published)
item.author_id            // Uuid
item.current_revision_id  // Option<Uuid>
item.stage_id             // Uuid (defaults to live stage)
item.created              // i64 (unix timestamp)
item.changed              // i64 (unix timestamp)
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
// Other users' public facts, by id (≤ 100 a call). Needs `user-api` and
// `user_directory = true` under [capabilities]; never an address.
let users = host::load_users(&[id_a, id_b])?;   // Vec<UserSummary>
// By exact name (case-insensitive, like login) or by a name prefix (≤ 10
// results, sorted). Same capability, same never-an-address rule.
let user = host::find_user_by_name("jsmith")?;           // Option<UserSummary>
let matches = host::find_users_by_name_prefix("js")?;    // Vec<UserSummary>
```

### Mail
Needs `mail` in `host_interfaces`. Mailing a member also needs `mail_users = true`
under [capabilities]; the kernel resolves the address and adds an unsubscribe link.
Many recipients: `queue_push` one job per recipient, send from `tap_queue_worker`.
```rust
host::mail_send_to_site_contacts("Subject", "Body", &[])?;  // site contact only
host::mail_send_to_user(&user_id, "Subject", "Body")?;      // one member, by id
```

### Items
Needs `item-api` in `host_interfaces`; acts as the requesting user (from cron or
a queue worker, needs `item_background = true`). Writes fire the item write taps
on the other plugins, as a person's edit does.
```rust
let item = host::get_item(&id)?;                                   // None: missing or not viewable
let saved = host::save_item(&json!({ "id": id, "title": "New" }))?;  // no id + "type": create
let moved = host::save_item(&json!({ "id": id, "stage": "curated" }))?; // by the workflow; create: default stage
host::delete_item(&id)?;                                          // Err(-62): no such item
let list = host::query_items(&json!({ "type": "page", "limit": 20 }))?; // full items, request's stage
let tr = host::list_translations(&[&id])?;                        // ≤ 100 ids, viewable only
host::save_translation(&json!({"item_id": id, "language": "it", "title": "Ciao"}))?; // an edit
```

### Queries
Needs `query` in `host_interfaces`. A Gather definition, run for the requesting
user on the request's stage and language; item rows carry `url`.
```rust
let page = host::query(&json!({
    "base_table": "item",
    "filters": [{"field": "type", "operator": "in", "value": ["blog", "news"]}],
    "sorts": [{"field": "created", "direction": "desc"}]
}), &json!({"per_page": 10}))?;                    // page.items, page.has_next
// by address: {"field": "url", "operator": "equals", "value": "/about"}
// or: "filter_groups": [{"conjunction": "or", "filters": [...]}]   (3 deep)
// own table only: "group_by", "aggregates": [{"function": "count", "label": "n"}], "having"
// Err(-16): table not declared  Err(-80): refused  Err(-81): over a bound
```

### Text formats
Needs `text-format` in `host_interfaces`. **Rendering HTML a user wrote? Filter
it through this first**: the kernel does not filter a plugin's output.
```rust
let safe = host::filter_text(&body, "filtered_html")?;  // Err(-70): user may not use it
                                                         // Err(-71): no such format
```

### Owned vocabularies
Needs `vocabulary` in `host_interfaces` and a `[[vocabulary_owners]]` entry in
the manifest (`owner_type`, `label`, `path = "/group/{owner}/categories"`,
optional `permission`). See "Owned vocabularies" in the plugin guide.
```rust
let v = host::create_owned_vocabulary(
    &OwnedVocabularyRequest::new("group", &group_id, "group_7_topics", "Topics"))?;
let all = host::list_owned_vocabularies("group", &group_id)?;
// Err(-90): owner type not declared  Err(-91): bad id or label  Err(-92): id taken
```

### Cache
```rust
let cached = host::cache::get("bin", "key");
host::cache::set("bin", "key", &value, json!(["tag:items"]));
host::cache::invalidate_tag("tag:items");
```

### Database
Your own tables only (migration-created or `db_tables`); content goes through
`host::query`, `save_item` and `save_translation`.
```rust
use trovato_sdk::host::db;
let rows = db::select(&json!({"table": "my_votes", "where": {"voter": v}, "limit": 10}))?;
let row = db::insert("my_votes", &json!({"voter": v, "score": 1}))?;
let n = db::update("my_votes", &json!({"score": 0}),
    &json!([{"field": "score", "op": ">", "value": 5}]))?;       // map or array where
let n = db::delete("my_votes", &json!([{"field": "voter", "op": "in", "value": ["a", "b"]}]))?;
let row = db::upsert("my_votes", &json!({"k": k, "score": 3}), &json!({"target": ["k"]}))?;
let n = db::insert_many("my_votes", &rows)?;                      // ≤ 500, all or none; Err(-82)
```
`raw_sql = true` opens `host::query_raw`/`execute_raw` with no viewer, stage or
language; the admin listing and the boot log name every holder. Before asking
for it, read [plugin-capabilities.md](plugin-capabilities.md#raw_sql).

### Inter-Plugin
```rust
if host::plugin::exists("other_plugin") {
    let result = host::plugin::invoke("other_plugin", "func", json!({}))?;
}
```

---

## `db` where operators

| Op | Example |
|----|---------|
| `=`, `!=`, `>`, `<`, `>=`, `<=` | `{"field": "x", "op": "=", "value": 1}` |
| `in`, `not_in` | `{"field": "x", "op": "in", "value": [1, 2, 3]}` (not empty) |
| `is_null`, `is_not_null` | `{"field": "x", "op": "is_null"}` |

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
- Return HTML a user wrote without `host::filter_text`

## Do

- Use `trovato_sdk::prelude::*`
- Return descriptive errors
- Use structured queries
- Cache expensive operations
- Check permissions
- Handle missing data with `Option`
