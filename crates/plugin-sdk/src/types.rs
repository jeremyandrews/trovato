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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let resp = AiResponse {
            content: "Hello!".to_string(),
            model: "gpt-4o".to_string(),
            usage: AiUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            latency_ms: 234,
            finish_reason: Some("stop".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content, "Hello!");
        assert_eq!(back.usage.total_tokens, 15);
        assert_eq!(back.finish_reason.as_deref(), Some("stop"));
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
}
