//! Core types for Trovato plugins.
//!
//! These types are used for communication between plugins and the kernel.
//! All tap functions use full-serialization (JSON in, JSON out).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Live stage UUID string, matching `LIVE_STAGE_ID` in the kernel.
///
/// Use this constant instead of hardcoding the UUID string in plugins
/// to stay in sync with the kernel's canonical definition.
pub const LIVE_STAGE_UUID: &str = "0193a5a0-0000-7000-8000-000000000001";

/// Returns the live stage UUID as a parsed `Uuid`.
///
/// # Panics
///
/// Panics if `LIVE_STAGE_UUID` is not a valid UUID (infallible with the hardcoded constant).
#[allow(clippy::expect_used)] // Infallible: parsing a hardcoded valid UUID constant
pub fn live_stage_id() -> Uuid {
    Uuid::parse_str(LIVE_STAGE_UUID).expect("LIVE_STAGE_UUID is a valid UUID")
}

/// A complete item (content record) for full-serialization taps.
///
/// Plugins receive this struct serialized as JSON for view/alter/insert/update taps.
///
/// SYNC: field names and types must match `crates/kernel/src/models/item.rs`.
/// The kernel serializes its `Item` via `serde_json::to_string()` and plugins
/// deserialize into this struct. Extra kernel fields (promote, sticky,
/// item_group_id) are ignored by serde. SDK-only helpers are fine as long as
/// they have `#[serde(default)]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// Unique identifier (UUIDv7, time-sortable).
    pub id: Uuid,

    /// Content type machine name (e.g., "blog", "page").
    #[serde(rename = "type")]
    pub item_type: String,

    /// Item title.
    pub title: String,

    /// Dynamic fields as key-value pairs.
    /// Values are JSON (can be TextValue, RecordRef, arrays, etc.).
    #[serde(default)]
    pub fields: HashMap<String, serde_json::Value>,

    /// Publication status (0 = unpublished, 1 = published).
    pub status: i32,

    /// Author user ID.
    pub author_id: Uuid,

    /// Current revision ID (null for items without revisions).
    #[serde(default)]
    pub current_revision_id: Option<Uuid>,

    /// Stage UUID referencing a stage category tag.
    #[serde(default = "live_stage_id")]
    pub stage_id: Uuid,

    /// Unix timestamp when created.
    pub created: i64,

    /// Unix timestamp when last changed.
    pub changed: i64,

    /// Language code (ISO 639-1, e.g., "en", "de", "ar").
    ///
    /// `None` for items created before language support or for
    /// language-neutral content. Plugins can read this to implement
    /// language-specific behavior.
    #[serde(default)]
    pub language: Option<String>,
}

impl Item {
    /// Get a field value as a specific type.
    pub fn get_field<T: for<'de> Deserialize<'de>>(&self, name: &str) -> Option<T> {
        self.fields
            .get(name)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Set a field value.
    pub fn set_field<T: Serialize>(&mut self, name: &str, value: T) {
        if let Ok(v) = serde_json::to_value(value) {
            self.fields.insert(name.to_string(), v);
        }
    }

    /// Get a text field's value string.
    pub fn get_text(&self, name: &str) -> Option<String> {
        self.get_field::<TextValue>(name).map(|tv| tv.value)
    }

    /// Get a text field with format info.
    pub fn get_text_value(&self, name: &str) -> Option<TextValue> {
        self.get_field(name)
    }
}

/// A text field value with its format (e.g., "filtered_html", "plain_text").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextValue {
    pub value: String,
    pub format: String,
}

impl TextValue {
    pub fn new(value: impl Into<String>, format: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            format: format.into(),
        }
    }

    /// Create plain text value.
    pub fn plain(value: impl Into<String>) -> Self {
        Self::new(value, "plain_text")
    }

    /// Create filtered HTML value.
    pub fn html(value: impl Into<String>) -> Self {
        Self::new(value, "filtered_html")
    }
}

/// A reference to another record (item, user, category term, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordRef {
    pub target_id: Uuid,
    pub target_type: String,
}

impl RecordRef {
    pub fn new(target_id: Uuid, target_type: impl Into<String>) -> Self {
        Self {
            target_id,
            target_type: target_type.into(),
        }
    }
}

/// Schema for a section type within a compound field.
/// Defined in FieldDefinition.settings.section_types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionTypeSchema {
    pub machine_name: String,
    pub label: String,
    pub fields: Vec<SectionFieldSchema>,
}

/// A field within a section type schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionFieldSchema {
    pub field_name: String,
    pub field_type: FieldType,
    pub label: String,
    #[serde(default)]
    pub required: bool,
}

/// A single section instance stored in JSONB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompoundSection {
    #[serde(rename = "type")]
    pub section_type: String,
    pub weight: i32,
    pub data: serde_json::Value,
}

/// Field type definitions for content type registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    Text {
        max_length: Option<usize>,
    },
    TextLong,
    Integer,
    Float,
    Boolean,
    RecordReference(String),
    File,
    Date,
    Email,
    Compound {
        allowed_types: Vec<String>,
        min_items: Option<usize>,
        max_items: Option<usize>,
    },
    /// An ordered array of content blocks rendered as HTML via `render_blocks()`.
    ///
    /// Storage format: JSON array of `{type, weight, data}` in JSONB `fields`.
    /// Block validation is handled by `BlockTypeRegistry`.
    Blocks,
    /// A Puck-format JSON component tree rendered via `render_puck_page()`.
    ///
    /// Storage format: `{"root": {...}, "content": [{type, props, zones}, ...]}`.
    /// Supports 12 component types with nested zones for visual page composition.
    PageBuilder,
}

/// A content type definition returned by `tap_item_info`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentTypeDefinition {
    pub machine_name: String,
    pub label: String,
    pub description: String,
    /// Custom label for the title field (e.g., "Conference Name" instead of "Title").
    #[serde(default)]
    pub title_label: Option<String>,
    pub fields: Vec<FieldDefinition>,
}

/// A single field definition within a content type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub field_name: String,
    pub field_type: FieldType,
    pub label: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "default_cardinality")]
    pub cardinality: i32,
    #[serde(default)]
    pub settings: serde_json::Value,

    /// Whether this field contains personally identifiable information (PII).
    ///
    /// When `true`, the field is included in GDPR data exports and flagged
    /// for deletion/anonymization. Default `false` for backward compatibility.
    #[serde(default)]
    pub personal_data: bool,
}

fn default_cardinality() -> i32 {
    1
}

