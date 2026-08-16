# Trovato Design: Plugin SDK Specification

*New document — fills the gap identified in Section 22 of the v2.1 design.*

This document specifies the `trovato_sdk` crate: the types, macros, host function bindings, and conventions that plugin authors use to build Trovato plugins. It is the authoritative reference for the plugin developer experience.

**Design principle:** Write the code you want developers to write, then build the Kernel to run it. If the SDK requires developers to understand WASM memory layout, JSON serialization, or handle indices, the design is wrong.

---

## 1. Plugin Structure

A plugin is a separate Rust crate compiled to `wasm32-wasip1`.

```
plugins/
  blog/
    Cargo.toml
    blog.info.toml       # metadata + tap declarations
    src/
      lib.rs             # plugin source
    target/
      wasm32-wasip1/release/
        blog.wasm        # compiled artifact
```

### 1.1 Cargo.toml

```toml
[package]
name = "blog"
version = "1.0.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
trovato_sdk = { path = "../../crates/plugin-sdk" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

### 1.2 Plugin Metadata (blog.info.toml)

```toml
name = "blog"
description = "Provides a blog content type with tags"
version = "1.0.0"
dependencies = ["item", "categories"]

[taps]
implements = [
    "tap_item_info",
    "tap_item_view",
    "tap_menu",
    "tap_perm",
]
weight = 0   # execution order; lower = earlier

[taps.options]
tap_item_view = { data_mode = "handle" }  # "handle" (default) or "full"
```

**Required fields:** name, version, taps.implements
**Optional fields:** description, dependencies, weight (default 0), taps.options

### 1.3 Data Access Modes

Declared per tap in `[taps.options]`. Default is `"handle"`.

| Mode | When to use | How it works |
|------|------------|--------------|
| `handle` | Most taps (default) | Kernel passes an opaque `ItemHandle`. Plugin reads/writes fields via host functions. No bulk serialization. |
| `full` | Complex mutations, bulk transforms | Kernel passes full Item JSON. Plugin returns modified JSON. Higher latency but full access. |

---

## 2. The trovato_sdk Crate

### 2.1 Prelude

```rust
use trovato_sdk::prelude::*;
```

Exports: `ItemHandle`, `Item`, `TapContext`, `RenderElement`, `ContentTypeDefinition`, `FieldDefinition`, `FieldType`, `MenuDefinition`, `PermissionDefinition`, `RecordRef`, `TextValue`, `render`, `plugin_tap`, `plugin_info`.

### 2.2 Proc Macros

#### `#[plugin_info]`

Placed on a module block. Declares plugin metadata that the SDK uses for compile-time validation against `.info.toml`.

```rust
#[plugin_info]
mod blog {
    const NAME: &str = "blog";
    const DESCRIPTION: &str = "Provides a blog content type";
    const VERSION: &str = "1.0.0";
    const DEPENDENCIES: &[&str] = &["item", "categories"];
}
```

**What it generates:** A static metadata struct accessible to the Kernel at load time. Does not generate `.info.toml` (that file is hand-written and is the source of truth for the Kernel).

#### `#[plugin_tap]`

Placed on a function. Generates the WASM export boilerplate matching the WIT interface.

```rust
#[plugin_tap]
fn item_view(item: &ItemHandle, ctx: &TapContext) -> RenderElement {
    // plugin logic
}
```

**What it generates:**

1. A WASM export function named `tap-item-view` (matching WIT convention)
2. For handle-based mode: receives `item-handle: s32`, wraps it in `ItemHandle`
3. For full-serialization mode: receives `item-json: string`, deserializes to `Item`
4. Creates a `TapContext` from the current request state
5. Calls the developer's function
6. Serializes the return value for the WASM boundary
7. Handles panics: catches, logs via host `log` function, returns an error RenderElement

**Function signature conventions by tap type:**

