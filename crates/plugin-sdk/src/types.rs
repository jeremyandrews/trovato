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

/// Field type definitions for content type registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    Text { max_length: Option<usize> },
    TextLong,
    Integer,
    Float,
    Boolean,
    RecordReference(String),
    File,
    Date,
    Email,
}

/// A content type definition returned by `tap_item_info`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentTypeDefinition {
    pub machine_name: String,
    pub label: String,
    pub description: String,
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
}

impl MenuDefinition {
    pub fn new(path: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            title: title.into(),
            callback: String::new(),
            permission: "access content".into(),
            parent: None,
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
}

/// Log levels for structured logging from plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}