impl FieldDefinition {
    pub fn new(name: &str, field_type: FieldType) -> Self {
        Self {
            field_name: name.into(),
            field_type,
            label: name.into(),
            required: false,
            cardinality: 1,
            settings: serde_json::Value::Object(Default::default()),
            personal_data: false,
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn label(mut self, label: &str) -> Self {
        self.label = label.into();
        self
    }

    pub fn cardinality(mut self, n: i32) -> Self {
        self.cardinality = n;
        self
    }
}

/// Input for `tap_item_access`.
///
/// Sent by the kernel when checking item access permissions. Contains the item
/// metadata, user context, and stage information needed for access decisions.
///
/// SYNC: An identical struct exists in `crates/kernel/src/content/item_service.rs`.
/// The kernel serializes its copy; plugins deserialize this one. Both must have
/// the same fields and serde attributes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemAccessInput {
    pub item_id: Uuid,
    pub item_type: String,
    pub author_id: Uuid,
    pub operation: String,
    pub user_id: Uuid,

    /// Whether the user is authenticated (false = anonymous).
    #[serde(default)]
    pub user_authenticated: bool,

    /// The user's granted permissions (empty for anonymous).
    #[serde(default)]
    pub user_permissions: Vec<String>,

    /// Stage UUID (None if item has no explicit stage).
    #[serde(default)]
    pub stage_id: Option<Uuid>,

    /// Stage machine name (e.g., "incoming", "curated", "live").
    #[serde(default)]
    pub stage_machine_name: Option<String>,
}

/// Access control result from `tap_item_access`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AccessResult {
    /// Explicitly grant access.
    Grant,
    /// Explicitly deny access.
    Deny,
    /// No opinion (let other plugins decide).
    Neutral,
}

/// Field-level access control result from `tap_field_access`.
///
/// Plugins return this to control per-field visibility. `Deny` wins
/// across all plugins (same aggregation as `AccessResult` for items).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldAccessResult {
    /// Allow access to this field.
    Allow,
    /// Deny access to this field.
    Deny,
    /// No opinion — let other plugins decide (default: allow).
    NoOpinion,
}

/// Field access operation type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldAccessOperation {
    /// Viewing the field value.
    View,
    /// Editing the field value.
    Edit,
}

/// The viewer context carried in a [`FieldAccessBatchInput`].
///
/// Mirrors the user fields the kernel already exposes to `tap_item_access`
/// (`user_id` / `authenticated` / `permissions`), nested here so the batch
/// payload reads as `{ "user": { … }, … }`.
///
/// SYNC: An identical struct exists in
/// `crates/kernel/src/content/item_service.rs`. The kernel serializes its copy;
/// plugins deserialize this one. Both must have the same fields and serde
/// attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldAccessUser {
    /// The viewer's user id (nil for anonymous).
    pub user_id: Uuid,
    /// Whether the viewer is authenticated (false = anonymous).
    #[serde(default)]
    pub authenticated: bool,
    /// The viewer's granted permissions (empty for anonymous).
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// Batch input for `tap_field_access` (the frozen 1.0 field-access payload).
///
/// A single dispatch carries the viewer, exactly **one** `item_type`, one
/// `operation` (`"view"` / `"edit"`, mirroring `ItemAccessInput.operation`), and
/// a **batch of field names**. The plugin returns a [`FieldAccessBatchResult`]
/// deciding every field in one call. Granularity is deliberately **type-level**:
/// a decision is a pure function of `(permissions, item_type, field, operation)`,
/// which is what lets the kernel batch per result-set-per-type and cache the
/// result. See design `fr-8-field-access-and-retrieval-layer.md` §2.
///
/// post-1.0: an additive optional `item` block
/// (`#[serde(default)] item: Option<FieldItemContext>`) extends this to per-item
/// granularity without breaking the frozen schema — old plugins ignore it.
///
/// SYNC: An identical struct exists in
/// `crates/kernel/src/content/item_service.rs`. The kernel serializes its copy;
/// plugins deserialize this one. Both must have the same fields and serde
/// attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldAccessBatchInput {
    /// The viewer context.
    pub user: FieldAccessUser,
    /// The single item type all `fields` belong to.
    pub item_type: String,
    /// The operation being gated: `"view"` or `"edit"`.
    pub operation: String,
    /// The batch of field names to decide — one dispatch decides all of them.
    pub fields: Vec<String>,
}

/// Batch result for `tap_field_access` (the frozen 1.0 field-access result).
///
/// Maps each requested field name to a [`FieldAccessResult`]. A field **absent**
/// from `decisions` is treated as [`FieldAccessResult::NoOpinion`] — a plugin
/// need only speak to fields it has an opinion on. The kernel aggregates
/// deny-wins across plugins with a **fail-open** default (see design §2.3).
///
/// SYNC: An identical struct exists in
/// `crates/kernel/src/content/item_service.rs`. Plugins serialize this one; the
/// kernel deserializes its copy. Both must have the same fields and serde
/// attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FieldAccessBatchResult {
    /// Per-field decision map. Absent field ⇒ `NoOpinion`.
    pub decisions: HashMap<String, FieldAccessResult>,
}

// ---- Account recovery (`tap_account_recovery`, FR-7c freeze gate) ------------
//
// The frozen 1.0 recovery contract. A plugin provides one or more recovery
// *methods*; the kernel owns the *flow* (account binding, flow nonce, expiry,
// single-use, rate limiting). One op-discriminated tap:
//
//   #[plugin_tap]
//   pub fn tap_account_recovery(input: RecoveryTapInput) -> RecoveryTapResult { … }
//
// **Op protocol** ([`RecoveryTapInput`], tagged by `op`):
//   - `describe` — what methods do you offer for this account?  → [`RecoveryTapResult::Methods`]
//   - `initiate` — the user chose method M; begin the challenge  → [`RecoveryTapResult::Initiated`]
//   - `verify`   — the user submitted response R; is it valid?   → [`RecoveryTapResult::Verdict`]
//
// **Verdict-only `verify` result.** [`RecoveryTapResult::Verdict`] carries a
// [`Verdict`] and NOTHING else — no `user_id`, no account, no session material —
// so a plugin cannot name a different account to escalate into, cannot mint a
// token, and cannot widen the flow. That structural absence is half the
// "cannot escalate" guarantee.
//
// **Namespaced `method_id`.** Every method id MUST be `<plugin_name>:<method>`.
// The kernel fold counts a `Verified` ONLY from the plugin owning the namespace
// (`method_id.starts_with("<plugin_name>:")`); any owner `Rejected` fails; no
// owner `Verified` denies; a trapped handler casts no vote. The fold is
// **fail-closed** (the deliberate inverse of field-access's fail-open), because
// recovery is the primary auth boundary. That owner-scope is the other half of
// the guarantee: a rogue plugin cannot forge approval on a method it does not
// own.
//
// **Transport note (bind the typed param, not `String`).** The WIT
// `tap-account-recovery: func(recovery-json: string) -> string` is
// transport-opaque only. The `#[plugin_tap]` macro deserializes the raw JSON
// *object* directly into your parameter type, so you MUST bind
// [`RecoveryTapInput`] (which deserializes from the object), **never** a literal
// `String` — binding `String` tries to parse a JSON object as a JSON string and
// fails into an `{"error":…}` output that the kernel fold treats as no vote
// (silently skipped). This is exactly the `FieldAccessBatchInput` pattern.