| Tap | Signature | Return |
|-----|-----------|--------|
| `item_info` | `fn() -> Vec<ContentTypeDefinition>` | Serialized to JSON |
| `item_view` | `fn(item: &ItemHandle, ctx: &TapContext) -> RenderElement` | Serialized to JSON |
| `item_view_alter` | `fn(render: &mut RenderElement, item: &ItemHandle, ctx: &TapContext)` | Mutates in place |
| `item_insert` | `fn(item: &ItemHandle, ctx: &TapContext) -> Result<(), String>` | Error string on failure |
| `item_update` | `fn(item: &ItemHandle, ctx: &TapContext) -> Result<(), String>` | Error string on failure |
| `item_delete` | `fn(item_id: Uuid, ctx: &TapContext) -> Result<(), String>` | Error string on failure |
| `item_access` | `fn(item: &ItemHandle, op: &str, ctx: &TapContext) -> AccessResult` | Grant / Deny / Neutral |
| `menu` | `fn() -> Vec<MenuDefinition>` | Serialized to JSON |
| `perm` | `fn() -> Vec<PermissionDefinition>` | Serialized to JSON |
| `form_alter` | `fn(form: &mut Form, form_id: &str, ctx: &TapContext)` | Mutates in place |
| `form_validate` | `fn(form_id: &str, values: &FormValues, ctx: &TapContext) -> Vec<FormError>` | Errors list |
| `form_submit` | `fn(form_id: &str, values: &FormValues, ctx: &TapContext) -> Result<(), String>` | Error on failure |
| `cron` | `fn(ctx: &TapContext) -> Result<(), String>` | Error on failure |
| `install` | `fn(ctx: &TapContext) -> Result<(), String>` | Create tables, seed data |
| `enable` | `fn(ctx: &TapContext) -> Result<(), String>` | Activation logic |
| `disable` | `fn(ctx: &TapContext) -> Result<(), String>` | Deactivation logic |
| `theme` | `fn() -> Vec<ThemeDefinition>` | Template registrations |
| `preprocess_item` | `fn(variables: &mut TemplateVars, item: &ItemHandle, ctx: &TapContext)` | Adds template vars |
| `item_update_index` | `fn(item: &ItemHandle, ctx: &TapContext) -> String` | Searchable text |
| `queue_info` | `fn() -> Vec<QueueDefinition>` | Queue declarations |
| `queue_worker` | `fn(payload: &str, ctx: &TapContext) -> Result<(), String>` | Process one queue item |

---

## 3. Core Types

### 3.1 ItemHandle

An opaque reference to an Item held in the Kernel's `RequestState`. Each host function call crosses the WASM boundary but transfers only the requested data, not the entire item.

```rust
pub struct ItemHandle {
    handle: i32,  // opaque WASM index, NOT the entity UUID
}

impl ItemHandle {
    // Metadata (read-only)
    pub fn id(&self) -> Uuid;             // item UUID
    pub fn revision_id(&self) -> Uuid;    // current revision UUID
    pub fn item_type(&self) -> String;    // "blog", "page", etc.
    pub fn author_id(&self) -> Uuid;      // author UUID
    pub fn status(&self) -> i32;          // 1 = published, 0 = unpublished
    pub fn created(&self) -> i64;         // unix timestamp
    pub fn changed(&self) -> i64;         // unix timestamp
    pub fn title(&self) -> String;

    // Field access (generic)
    pub fn get_field<T: FromField>(&self, field_name: &str) -> Option<T>;
    pub fn set_field<T: IntoField>(&self, field_name: &str, value: T);

    // Raw JSONB access (escape hatch)
    pub fn get_field_json(&self, field_name: &str) -> Option<String>;
    pub fn set_field_json(&self, field_name: &str, json: &str);
}
```

**Implementation note:** Each method calls a WIT host function. `title()` calls `get-title(handle)`. `id()` calls `get-id(handle)` which returns a UUID string that the SDK parses. `get_field::<String>("body")` calls `get-field-string(handle, "body")`. The SDK maps Rust generic types to the appropriate WIT function at compile time via the `FromField` trait.

### 3.2 FromField / IntoField Traits

```rust
pub trait FromField: Sized {
    fn from_field(handle: i32, field_name: &str) -> Option<Self>;
}

pub trait IntoField {
    fn into_field(self, handle: i32, field_name: &str);
}
```

**Implementations provided by the SDK:**

