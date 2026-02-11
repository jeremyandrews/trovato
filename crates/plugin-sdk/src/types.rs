use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque handle to an item stored in the Kernel's RequestState.
/// Plugins use this to call host functions (get_field, set_field, etc.)
/// without serializing the entire item across the WASM boundary.
#[derive(Debug, Clone, Copy)]
pub struct ItemHandle {
    handle: i32,
}

impl ItemHandle {
    pub fn from_raw(handle: i32) -> Self {
        Self { handle }
    }

    pub fn raw(&self) -> i32 {
        self.handle
    }
}

/// A text field value with its format (e.g., "filtered_html", "plain_text").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextValue {
    pub value: String,
    pub format: String,
}

/// A reference to another record (item, user, category term, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordRef {
    pub target_id: Uuid,
    pub target_type: String,
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
    pub required: bool,
    pub cardinality: i32,
    pub settings: serde_json::Value,
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
    Grant,
    Deny,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}