/// The account context the kernel passes to a recovery plugin.
///
/// `email_present` is a hint on `describe`/`initiate`; it is `#[serde(default)]`
/// so the leaner `verify` account (`{ "user_id": … }`) deserializes without it.
///
/// SYNC: An identical struct exists in `crates/kernel/src/recovery.rs`
/// (`RecoveryAccount`). The kernel serializes this; plugins deserialize it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAccount {
    /// The account being recovered.
    pub user_id: Uuid,
    /// Whether the account has a deliverable email (a `describe`/`initiate` hint).
    #[serde(default)]
    pub email_present: bool,
}

/// Input to `tap_account_recovery`, discriminated by `op`
/// (`describe` | `initiate` | `verify`). Bind THIS as your tap parameter — never
/// a literal `String` (see the module note above).
///
/// `flow_id` is a kernel-owned nonce opaque to the plugin; the plugin never
/// drives flow state.
///
/// SYNC: An identical enum exists in `crates/kernel/src/recovery.rs`
/// (`RecoveryTapInput`). The kernel serializes this; plugins deserialize it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RecoveryTapInput {
    /// "What methods do you offer for this account?"
    Describe {
        /// Kernel-owned flow nonce (opaque to the plugin).
        flow_id: String,
        /// The account being recovered.
        account: RecoveryAccount,
        /// The user's locale hint (e.g. `"en"`), if any.
        #[serde(default)]
        locale: Option<String>,
    },
    /// "The user chose method `method_id`; begin the challenge."
    Initiate {
        /// Kernel-owned flow nonce (opaque to the plugin).
        flow_id: String,
        /// The account being recovered.
        account: RecoveryAccount,
        /// The chosen method, namespaced `<plugin_name>:<method>`.
        method_id: String,
    },
    /// "The user submitted `response`; is it valid for this flow?"
    Verify {
        /// Kernel-owned flow nonce (opaque to the plugin).
        flow_id: String,
        /// The account being recovered.
        account: RecoveryAccount,
        /// The method being verified, namespaced `<plugin_name>:<method>`.
        method_id: String,
        /// The user-submitted token/code, opaque to the kernel.
        response: String,
    },
}

/// One recovery method a plugin advertises for an account (a `describe` result).
///
/// SYNC: An identical struct exists in `crates/kernel/src/recovery.rs`
/// (`RecoveryMethod`). Plugins serialize this; the kernel deserializes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryMethod {
    /// Namespaced `<plugin_name>:<method>`.
    pub method_id: String,
    /// Human-readable name for the method chooser.
    pub display_name: String,
    /// Whether the method is currently available for this account.
    pub available: bool,
}

/// The `verify` verdict — PascalCase on the wire. This is the *entire* payload a
/// plugin may return from `verify`: no account identifier, no session material.
///
/// SYNC: An identical enum exists in `crates/kernel/src/recovery.rs`
/// (`Verdict`). Serialized verbatim (`"Verified"` / `"Rejected"` / `"Pending"` /
/// `"NoOpinion"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// The response is valid; the owning plugin approves recovery.
    Verified,
    /// The response is invalid; the owning plugin rejects recovery.
    Rejected,
    /// Not yet decided; keep the flow open until the kernel TTL.
    Pending,
    /// No opinion on this method (folds to fail-closed if the only owner vote).
    NoOpinion,
}

/// Output of `tap_account_recovery`, discriminated by `result`
/// (`methods` | `initiated` | `verdict`).
///
/// SYNC: An identical enum exists in `crates/kernel/src/recovery.rs`
/// (`RecoveryTapResult`). Plugins serialize this; the kernel deserializes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RecoveryTapResult {
    /// Response to `describe`: the methods this plugin offers for the account.
    Methods {
        /// The advertised methods (namespaced ids).
        methods: Vec<RecoveryMethod>,
    },
    /// Response to `initiate`: the challenge has begun.
    Initiated {
        /// `"initiated"` or `"unavailable"`.
        status: String,
        /// A hint SAFE to display to the user — **no secrets**.
        challenge_hint: String,
        /// Advisory only; the kernel enforces its own TTL.
        expires_in_secs: u64,
    },
    /// Response to `verify`: a bare verdict and nothing else.
    Verdict {
        /// The verdict for the (flow, account, method) the kernel dispatched.
        verdict: Verdict,
    },
}

/// Menu route definition returned by `tap_menu`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuDefinition {
    pub path: String,
    pub title: String,
    pub callback: String,
    pub permission: String,
    pub parent: Option<String>,
    /// Whether this is a local task (tab-style navigation on entity pages).
    #[serde(default)]
    pub local_task: bool,
}

impl MenuDefinition {
    pub fn new(path: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            title: title.into(),
            callback: String::new(),
            permission: "access content".into(),
            parent: None,
            local_task: false,
        }
    }

    pub fn callback(mut self, callback: impl Into<String>) -> Self {
        self.callback = callback.into();
        self
    }

    pub fn permission(mut self, permission: impl Into<String>) -> Self {
        self.permission = permission.into();
        self
    }

    pub fn parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    pub fn local_task(mut self) -> Self {
        self.local_task = true;
        self
    }
}

// =============================================================================
// Plugin-served requests (`tap_api`) — added in KERNEL_API_VERSION (0,99)
// =============================================================================