| Rust Type | WIT Function | JSONB Shape |
|-----------|-------------|-------------|
| `String` | `get-field-string` / `set-field-string` | `{"value": "..."}` → extracts `value` |
| `i64` | `get-field-int` / `set-field-int` | `{"value": N}` → extracts `value` |
| `f64` | `get-field-float` / `set-field-float` | `{"value": N.N}` → extracts `value` |
| `bool` | `get-field-int` / `set-field-int` | `{"value": 0\|1}` → converts |
| `TextValue` | `get-field-json` | `{"value": "...", "format": "filtered_html"}` |
| `Vec<RecordRef>` | `get-field-json` | `[{"target_id": N, "target_type": "..."}]` |
| `RecordRef` | `get-field-json` | `{"target_id": N, "target_type": "..."}` |
| `serde_json::Value` | `get-field-json` / `set-field-json` | Any valid JSON |

### 3.3 Field Value Types

```rust
/// Text with a format (filtered_html, plain_text, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextValue {
    pub value: String,
    pub format: String,
}

/// Reference to another record (item, user, category term)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordRef {
    pub target_id: Uuid,
    pub target_type: String,  // "item", "user", "category_term"
}
```

### 3.4 Content Type Definitions

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentTypeDefinition {
    pub machine_name: String,      // "blog"
    pub label: String,             // "Blog Post"
    pub description: String,
    pub fields: Vec<FieldDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub field_name: String,        // "field_body"
    pub field_type: FieldType,
    pub label: String,
    pub required: bool,
    pub cardinality: i32,          // 1 = single, -1 = unlimited
    pub settings: serde_json::Value,
}

impl FieldDefinition {
    pub fn new(name: &str, field_type: FieldType) -> Self;
    pub fn required(mut self) -> Self;
    pub fn label(mut self, label: &str) -> Self;
    pub fn cardinality(mut self, n: i32) -> Self;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    Text { max_length: Option<usize> },
    TextLong,                          // no length limit, has format
    Integer,
    Float,
    Boolean,
    RecordReference(String),           // target type: "category_term", "user", etc.
    File,
    Date,
    Email,
}
```

### 3.5 Menu & Permission Definitions

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuDefinition {
    pub path: String,              // "blog" -> /blog
    pub title: String,
    pub callback: String,          // function name in this plugin
    pub permission: String,        // "access content"
    pub parent: Option<String>,    // parent path for breadcrumbs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDefinition {
    pub name: String,              // "create blog content"
    pub description: String,
}

impl PermissionDefinition {
    pub fn new(name: &str, description: &str) -> Self;
}
```

### 3.6 TapContext

Provides access to the current request's state without exposing WASM internals.

```rust
pub struct TapContext {
    // All methods call host functions
}

impl TapContext {
    // Current user
    pub fn current_user_id(&self) -> Uuid;
    pub fn has_permission(&self, permission: &str) -> bool;

    // Request context (key-value store shared across plugins for this request)
    pub fn get(&self, key: &str) -> Option<String>;
    pub fn set(&self, key: &str, value: &str);

    // Variables (persistent key-value config, stored in DB)
    pub fn variable_get(&self, name: &str, default: &str) -> String;
    pub fn variable_set(&self, name: &str, value: &str) -> Result<(), String>;

    // Logging
    pub fn log(&self, level: LogLevel, message: &str);

    // Database (structured queries only by default)
    pub fn db_select(&self, query: &SelectQuery) -> Result<Vec<serde_json::Value>, String>;
    pub fn db_insert(&self, table: &str, data: &serde_json::Value) -> Result<Uuid, String>;

    // Inter-plugin communication
    pub fn invoke_plugin(&self, plugin: &str, function: &str, payload: &str)
        -> Result<String, String>;
    pub fn plugin_exists(&self, plugin: &str) -> bool;

    // Cache
    pub fn cache_get(&self, bin: &str, key: &str) -> Option<String>;
    pub fn cache_set(&self, bin: &str, key: &str, value: &str, tags: &[&str]);
    pub fn cache_invalidate_tag(&self, tag: &str);
}

#[derive(Debug, Clone)]
pub enum LogLevel { Debug, Info, Warning, Error }
```

### 3.7 Access Control

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum AccessResult {
    Grant,     // explicitly allow (overrides Neutral, not Deny)
    Deny,      // explicitly deny (wins over everything)
    Neutral,   // no opinion; fall through to next plugin
}
```

**Aggregation rule:** If any plugin returns `Deny`, access is denied. Otherwise, if any plugin returns `Grant`, access is granted. If all return `Neutral`, access is denied (safe default).

---

## 4. RenderElement

### 4.1 JSON Schema

Every plugin view tap returns a RenderElement. This is a JSON tree with `#`-prefixed metadata keys and named children.

