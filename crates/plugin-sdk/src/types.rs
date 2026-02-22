//! Core types for Trovato plugins.
//!
//! These types are used for communication between plugins and the kernel.
//! All tap functions use full-serialization (JSON in, JSON out).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A complete item (content record) for full-serialization taps.
///
/// Plugins receive this struct serialized as JSON for view/alter/insert/update taps.
/// Phase 0 benchmarks proved full-serialization is 1.2-1.6x faster than handle-based
/// field access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// Unique identifier (UUIDv7, time-sortable).
    pub id: Uuid,

    /// Content type machine name (e.g., "blog", "page").
    pub item_type: String,

    /// Item title.
    pub title: String,

    /// Dynamic fields as key-value pairs.
    /// Values are JSON (can be TextValue, RecordRef, arrays, etc.).
    pub fields: HashMap<String, serde_json::Value>,

    /// Publication status (0 = unpublished, 1 = published).
    pub status: i32,

    /// Author user ID.
    pub author_id: Uuid,

    /// Revision ID for staged content.
    #[serde(default)]
    pub revision_id: Option<Uuid>,

    /// Stage ID (None = live).
    #[serde(default)]
    pub stage_id: Option<String>,

    /// Unix timestamp when created.
    pub created: i64,

    /// Unix timestamp when last changed.
    pub changed: i64,
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
/// Sent by the kernel when checking item access permissions. Contains only
/// the fields needed for access decisions — not the full Item.
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

/// Log levels for structured logging from plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
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
}