/// A `tap_menu` entry that can be a navigation link **or** a plugin-served
/// route (**G-NO-PLUGIN-HTTP**, added in `KERNEL_API_VERSION (0,99)`).
///
/// [`MenuDefinition`] predates the plugin-served surface and carries only the
/// navigation half — no `method`, `handler_type` or `visible` — so it cannot
/// describe an `api` route. Those fields could not be *added* to it: every one
/// of its fields is public, so a struct literal built outside the SDK would
/// stop compiling, which is a MAJOR break of a frozen type. `MenuRoute` is
/// therefore a new type rather than three new fields, and the freeze holds.
///
/// The two **serialize to the same shape** the kernel's registry reads, so a
/// plugin can return either; `MenuDefinition` is still the right thing for a
/// plain navigation entry, and a plugin needing both returns
/// `Vec<serde_json::Value>`.
///
/// To be routed, an entry needs `handler_type = "api"` and a non-empty
/// `callback`. `permission` is the gate the kernel checks before dispatching to
/// `tap_api`; empty means public.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MenuRoute {
    /// URL path pattern; `:name` marks a parameter (`/x/:id`).
    pub path: String,
    /// Human-readable title.
    pub title: String,
    /// Handler name passed to `tap_api` as [`ApiRequest::callback`].
    #[serde(default)]
    pub callback: String,
    /// Required permission; empty is public.
    #[serde(default)]
    pub permission: String,
    /// Parent path, for navigation hierarchy.
    #[serde(default)]
    pub parent: Option<String>,
    /// Sort weight (lower sorts first).
    #[serde(default)]
    pub weight: i32,
    /// Whether the entry appears in navigation. An api route usually does not.
    #[serde(default)]
    pub visible: bool,
    /// HTTP method: `GET`, `POST`, `PUT` or `DELETE`.
    #[serde(default)]
    pub method: String,
    /// `"page"`, `"api"` or `"form"`. Only `"api"` is routed to `tap_api`.
    #[serde(default)]
    pub handler_type: String,
    /// Whether this is a local task (tab-style navigation).
    #[serde(default)]
    pub local_task: bool,
}

impl MenuRoute {
    /// A plugin-served route: `method` + `path` dispatched to `callback`.
    ///
    /// Defaults to invisible in navigation and public; call
    /// [`MenuRoute::permission`] to gate it, which any route that writes
    /// should.
    pub fn api(
        method: impl Into<String>,
        path: impl Into<String>,
        callback: impl Into<String>,
    ) -> Self {
        let path = path.into();
        Self {
            title: path.clone(),
            path,
            callback: callback.into(),
            permission: String::new(),
            parent: None,
            weight: 0,
            visible: false,
            method: method.into().to_uppercase(),
            handler_type: "api".into(),
            local_task: false,
        }
    }

    /// A plain navigation entry, equivalent to [`MenuDefinition::new`].
    pub fn page(path: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            title: title.into(),
            callback: String::new(),
            permission: "access content".into(),
            parent: None,
            weight: 0,
            visible: true,
            method: "GET".into(),
            handler_type: "page".into(),
            local_task: false,
        }
    }

    /// Set the human-readable title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Gate the entry on a permission. Empty means public.
    pub fn permission(mut self, permission: impl Into<String>) -> Self {
        self.permission = permission.into();
        self
    }

    /// Set the parent path for navigation hierarchy.
    pub fn parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Set the sort weight.
    pub fn weight(mut self, weight: i32) -> Self {
        self.weight = weight;
        self
    }

    /// Show the entry in navigation.
    pub fn visible(mut self) -> Self {
        self.visible = true;
        self
    }

    /// Mark the entry as a local task (tab-style navigation).
    pub fn local_task(mut self) -> Self {
        self.local_task = true;
        self
    }
}

impl From<MenuDefinition> for MenuRoute {
    fn from(menu: MenuDefinition) -> Self {
        Self {
            path: menu.path,
            title: menu.title,
            callback: menu.callback,
            permission: menu.permission,
            parent: menu.parent,
            weight: 0,
            visible: true,
            method: "GET".into(),
            handler_type: "page".into(),
            local_task: menu.local_task,
        }
    }
}

/// One HTTP request handed to a plugin's `tap_api` (**G-NO-PLUGIN-HTTP**).
///
/// Before `KERNEL_API_VERSION (0,99)` there was **no surface through which a
/// plugin served a request**, and therefore no way for an authenticated user to
/// write a plugin-owned table: [`MenuDefinition::callback`] was dropped on
/// deserialize, the form taps were never dispatched from any route, and
/// `tap_form_ajax` was admin-only and service-less. A `MenuDefinition` with
/// `handler_type = "api"` and a `callback` is now routed here.
///
/// The dispatch carries a live services handle and the authenticated
/// [`ApiRequest::user_id`], so the tap can write. The menu entry's `permission`
/// is the gate and is checked by the kernel before dispatch.
///
/// `#[non_exhaustive]`: additive fields may appear in a future minor release, so
/// construct with [`ApiRequest::new`] and match with `..`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ApiRequest {
    /// The `callback` name from the matched [`MenuDefinition`]. A plugin that
    /// registers several endpoints routes on this rather than on the path.
    pub callback: String,
    /// HTTP method, uppercase (`"GET"`, `"POST"`, …).
    pub method: String,
    /// The concrete request path, e.g. `/argus/story/<uuid>/react`.
    pub path: String,
    /// Path parameters extracted from the menu pattern: `/x/:id` over `/x/7`
    /// yields `{"id": "7"}`.
    #[serde(default)]
    pub params: HashMap<String, String>,
    /// Query-string parameters, last value winning on a repeated name.
    #[serde(default)]
    pub query: HashMap<String, String>,
    /// Request body, verbatim. Empty for a body-less method.
    #[serde(default)]
    pub body: String,
    /// The authenticated user's id as a uuid string, or the nil uuid for an
    /// anonymous caller — the same convention the gather layer's
    /// `ContextualValue::CurrentUser` uses.
    pub user_id: String,
    /// Whether the caller is authenticated. Distinguishes a real user from the
    /// anonymous account, which also holds the nil uuid.
    pub authenticated: bool,
    /// A single-use CSRF token for this caller's session, to render into a form
    /// the plugin serves.
    ///
    /// A plugin serving an HTML form has to put a valid token in it, and
    /// `tap_api` is one call with no way to ask the kernel for one. The kernel
    /// mints a token per request and hands it over here. Write it into a hidden
    /// `_token` input and the POST is accepted without any JavaScript:
    ///
    /// ```ignore
    /// format!(
    ///     r#"<form method="post" action="/contact">
    ///   <input type="hidden" name="_token" value="{token}">
    ///   <textarea name="message"></textarea>
    ///   <button type="submit">Send</button>
    /// </form>"#,
    ///     token = escape_html(&request.csrf_token),
    /// )
    /// ```
    ///
    /// Present on a POST as well as a GET: a token is single-use, so a submission
    /// that fails the plugin's own validation has already spent the one it
    /// arrived with, and re-rendering the form needs this one.
    ///
    /// **Empty when the caller authenticated with an `Authorization: Bearer`
    /// token**, which needs no CSRF token and is not being served a form. Treat
    /// empty as "nothing to render a token into" rather than as an error.
    ///
    /// Added at `KERNEL_API_VERSION (0,101)`; `#[serde(default)]`, so a payload
    /// from a kernel that does not send it deserializes with it empty.
    #[serde(default)]
    pub csrf_token: String,
}