```json
{
    "#type": "container",
    "#weight": 0,
    "#attributes": { "class": ["item", "item--blog"] },
    "title": {
        "#type": "markup",
        "#tag": "h2",
        "#weight": -10,
        "#value": "My Blog Post"
    },
    "body": {
        "#type": "markup",
        "#weight": 0,
        "#value": "<p>Post content here...</p>",
        "#format": "filtered_html"
    },
    "tags": {
        "#type": "container",
        "#weight": 10,
        "#attributes": { "class": ["field", "field--tags"] },
        "0": { "#type": "markup", "#tag": "a", "#value": "Rust", "#attributes": { "href": "/categories/rust" } },
        "1": { "#type": "markup", "#tag": "a", "#value": "WASM", "#attributes": { "href": "/categories/wasm" } }
    }
}
```

**Properties:**

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `#type` | string | yes | Element type: `container`, `markup`, `table`, `list`, `link` |
| `#weight` | integer | no (default 0) | Sort order among siblings. Lower = earlier. |
| `#attributes` | object | no | HTML attributes: `class` (array), `id`, `data-*`, etc. |
| `#value` | string | depends | The content. Required for `markup`. |
| `#tag` | string | no (default `div`) | HTML tag for `markup` type. |
| `#format` | string | no | Text format for sanitization. If present, value runs through the filter pipeline. If absent, value is HTML-escaped. |
| (children) | object | no | Named child RenderElements. Rendered in `#weight` order. |

**Element types:**

| Type | Renders as | Notes |
|------|-----------|-------|
| `container` | `<div>` (or tag from `#tag`) wrapping children | No `#value`. Only children + attributes. |
| `markup` | `<#tag>#value</#tag>` | If `#format` present, value is filtered. If absent, escaped. |
| `table` | `<table>` | Children are rows. Special `#header` property for column headers. |
| `list` | `<ul>` or `<ol>` | Children are `<li>` items. `#list_type` = "ul" or "ol". |
| `link` | `<a href="#href">#value</a>` | `#href` property required. |

### 4.2 Rust Builder API

The `render` module provides a builder that produces the JSON schema above.

```rust
pub mod render {
    pub fn container() -> ElementBuilder;
    pub fn markup(tag: &str, value: &str) -> ElementBuilder;
    pub fn filtered_markup(value: &str, format: &str) -> ElementBuilder;
    pub fn link(href: &str, text: &str) -> ElementBuilder;
    pub fn table() -> TableBuilder;
    pub fn list(list_type: ListType) -> ListBuilder;
}

pub struct ElementBuilder { /* ... */ }

impl ElementBuilder {
    pub fn attr(self, key: &str, value: &str) -> Self;
    pub fn class(self, class: &str) -> Self;       // shorthand for attr("class", ...)
    pub fn weight(self, w: i32) -> Self;
    pub fn child(self, key: &str, element: RenderElement) -> Self;
    pub fn build(self) -> RenderElement;
}
```

**Usage:**

```rust
let element = render::container()
    .class("item--blog")
    .child("title", render::markup("h2", &title).weight(-10).build())
    .child("body", render::filtered_markup(&body.value, &body.format).build())
    .child("tags", render::container()
        .class("field--tags")
        .weight(10)
        .child("0", render::link("/categories/rust", "Rust").build())
        .child("1", render::link("/categories/wasm", "WASM").build())
        .build())
    .build();
```

### 4.3 Alter Semantics

`tap_item_view_alter` receives a mutable reference to the RenderElement tree. Plugins can:

- Add new children: `render.set_child("sidebar", element)`
- Remove children: `render.remove_child("tags")`
- Modify attributes: `render.add_class("promoted")`
- Change weight: `render.set_weight(100)`
- Access children: `render.get_child("title")`

Plugins **cannot** replace the entire tree (the function signature takes `&mut RenderElement`, not returning a new one). This prevents clobber bugs where one plugin overwrites another's changes.

---

## 5. Mutation Model

### 5.1 Tap Invocation Order

Taps are invoked in **weight order** (ascending) as declared in each plugin's `.info.toml`. Default weight is 0. Plugins with equal weight are invoked in load order (topological sort of dependencies).