impl ApiRequest {
    /// Construct a request. Optional fields default to empty.
    pub fn new(
        callback: impl Into<String>,
        method: impl Into<String>,
        path: impl Into<String>,
        user_id: impl Into<String>,
        authenticated: bool,
    ) -> Self {
        Self {
            callback: callback.into(),
            method: method.into(),
            path: path.into(),
            params: HashMap::new(),
            query: HashMap::new(),
            body: String::new(),
            user_id: user_id.into(),
            authenticated,
            csrf_token: String::new(),
        }
    }

    /// Parse the body as JSON.
    ///
    /// # Errors
    /// Returns the serde error when the body is not valid JSON of the target
    /// shape.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.body)
    }
}

/// What a plugin's `tap_api` returns for one request.
///
/// The kernel serves `status`, `headers` and `body` as-is. A plugin is
/// responsible for escaping anything it puts in an HTML body; the kernel does
/// not sanitize a plugin's response, and `content_type` defaults to JSON for
/// that reason.
///
/// `#[non_exhaustive]`: construct with the helpers below and match with `..`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ApiResponse {
    /// HTTP status code. A value outside 100..=599 is served as 500.
    pub status: u16,
    /// Response body.
    pub body: String,
    /// `Content-Type`. Defaults to `application/json`.
    #[serde(default = "default_api_content_type")]
    pub content_type: String,
}

fn default_api_content_type() -> String {
    "application/json".to_string()
}

impl ApiResponse {
    /// A `200` carrying a JSON body.
    ///
    /// # Errors
    /// Returns the serde error when `value` cannot be serialized.
    pub fn json<T: Serialize>(value: &T) -> Result<Self, serde_json::Error> {
        Ok(Self {
            status: 200,
            body: serde_json::to_string(value)?,
            content_type: default_api_content_type(),
        })
    }

    /// A response with an explicit status and JSON body.
    pub fn with_status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            content_type: default_api_content_type(),
        }
    }

    /// A `{"error": "…"}` body under the given status.
    pub fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            body: serde_json::json!({ "error": message }).to_string(),
            content_type: default_api_content_type(),
        }
    }

    /// Override the content type.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = content_type.into();
        self
    }
}

/// Permission definition returned by `tap_perm`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDefinition {
    pub name: String,
    pub description: String,
}

impl PermissionDefinition {
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }

    /// Generate standard view/create/edit/delete permissions for a content type.
    ///
    /// Produces 4 permissions matching the kernel's fallback format:
    /// - `"view {type} content"` — view unpublished items (published items use `"access content"`)
    /// - `"create {type} content"`
    /// - `"edit {type} content"`
    /// - `"delete {type} content"`
    pub fn crud_for_type(content_type: &str) -> Vec<Self> {
        vec![
            Self::new(
                &format!("view {content_type} content"),
                &format!("View unpublished {content_type} items"),
            ),
            Self::new(
                &format!("create {content_type} content"),
                &format!("Create new {content_type} items"),
            ),
            Self::new(
                &format!("edit {content_type} content"),
                &format!("Edit any {content_type} item"),
            ),
            Self::new(
                &format!("delete {content_type} content"),
                &format!("Delete any {content_type} item"),
            ),
        ]
    }
}

/// Input for `tap_cron`.
///
/// Sent by the kernel during each cron cycle to plugins that implement
/// the `tap_cron` hook. Plugins can use the timestamp to implement
/// interval-based scheduling (e.g., "run only every 5 minutes").
///
/// SYNC: The kernel serializes this as `{"timestamp": <unix_ts>}` in
/// `crates/kernel/src/cron/mod.rs`. Both sides must agree on the format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronInput {
    /// Unix timestamp (seconds) when the cron cycle started.
    pub timestamp: i64,
}

/// Options for the additive `enqueue` queue host function (P11d / D-48).
///
/// Serializes to the `opts` JSON the kernel parses. Both fields default to 0,
/// so [`QueueOptions::default`] makes [`crate::host::queue_enqueue`] behave like
/// [`crate::host::queue_push`] (priority 0, no delay).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueOptions {
    /// Dispatch priority — higher values are drained first (default 0).
    #[serde(default)]
    pub priority: i32,
    /// Seconds to defer the first attempt (default 0 = eligible immediately).
    #[serde(default)]
    pub delay: i64,
}

/// An outbound HTTP request made through the kernel's HTTP host function.
///
/// Plugins cannot make direct network calls from WASM. Instead, they build
/// an `HttpRequest` and pass it to [`crate::host::http_request`], which the
/// kernel executes on the plugin's behalf with configurable timeouts and
/// security restrictions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    /// Full URL to request (must be `https://` or `http://`).
    pub url: String,
    /// HTTP method (GET, POST, PUT, DELETE, etc.).
    #[serde(default = "default_http_method")]
    pub method: String,
    /// Request headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Optional request body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Request timeout in milliseconds (default: 30000, max: 60000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
}

fn default_http_method() -> String {
    "GET".to_string()
}

impl HttpRequest {
    /// Create a GET request to the given URL.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: "GET".to_string(),
            headers: HashMap::new(),
            body: None,
            timeout_ms: None,
        }
    }

    /// Create a POST request to the given URL with a body.
    pub fn post(url: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: "POST".to_string(),
            headers: HashMap::new(),
            body: Some(body.into()),
            timeout_ms: None,
        }
    }

    /// Add a header to the request.
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Set the request timeout in milliseconds.
    pub fn timeout(mut self, ms: u32) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

/// Response from an HTTP request made through the kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// Response body as a string.
    pub body: String,
}