### 5.2 Mutation Accumulation

When multiple plugins implement the same tap, **mutations accumulate**. Plugin B sees changes made by Plugin A.

For handle-based access, this is automatic: both plugins operate on the same `Item` in the Kernel's `RequestState`. When Plugin A calls `item.set_field("field_promoted", true)`, the field is written immediately in the Kernel. Plugin B's subsequent `item.get_field::<bool>("field_promoted")` returns `true`.

For full-serialization access, the Kernel applies Plugin A's returned JSON before passing the (now modified) Item to Plugin B.

### 5.3 Alter Taps

Alter taps (`tap_item_view_alter`, `tap_form_alter`) follow the same accumulation model. Plugin A's modifications to the RenderElement or Form are visible to Plugin B.

---

## 6. Host Functions (WIT Interface)

The authoritative WIT interface with handle-based data access.

```wit
package trovato:kernel;

// --- Item API (handle-based) ---
interface item-api {
    // Read
    get-title: func(item-handle: s32) -> string;
    get-field-string: func(item-handle: s32, field-name: string) -> option<string>;
    get-field-int: func(item-handle: s32, field-name: string) -> option<s64>;
    get-field-float: func(item-handle: s32, field-name: string) -> option<f64>;
    get-field-json: func(item-handle: s32, field-name: string) -> option<string>;
    get-type: func(item-handle: s32) -> string;
    get-id: func(item-handle: s32) -> string;           // was get-nid, returns UUID string
    get-revision-id: func(item-handle: s32) -> string;  // was get-vid, returns UUID string
    get-author-id: func(item-handle: s32) -> string;    // was get-uid, returns UUID string
    get-status: func(item-handle: s32) -> s32;
    get-created: func(item-handle: s32) -> s64;
    get-changed: func(item-handle: s32) -> s64;

    // Write
    set-title: func(item-handle: s32, value: string);
    set-field-string: func(item-handle: s32, field-name: string, value: string);
    set-field-int: func(item-handle: s32, field-name: string, value: s64);
    set-field-float: func(item-handle: s32, field-name: string, value: f64);
    set-field-json: func(item-handle: s32, field-name: string, value-json: string);
    set-status: func(item-handle: s32, value: s32);
}

// --- Database API (structured) ---
interface db {
    select: func(query-json: string) -> result<string, string>;
    insert: func(table: string, data-json: string) -> result<string, string>;  // returns UUID string (was s64)
    update: func(table: string, data-json: string, where-json: string) -> result<u64, string>;
    delete: func(table: string, where-json: string) -> result<u64, string>;

    // Permission-gated raw SQL (requires "raw_sql" in .info.toml permissions)
    query-raw: func(sql: string, params-json: string) -> result<string, string>;
    execute-raw: func(sql: string, params-json: string) -> result<u64, string>;
}

// --- Variables (persistent config) ---
interface variables {
    get: func(name: string, default-value: string) -> string;
    set: func(name: string, value: string) -> result<_, string>;
}

// --- Request context (per-request shared state) ---
interface request-context {
    get: func(key: string) -> option<string>;
    set: func(key: string, value: string);
}

// --- User API ---
interface user-api {
    current-user-has-permission: func(permission: string) -> bool;
    current-user-id: func() -> string;  // was current-user-uid, returns UUID string
}

// --- Cache API ---
interface cache-api {
    get: func(bin: string, key: string) -> option<string>;
    set: func(bin: string, key: string, value: string, tags-json: string);
    invalidate-tag: func(tag: string);
}

// --- Inter-plugin communication ---
interface plugin-api {
    invoke: func(plugin-name: string, function-name: string, payload: string)
        -> result<string, string>;
    plugin-exists: func(plugin-name: string) -> bool;
}

// --- Logging ---
interface logging {
    log: func(level: string, plugin: string, message: string);
}

// --- Plugin world ---
world plugin {
    import item-api;
    import db;
    import variables;
    import request-context;
    import user-api;
    import cache-api;
    import plugin-api;
    import logging;

    // Lifecycle
    export tap-install: func() -> result<_, string>;
    export tap-enable: func() -> result<_, string>;
    export tap-disable: func() -> result<_, string>;
    export tap-uninstall: func() -> result<_, string>;

    // Content type registration
    export tap-item-info: func() -> string;

    // Item CRUD (handle-based by default)
    export tap-item-view: func(item-handle: s32) -> string;
    export tap-item-view-alter: func(render-json: string, item-handle: s32) -> string;
    export tap-item-insert: func(item-handle: s32) -> result<_, string>;
    export tap-item-update: func(item-handle: s32) -> result<_, string>;
    export tap-item-delete: func(item-id: string) -> result<_, string>;
    export tap-item-access: func(item-handle: s32, op: string) -> string;

    // Full-serialization variants (opt-in via .info.toml)
    export tap-item-view-full: func(item-json: string) -> string;
    export tap-item-insert-full: func(item-json: string) -> result<_, string>;
    export tap-item-update-full: func(item-json: string) -> result<_, string>;

    // Categories
    export tap-categories-term-insert: func(term-json: string) -> result<_, string>;
    export tap-categories-term-update: func(term-json: string) -> result<_, string>;
    export tap-categories-term-delete: func(term-id: string) -> result<_, string>;

    // Forms
    export tap-form-alter: func(form-id: string, form-json: string) -> string;
    export tap-form-validate: func(form-id: string, values-json: string) -> string;
    export tap-form-submit: func(form-id: string, values-json: string) -> result<_, string>;

    // Routing & permissions
    export tap-menu: func() -> string;
    export tap-perm: func() -> string;

    // Theme
    export tap-theme: func() -> string;
    export tap-preprocess-item: func(context-json: string, item-handle: s32) -> string;

    // Search
    export tap-item-update-index: func(item-handle: s32) -> string;

    // Cron & queues
    export tap-cron: func() -> result<_, string>;
    export tap-queue-info: func() -> string;
    export tap-queue-worker: func(item-json: string) -> result<_, string>;

    // User lifecycle
    export tap-user-login: func(user-json: string) -> result<_, string>;
}
```
```

### 6.1 Database Query JSON Schema

The `select` host function accepts a structured JSON query, not raw SQL:

```json
{
    "table": "item",
    "fields": ["nid", "title", "fields->>'field_rating' as rating"],
    "conditions": [
        {"field": "type", "op": "=", "value": "blog"},
        {"field": "status", "op": "=", "value": 1}
    ],
    "order_by": [{"field": "created", "direction": "DESC"}],
    "limit": 25,
    "offset": 0,
    "joins": [
        {
            "table": "category_term_item",
            "on": "item.nid = category_term_item.nid",
            "type": "INNER"
        }
    ]
}
```

The Kernel translates this to SeaQuery, preventing SQL injection. Plugins cannot inject arbitrary SQL through this interface.

**Supported operators in `conditions[].op`:**

| Operator | Meaning | Value type |
|----------|---------|------------|
| `=` | Equals | scalar |
| `!=` | Not equals | scalar |
| `>` | Greater than | scalar (numeric for JSONB fields) |
| `<` | Less than | scalar |
| `>=` | Greater than or equal | scalar |
| `<=` | Less than or equal | scalar |
| `LIKE` | SQL LIKE pattern | string (use `%` wildcards) |
| `IN` | Value in list | array |
| `NOT IN` | Value not in list | array |
| `IS NULL` | Field is null | (no value needed) |
| `IS NOT NULL` | Field is not null | (no value needed) |
| `BETWEEN` | Between two values | array of `[min, max]` |

For JSONB fields referenced via `fields->>'field_name'`, the Kernel applies `::numeric` casts automatically when the operator is a numeric comparison and the value is a number.

**For queries too complex for the structured API**, plugins can request `raw_sql` permission in `.info.toml`:

```toml
[permissions]
requires = ["raw_sql"]
```

Administrators must explicitly grant this permission. Plugins with `raw_sql` access are treated as trusted.

### 6.2 Host Functions Not Yet Specified (Deferred)

These are recognized gaps to be designed as phases progress:

| Function | Phase | Notes |
|----------|-------|-------|
| File read/write | Phase 6 | Upload, download, generate URLs |
| Outbound HTTP | Post-MVP | Plugins calling external APIs |
| Time/Date | Phase 1 | `get_timestamp() -> i64` (trivial, add early) |
| Crypto primitives | Post-MVP | HMAC, hash, encrypt |
| Queue enqueue | Phase 1 | `queue_enqueue(queue_name, payload)` |

---

## 7. Error Handling

### 7.1 Return Types

| Tap type | Return | On error |
|----------|--------|----------|
| View taps | `RenderElement` | Return an error RenderElement (`render::markup("div", "Error: ...").class("error").build()`) |
| Mutation taps | `Result<(), String>` | Return `Err("description")`. Kernel logs and may abort the operation. |
| Info taps | `Vec<T>` serialized to JSON | Return empty `"[]"`. Kernel logs the failure. |
| Alter taps | Mutate in place | If the plugin panics, Kernel catches and continues with unaltered data. |

### 7.2 Guest Panics

If a plugin panics during a tap invocation:

1. Wasmtime traps the panic (WASM has no uncatchable exceptions)
2. The Kernel logs the error: `"Plugin 'blog' panicked during tap_item_view: {message}"`
3. For view taps: the Kernel skips this plugin's contribution and continues
4. For mutation taps: the Kernel aborts the operation and returns an error to the user
5. The plugin's Store is dropped (it was per-request anyway)

### 7.3 Host Function Errors

Host functions return `result<T, string>` in WIT. The SDK wraps these as `Result<T, PluginError>` in Rust. Plugin authors should handle errors or propagate them:

```rust
let results = ctx.db_select(&query)?;  // propagates error
// or
let results = ctx.db_select(&query).unwrap_or_default();  // fallback
```

---

## 8. Plugin Lifecycle

### 8.1 Discovery & Loading

1. Kernel reads `plugins/` directory at startup
2. For each subdirectory, reads `{name}.info.toml`
3. Checks `system` table for enabled plugins
4. Topological sort by dependencies
5. Compiles each `.wasm` file to a Wasmtime `Module` (one-time cost)
6. Builds `TapRegistry` from all enabled plugins' tap declarations

### 8.2 Per-Request Execution

1. Request arrives
2. Kernel creates a `RequestState` (db pool, redis, user, stage)
3. When a tap fires, Kernel lazily instantiates needed plugins (grabs Store from pool, ~5us)
4. Tap function called via WASM export
5. Plugin calls host functions as needed (each crosses boundary, ~1us each)
6. Plugin returns result; Kernel processes it
7. After all taps for this stage complete, Kernel proceeds to next stage
8. Request ends; all Stores returned to pool

### 8.3 Install / Uninstall

`tap_install` runs when a plugin is first enabled. Use it to create custom database tables:

```rust
#[plugin_tap]
fn install(ctx: &TapContext) -> Result<(), String> {
    ctx.db_execute_raw(
        "CREATE TABLE IF NOT EXISTS blog_settings (
            item_id UUID PRIMARY KEY REFERENCES item(id),
            allow_comments BOOLEAN DEFAULT true
        )", &[]
    )?;
    Ok(())
}
```

`tap_uninstall` cleans up. The Kernel confirms with the administrator before running it.

### 8.4 Enable / Disable

`tap_enable` runs each time a plugin is activated (including first install). `tap_disable` runs when deactivated. These are for runtime state (registering cron jobs, clearing caches), not schema changes.

---

## 9. Testing

### 9.1 Plugin Unit Tests

The `trovato_sdk_test` crate provides a mock Kernel for testing plugins without a running server:

```rust
#[cfg(test)]
mod tests {
    use trovato_sdk_test::MockKernel;