/// Response metadata returned when opening a streaming HTTP fetch through the
/// kernel (`http-open`, p11j / G-HTTP-META).
///
/// Carries the streaming handle plus the response `status` and `headers` in the
/// **same representation** as the one-shot [`HttpResponse`] (lowercased header
/// names, last value winning on a repeated name), so a streaming consumer can do
/// conditional GET — distinguish a `304 Not Modified` from an empty `200`, read
/// back a fresh `ETag`/`Last-Modified`, dispatch on `Content-Type`, preallocate
/// from `Content-Length` — on the streaming path. The body is streamed
/// separately via [`crate::host::http_read`], so it is absent here.
///
/// `#[non_exhaustive]`: additive fields may appear in a future minor release, so
/// construct with [`HttpOpenResponse::new`] and match with `..`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HttpOpenResponse {
    /// Opaque, `Store`-scoped streaming handle for [`crate::host::http_read`] and
    /// [`crate::host::http_close`].
    pub handle: u32,
    /// HTTP status code (identical representation to [`HttpResponse::status`]).
    pub status: u16,
    /// Response headers (identical representation to [`HttpResponse::headers`]).
    pub headers: HashMap<String, String>,
}

impl HttpOpenResponse {
    /// Construct an open-response from a handle and its captured response
    /// metadata.
    pub fn new(handle: u32, status: u16, headers: HashMap<String, String>) -> Self {
        Self {
            handle,
            status,
            headers,
        }
    }
}

/// Log levels for structured logging from plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

// =============================================================================
// AI types — shared between kernel and plugins for `ai_request()` host function
// =============================================================================

/// The kind of AI operation to perform.
///
/// Must use the same `snake_case` serde representation as the kernel's
/// `AiOperationType` so JSON is wire-compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiOperationType {
    /// Conversational / completion.
    Chat,
    /// Text embedding.
    Embedding,
    /// Image generation.
    ImageGeneration,
    /// Speech-to-text transcription.
    SpeechToText,
    /// Text-to-speech synthesis.
    TextToSpeech,
    /// Content moderation.
    Moderation,
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiMessage {
    /// Message role: `"system"`, `"user"`, or `"assistant"`.
    pub role: String,
    /// Message content.
    pub content: String,
}

impl AiMessage {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    /// Create an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

/// Options for controlling AI request behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiRequestOptions {
    /// Maximum tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0 = deterministic, 2.0 = very random).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top-p nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// A request to the AI provider, serialized as JSON for the `ai_request()` host function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRequest {
    /// Operation type (determines which provider/model is used).
    pub operation: AiOperationType,
    /// Optional provider ID override (uses site default if `None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Optional model override (uses provider's configured model if `None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Chat messages (for Chat operation).
    #[serde(default)]
    pub messages: Vec<AiMessage>,
    /// Input text (for Embedding, Moderation, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Request options.
    #[serde(default)]
    pub options: AiRequestOptions,
}

/// Token usage statistics from the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiUsage {
    /// Tokens used in the prompt/input.
    pub prompt_tokens: u32,
    /// Tokens generated in the response.
    pub completion_tokens: u32,
    /// Total tokens (prompt + completion).
    pub total_tokens: u32,
}

/// Normalized response from an AI provider.
///
/// `#[non_exhaustive]`: additive fields may appear in a future minor release
/// (`cost_estimate` was one, added pre-1.0), so construct with
/// [`AiResponse::new`] and match with `..`. Applied at the 1.0 freeze boundary
/// so post-1.0 additions stay minor under cargo-semver-checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AiResponse {
    /// Generated text content.
    pub content: String,
    /// Model that was actually used.
    pub model: String,
    /// Token usage statistics.
    pub usage: AiUsage,
    /// Round-trip latency in milliseconds.
    pub latency_ms: u64,
    /// Reason the generation stopped (e.g., `"stop"`, `"length"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Estimated dollar cost of this call (G-COST-OPAQUE, p11j): the same figure
    /// the kernel writes to `ai_usage_log.cost_estimate`, computed from the model
    /// and token counts. `None` when the model is unpriced or no pricing is
    /// configured — the honest "unknown", distinct from a genuine `Some(0.0)`.
    /// The kernel populates it; on native test builds it is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_estimate: Option<f64>,
}

impl AiResponse {
    /// Construct a response from a provider parse, with no cost estimate. The
    /// kernel sets [`AiResponse::cost_estimate`] separately after its pricing
    /// lookup; other constructors (native test stub) leave it `None`.
    pub fn new(
        content: String,
        model: String,
        usage: AiUsage,
        latency_ms: u64,
        finish_reason: Option<String>,
    ) -> Self {
        Self {
            content,
            model,
            usage,
            latency_ms,
            finish_reason,
            cost_estimate: None,
        }
    }
}

/// Data contributed by a plugin to a user's GDPR data export.
///
/// Plugins implementing `tap_user_export` return this with their
/// plugin-specific data for the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserExportData {
    /// Name of the plugin contributing data.
    pub plugin_name: String,

    /// Human-readable label for this data category.
    pub data_type: String,

    /// The actual records (arbitrary JSON objects).
    pub records: Vec<serde_json::Value>,
}

/// Context passed to `tap_ai_request` for governance policy decisions.
///
/// Plugins implementing `tap_ai_request` use this to decide whether to
/// allow, modify, or deny an AI request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRequestContext {
    /// User who initiated the request.
    pub user_id: Uuid,

    /// Plugin that called `ai_request()`.
    pub plugin_name: String,

    /// Type of AI operation (Chat, Embedding, etc.).
    pub operation_type: AiOperationType,

    /// Item ID if the request is content-related (e.g., field enrichment).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<Uuid>,

    /// Field name if the request is a field rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_name: Option<String>,
}

/// Decision from a `tap_ai_request` handler.
///
/// Uses deny-wins aggregation: if any plugin returns `Deny`, the request
/// is blocked regardless of other plugins' decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AiRequestDecision {
    /// Allow the request as-is.
    Allow,
    /// Allow the request after modifications made by the tap handler.
    AllowModified,
    /// Deny the request with a reason.
    Deny(String),
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn cron_input_round_trip() {
        let input = CronInput {
            timestamp: 1_700_000_000,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert_eq!(json, r#"{"timestamp":1700000000}"#);

        let parsed: CronInput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.timestamp, 1_700_000_000);
    }

    #[test]
    fn cron_input_deserializes_from_kernel_format() {
        // The kernel serializes CronInput directly; plugins must be able to parse it
        let kernel_json = r#"{"timestamp":1234567890}"#;
        let input: CronInput = serde_json::from_str(kernel_json).unwrap();
        assert_eq!(input.timestamp, 1_234_567_890);
    }

    // ---- HTTP types ----

    #[test]
    fn http_request_get_builder() {
        let req = HttpRequest::get("https://example.com/api")
            .header("Accept", "application/json")
            .timeout(5000);
        assert_eq!(req.method, "GET");
        assert_eq!(req.url, "https://example.com/api");
        assert_eq!(req.headers.get("Accept").unwrap(), "application/json");
        assert_eq!(req.timeout_ms, Some(5000));
        assert!(req.body.is_none());
    }

    #[test]
    fn http_request_post_builder() {
        let req = HttpRequest::post("https://example.com/api", r#"{"key":"value"}"#);
        assert_eq!(req.method, "POST");
        assert_eq!(req.body.as_deref(), Some(r#"{"key":"value"}"#));
    }

    #[test]
    fn http_request_serde_roundtrip() {
        let req = HttpRequest::get("https://example.com")
            .header("X-Custom", "test")
            .timeout(10000);
        let json = serde_json::to_string(&req).unwrap();
        let back: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url, "https://example.com");
        assert_eq!(back.method, "GET");
        assert_eq!(back.headers.get("X-Custom").unwrap(), "test");
        assert_eq!(back.timeout_ms, Some(10000));
    }

    #[test]
    fn http_response_serde_roundtrip() {
        let resp = HttpResponse {
            status: 200,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: r#"[{"id":1}]"#.to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: HttpResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, 200);
        assert_eq!(back.body, r#"[{"id":1}]"#);
    }

    #[test]
    fn http_request_default_method_is_get() {
        let json = r#"{"url":"https://example.com"}"#;
        let req: HttpRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "GET");
    }

    // ---- AI types serde roundtrips ----

    #[test]
    fn ai_operation_type_serde_roundtrip() {
        let ops = [
            (AiOperationType::Chat, "\"chat\""),
            (AiOperationType::Embedding, "\"embedding\""),
            (AiOperationType::ImageGeneration, "\"image_generation\""),
            (AiOperationType::SpeechToText, "\"speech_to_text\""),
            (AiOperationType::TextToSpeech, "\"text_to_speech\""),
            (AiOperationType::Moderation, "\"moderation\""),
        ];
        for (op, expected_json) in ops {
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(json, expected_json, "serialize {op:?}");
            let back: AiOperationType = serde_json::from_str(&json).unwrap();
            assert_eq!(op, back);
        }
    }

    #[test]
    fn ai_request_serde_roundtrip() {
        let req = AiRequest {
            operation: AiOperationType::Chat,
            provider_id: None,
            model: Some("gpt-4o".to_string()),
            messages: vec![
                AiMessage::system("You are helpful."),
                AiMessage::user("Hello"),
            ],
            input: None,
            options: AiRequestOptions {
                max_tokens: Some(200),
                temperature: Some(0.3),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: AiRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.operation, AiOperationType::Chat);
        assert_eq!(back.model.as_deref(), Some("gpt-4o"));
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[0].role, "system");
        assert_eq!(back.options.max_tokens, Some(200));
    }

    #[test]
    fn ai_response_serde_roundtrip() {
        let mut resp = AiResponse::new(
            "Hello!".to_string(),
            "gpt-4o".to_string(),
            AiUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            234,
            Some("stop".to_string()),
        );
        // Default: no cost estimate (constructor leaves it None, and the field is
        // omitted from the JSON when None).
        assert_eq!(resp.cost_estimate, None);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("cost_estimate"),
            "a None cost_estimate must be omitted from the JSON"
        );
        let back: AiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content, "Hello!");
        assert_eq!(back.usage.total_tokens, 15);
        assert_eq!(back.finish_reason.as_deref(), Some("stop"));
        assert_eq!(back.cost_estimate, None);

        // A populated cost estimate (G-COST-OPAQUE) round-trips.
        resp.cost_estimate = Some(0.0125);
        let json = serde_json::to_string(&resp).unwrap();
        let back: AiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cost_estimate, Some(0.0125));
    }

    #[test]
    fn ai_request_options_default_is_empty() {
        let opts = AiRequestOptions::default();
        let json = serde_json::to_string(&opts).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn ai_message_constructors() {
        let sys = AiMessage::system("sys");
        assert_eq!(sys.role, "system");
        assert_eq!(sys.content, "sys");

        let user = AiMessage::user("usr");
        assert_eq!(user.role, "user");

        let asst = AiMessage::assistant("asst");
        assert_eq!(asst.role, "assistant");
    }

    // ---- Item language field ----

    #[test]
    fn item_language_round_trip() {
        // Kernel sends language as a string — SDK receives as Option<String>
        let kernel_json = r#"{
            "id": "01234567-89ab-cdef-0123-456789abcdef",
            "type": "blog",
            "title": "Test",
            "fields": {},
            "status": 1,
            "author_id": "01234567-89ab-cdef-0123-456789abcdef",
            "stage_id": "0193a5a0-0000-7000-8000-000000000001",
            "created": 1700000000,
            "changed": 1700000000,
            "language": "de"
        }"#;
        let item: Item = serde_json::from_str(kernel_json).unwrap();
        assert_eq!(item.language, Some("de".to_string()));

        // Round-trip back to JSON includes language
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""language":"de""#));
    }

    #[test]
    fn item_missing_language_defaults_to_none() {
        // Old kernel data without language field — backward compatible
        let old_json = r#"{
            "id": "01234567-89ab-cdef-0123-456789abcdef",
            "type": "blog",
            "title": "Old Item",
            "fields": {},
            "status": 1,
            "author_id": "01234567-89ab-cdef-0123-456789abcdef",
            "stage_id": "0193a5a0-0000-7000-8000-000000000001",
            "created": 1700000000,
            "changed": 1700000000
        }"#;
        let item: Item = serde_json::from_str(old_json).unwrap();
        assert_eq!(item.language, None);
    }

    // ---- SDK Backward Compatibility (Story 48.5) ----
    //
    // These tests verify that JSON payloads serialized by an older version
    // of the SDK (without new fields) can still be deserialized by the
    // current SDK. This is the contract that lets compiled WASM plugins
    // continue working without recompilation.

    #[test]
    fn backward_compat_item_without_new_fields() {
        // Simulates an old plugin binary that doesn't know about
        // `language` — the field should default to None.
        let old_json = r#"{
            "id": "01234567-89ab-cdef-0123-456789abcdef",
            "type": "conference",
            "title": "RustConf",
            "fields": {"field_city": "Portland"},
            "status": 1,
            "author_id": "01234567-89ab-cdef-0123-456789abcdef",
            "stage_id": "0193a5a0-0000-7000-8000-000000000001",
            "created": 1700000000,
            "changed": 1700000000
        }"#;
        let item: Item = serde_json::from_str(old_json).unwrap();
        assert_eq!(
            item.language, None,
            "missing language should default to None"
        );
        assert_eq!(item.item_type, "conference");
        assert_eq!(item.title, "RustConf");
    }

    #[test]
    fn backward_compat_item_with_new_fields() {
        // Simulates the kernel sending an Item with all new fields to a
        // plugin. The plugin should be able to deserialize it even if
        // the plugin's struct doesn't have these fields (serde ignores
        // unknown fields by default).
        let new_json = r#"{
            "id": "01234567-89ab-cdef-0123-456789abcdef",
            "type": "blog",
            "title": "Test",
            "fields": {},
            "status": 1,
            "author_id": "01234567-89ab-cdef-0123-456789abcdef",
            "stage_id": "0193a5a0-0000-7000-8000-000000000001",
            "created": 1700000000,
            "changed": 1700000000,
            "language": "de",
            "unknown_future_field": "should be ignored"
        }"#;
        let item: Item = serde_json::from_str(new_json).unwrap();
        assert_eq!(item.language, Some("de".to_string()));
        // unknown_future_field is silently ignored — no deny_unknown_fields
    }

    #[test]
    fn backward_compat_field_definition_without_personal_data() {
        // Old FieldDefinition JSON without `personal_data` — defaults to false.
        let old_json = r#"{
            "field_name": "body",
            "field_type": "TextLong",
            "label": "Body",
            "required": true,
            "cardinality": 1,
            "settings": {}
        }"#;
        let def: FieldDefinition = serde_json::from_str(old_json).unwrap();
        assert!(
            !def.personal_data,
            "missing personal_data should default to false"
        );
        assert!(def.required);
    }

    #[test]
    fn backward_compat_field_definition_with_personal_data() {
        let new_json = r#"{
            "field_name": "email",
            "field_type": "Email",
            "label": "Email Address",
            "required": false,
            "cardinality": 1,
            "settings": {},
            "personal_data": true
        }"#;
        let def: FieldDefinition = serde_json::from_str(new_json).unwrap();
        assert!(def.personal_data);
    }

    #[test]
    fn no_deny_unknown_fields_on_item() {
        // Verify Item does not use #[serde(deny_unknown_fields)] which
        // would break backward compatibility when the kernel adds fields.
        let json_with_extras = r#"{
            "id": "01234567-89ab-cdef-0123-456789abcdef",
            "type": "page",
            "title": "Test",
            "fields": {},
            "status": 1,
            "author_id": "01234567-89ab-cdef-0123-456789abcdef",
            "stage_id": "0193a5a0-0000-7000-8000-000000000001",
            "created": 0,
            "changed": 0,
            "completely_new_field_from_future": 42,
            "another_future_field": {"nested": true}
        }"#;
        // This must not panic — unknown fields are silently ignored.
        let result: Result<Item, _> = serde_json::from_str(json_with_extras);
        assert!(
            result.is_ok(),
            "Item must accept unknown fields: {result:?}"
        );
    }

    #[test]
    fn no_deny_unknown_fields_on_field_definition() {
        let json_with_extras = r#"{
            "field_name": "test",
            "field_type": "TextLong",
            "label": "Test",
            "required": false,
            "cardinality": 1,
            "settings": {},
            "future_retention_policy": "archive",
            "future_ai_enrichment": true
        }"#;
        let result: Result<FieldDefinition, _> = serde_json::from_str(json_with_extras);
        assert!(
            result.is_ok(),
            "FieldDefinition must accept unknown fields: {result:?}"
        );
    }

    // ---- Field-access batch contract (FR-8 Story 3.1, freeze-relevant) ----

    #[test]
    fn field_access_batch_input_matches_frozen_schema() {
        // The exact frozen wire shape from design §2.2 must round-trip.
        let input = FieldAccessBatchInput {
            user: FieldAccessUser {
                user_id: Uuid::nil(),
                authenticated: true,
                permissions: vec!["access content".to_string()],
            },
            item_type: "article".to_string(),
            operation: "view".to_string(),
            fields: vec!["ssn".to_string(), "salary".to_string()],
        };
        let json = serde_json::to_string(&input).unwrap();
        // Nested user object, top-level item_type/operation/fields.
        assert!(json.contains(r#""user":{"#), "user must nest: {json}");
        assert!(json.contains(r#""item_type":"article""#), "{json}");
        assert!(json.contains(r#""operation":"view""#), "{json}");
        assert!(json.contains(r#""fields":["ssn","salary"]"#), "{json}");
        let back: FieldAccessBatchInput = serde_json::from_str(&json).unwrap();
        assert_eq!(back, input);
    }

    #[test]
    fn field_access_batch_result_matches_frozen_schema() {
        let mut decisions = HashMap::new();
        decisions.insert("ssn".to_string(), FieldAccessResult::Deny);
        decisions.insert("salary".to_string(), FieldAccessResult::Allow);
        decisions.insert("notes".to_string(), FieldAccessResult::NoOpinion);
        let result = FieldAccessBatchResult { decisions };
        let json = serde_json::to_string(&result).unwrap();
        // Tri-state values serialize with their frozen names.
        assert!(json.contains(r#""ssn":"Deny""#), "{json}");
        assert!(json.contains(r#""salary":"Allow""#), "{json}");
        assert!(json.contains(r#""notes":"NoOpinion""#), "{json}");
        let back: FieldAccessBatchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, result);
    }

    #[test]
    fn field_access_result_absent_field_is_noopinion_by_omission() {
        // A plugin need only speak to fields it has an opinion on; the kernel
        // treats an omitted field as NoOpinion. Prove a sparse map round-trips.
        let json = r#"{"decisions":{"ssn":"Deny"}}"#;
        let result: FieldAccessBatchResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.decisions.get("ssn"), Some(&FieldAccessResult::Deny));
        assert!(!result.decisions.contains_key("salary"));
    }

    #[test]
    fn field_access_user_anonymous_defaults() {
        // Anonymous viewer: only user_id present; authenticated/permissions default.
        let json = r#"{"user_id":"00000000-0000-0000-0000-000000000000"}"#;
        let user: FieldAccessUser = serde_json::from_str(json).unwrap();
        assert!(!user.authenticated);
        assert!(user.permissions.is_empty());
    }
}