    #[test]
    fn test_blog_item_view() {
        let kernel = MockKernel::new();
        let item = kernel.create_item("blog", "Test Post", Uuid::now_v7());
        item.set_field("field_body", TextValue {
            value: "Hello world".into(),
            format: "filtered_html".into(),
        });

        let render = blog::item_view(&item.handle(), &kernel.context());

        assert_eq!(render.get_child("title").unwrap().value(), "Test Post");
    }
}
```

### 9.2 Integration Tests

Use the `test-utils` crate to spin up a real Kernel with Postgres and Redis:

```rust
#[tokio::test]
async fn test_blog_plugin_end_to_end() {
    let env = TestEnvironment::new()
        .with_plugin("blog")
        .with_plugin("categories")
        .build().await;

    let response = env.post("/item/add/blog", json!({
        "title": "Integration Test",
        "fields": { "field_body": { "value": "test", "format": "plain_text" } }
    })).await;

    assert_eq!(response.status(), 200);
    let item_id = response.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let item = env.load_item(item_id).await;
    assert_eq!(item.title, "Integration Test");
}
```

---

## 10. Complete Example: Blog Plugin

This is the reference implementation. Phase 2 begins by writing this file as a specification, then building the SDK to make it compile.

```rust
use trovato_sdk::prelude::*;

#[plugin_info]
mod blog {
    const NAME: &str = "blog";
    const DESCRIPTION: &str = "Provides a blog content type with tags";
    const VERSION: &str = "1.0.0";
    const DEPENDENCIES: &[&str] = &["item", "categories"];
}

#[plugin_tap]
fn item_info() -> Vec<ContentTypeDefinition> {
    vec![ContentTypeDefinition {
        machine_name: "blog".into(),
        label: "Blog Post".into(),
        description: "A blog entry with body and tags".into(),
        fields: vec![
            FieldDefinition::new("field_body", FieldType::TextLong)
                .required()
                .label("Body"),
            FieldDefinition::new("field_tags", FieldType::RecordReference("category_term".into()))
                .cardinality(-1)
                .label("Tags"),
        ],
    }]
}

#[plugin_tap]
fn item_view(item: &ItemHandle, ctx: &TapContext) -> RenderElement {
    let title = item.title();
    let body = item.get_field::<TextValue>("field_body");
    let tags = item.get_field::<Vec<RecordRef>>("field_tags")
        .unwrap_or_default();

    let mut element = render::container()
        .class("item--blog")
        .child("title", render::markup("h2", &title).weight(-10).build())
        .build();

    if let Some(body) = body {
        element.set_child("body",
            render::filtered_markup(&body.value, &body.format).build());
    }

    if !tags.is_empty() {
        let mut tags_container = render::container()
            .class("field--tags")
            .weight(10);
        for (i, tag_ref) in tags.iter().enumerate() {
            // In a real plugin, you'd look up the term name
            tags_container = tags_container.child(
                &i.to_string(),
                render::link(
                    &format!("/categories/{}", tag_ref.target_id),
                    &format!("Tag {}", tag_ref.target_id),
                ).build()
            );
        }
        element.set_child("tags", tags_container.build());
    }

    element
}

#[plugin_tap]
fn item_access(item: &ItemHandle, op: &str, ctx: &TapContext) -> AccessResult {
    match op {
        "view" => {
            if item.status() == 1 || item.author_id() == ctx.current_user_id() {
                AccessResult::Grant
            } else {
                AccessResult::Neutral
            }
        }
        "edit" => {
            if item.author_id() == ctx.current_user_id() && ctx.has_permission("edit own blog content") {
                AccessResult::Grant
            } else {
                AccessResult::Neutral
            }
        }
        _ => AccessResult::Neutral,
    }
}

#[plugin_tap]
fn menu() -> Vec<MenuDefinition> {
    vec![MenuDefinition {
        path: "blog".into(),
        title: "Blog".into(),
        callback: "blog_listing".into(),
        permission: "access content".into(),
        parent: None,
    }]
}

#[plugin_tap]
fn perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("create blog content", "Create new blog posts"),
        PermissionDefinition::new("edit own blog content", "Edit own blog posts"),
        PermissionDefinition::new("delete own blog content", "Delete own blog posts"),
    ]
}
```

---

## 11. Provisional Sections (May Change Post-Phase 0)

The following are specified based on the current handle-based design. If Phase 0 benchmarks show handle-based is not viable (>5x slower than expected), these sections will be rewritten to use full serialization as the default.

**Sections affected:** 3.1 (ItemHandle), 3.2 (FromField/IntoField), 6 (WIT interface item-api), and Section 5.2 of the mutation model (which assumes shared-state handles).

**Phase 0 benchmark requirements for handle-based to remain default:**
1. 500 calls where the plugin reads 3 fields via host functions, modifies 1, writes it back: **<250ms total** (0.5ms per call)
2. 100 concurrent requests each instantiating a plugin and calling a tap: **<50ms p95**
3. Handle-based should be **>5x faster** than full serialization for the same workload

If these targets are not met, the SDK falls back to full serialization and the `ItemHandle` type becomes a convenience wrapper over `Item` rather than a host-function proxy.
