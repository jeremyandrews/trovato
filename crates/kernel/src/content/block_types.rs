//! Block type registry and server-side block validation (Epics 24.1 & 24.2).
//!
//! Provides:
//! - `BlockTypeDefinition`: Schema and metadata for a single block type
//! - `BlockTypeRegistry`: Registry of all known block types with validation
//! - `sanitize_html`: HTML sanitization via ammonia
//! - `sanitize_blocks`: In-place sanitization of text content across block arrays

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Definition of a single block type in the editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockTypeDefinition {
    /// Machine name of the block type (e.g. "paragraph", "heading").
    pub type_name: String,
    /// Human-readable label (e.g. "Paragraph", "Heading").
    pub label: String,
    /// JSON Schema describing the expected data shape.
    pub schema: Value,
    /// Text formats this block can use (e.g. "filtered_html", "plain_text").
    pub allowed_formats: Vec<String>,
    /// Plugin that provides this block type.
    pub plugin: String,
}

/// Registry of block type definitions, keyed by type name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockTypeRegistry {
    types: HashMap<String, BlockTypeDefinition>,
}

impl Default for BlockTypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockTypeRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
        }
    }

    /// Create a registry pre-populated with the 8 standard block types.
    pub fn with_standard_types() -> Self {
        let mut registry = Self::new();
        registry.register_standard_types();
        registry
    }

    /// Register a single block type definition.
    pub fn register(&mut self, definition: BlockTypeDefinition) {
        self.types.insert(definition.type_name.clone(), definition);
    }

    /// Look up a block type by name.
    pub fn get(&self, type_name: &str) -> Option<&BlockTypeDefinition> {
        self.types.get(type_name)
    }

    /// Check whether a block type is registered.
    pub fn contains(&self, type_name: &str) -> bool {
        self.types.contains_key(type_name)
    }

    /// Return the number of registered block types.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// List all registered type names.
    pub fn type_names(&self) -> Vec<String> {
        self.types.keys().cloned().collect()
    }

    /// Register the 8 standard block types: paragraph, heading, image, list,
    /// quote, code, delimiter, embed.
    pub fn register_standard_types(&mut self) {
        self.register(BlockTypeDefinition {
            type_name: "paragraph".to_string(),
            label: "Paragraph".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
            allowed_formats: vec!["filtered_html".to_string(), "plain_text".to_string()],
            plugin: "core".to_string(),
        });

        self.register(BlockTypeDefinition {
            type_name: "heading".to_string(),
            label: "Heading".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "level": { "type": "integer", "minimum": 1, "maximum": 6 }
                },
                "required": ["text", "level"]
            }),
            allowed_formats: vec!["filtered_html".to_string(), "plain_text".to_string()],
            plugin: "core".to_string(),
        });

        self.register(BlockTypeDefinition {
            type_name: "image".to_string(),
            label: "Image".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": {
                        "type": "object",
                        "properties": {
                            "url": { "type": "string", "minLength": 1 }
                        },
                        "required": ["url"]
                    },
                    "caption": { "type": "string" },
                    "alt": { "type": "string" }
                },
                "required": ["file"]
            }),
            allowed_formats: vec![],
            plugin: "core".to_string(),
        });

        self.register(BlockTypeDefinition {
            type_name: "list".to_string(),
            label: "List".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "style": { "type": "string", "enum": ["ordered", "unordered"] },
                    "items": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["style", "items"]
            }),
            allowed_formats: vec!["filtered_html".to_string(), "plain_text".to_string()],
            plugin: "core".to_string(),
        });

        self.register(BlockTypeDefinition {
            type_name: "quote".to_string(),
            label: "Quote".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "caption": { "type": "string" }
                },
                "required": ["text"]
            }),
            allowed_formats: vec!["filtered_html".to_string(), "plain_text".to_string()],
            plugin: "core".to_string(),
        });

        self.register(BlockTypeDefinition {
            type_name: "code".to_string(),
            label: "Code".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string" },
                    "language": { "type": "string" }
                },
                "required": ["code"]
            }),
            allowed_formats: vec![],
            plugin: "core".to_string(),
        });

        self.register(BlockTypeDefinition {
            type_name: "delimiter".to_string(),
            label: "Delimiter".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            allowed_formats: vec![],
            plugin: "core".to_string(),
        });

        self.register(BlockTypeDefinition {
            type_name: "embed".to_string(),
            label: "Embed".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string" },
                    "source": { "type": "string" },
                    "embed": { "type": "string" },
                    "width": { "type": "integer" },
                    "height": { "type": "integer" }
                },
                "required": ["service", "source"]
            }),
            allowed_formats: vec![],
            plugin: "core".to_string(),
        });
    }

    /// Validate block data against the registered block type.
    ///
    /// Returns a list of validation error messages. An empty list means the
    /// block is valid.
    ///
    /// Validation rules per block type:
    /// - paragraph / heading / quote / list: sanitize text fields via ammonia
    /// - image: `file.url` must be present and non-empty
    /// - code: `code` field must exist
    /// - embed: `service` and `source` fields must exist
    pub fn validate_block(&self, type_name: &str, data: &Value) -> Vec<String> {
        let mut errors = Vec::new();

        // Check block type is registered
        if !self.contains(type_name) {
            errors.push(format!("unknown block type '{type_name}'"));
            return errors;
        }

        match type_name {
            "paragraph" => {
                validate_text_field(data, "text", "paragraph", &mut errors);
            }
            "heading" => {
                validate_text_field(data, "text", "heading", &mut errors);
                // Validate level if present
                if let Some(level) = data.get("level") {
                    if let Some(n) = level.as_i64() {
                        if !(1..=6).contains(&n) {
                            errors.push(format!("heading: level must be between 1 and 6, got {n}"));
                        }
                    } else {
                        errors.push("heading: level must be an integer".to_string());
                    }
                }
            }
            "quote" => {
                validate_text_field(data, "text", "quote", &mut errors);
                // Caption is optional, but sanitize if present
                if data.get("caption").and_then(|v| v.as_str()).is_some() {
                    validate_text_field(data, "caption", "quote", &mut errors);
                }
            }
            "list" => {
                if let Some(items) = data.get("items").and_then(|v| v.as_array()) {
                    for (i, item) in items.iter().enumerate() {
                        if let Some(text) = item.as_str() {
                            let sanitized = sanitize_html(text);
                            if sanitized != text {
                                errors.push(format!(
                                    "list: item {i} contains disallowed HTML that was sanitized"
                                ));
                            }
                        }
                    }
                }
            }
            "image" => {
                let url = data
                    .get("file")
                    .and_then(|f| f.get("url"))
                    .and_then(|u| u.as_str());
                match url {
                    Some("") => {
                        errors.push("image: file.url must not be empty".to_string());
                    }
                    Some(_) => {} // valid
                    None => {
                        errors.push("image: missing required field file.url".to_string());
                    }
                }
            }
            "code" => {
                if data.get("code").is_none() {
                    errors.push("code: missing required field 'code'".to_string());
                }
            }
            "embed" => {
                if data.get("service").is_none() {
                    errors.push("embed: missing required field 'service'".to_string());
                }
                if data.get("source").is_none() {
                    errors.push("embed: missing required field 'source'".to_string());
                }
            }
            // delimiter and any other registered types pass without extra validation
            _ => {}
        }

        errors
    }

    /// Sanitize all text content in an array of blocks in-place.
    ///
    /// Walks each block and applies `ammonia::clean()` to text-bearing fields
    /// for paragraph, heading, quote, and list blocks.
    pub fn sanitize_blocks(&self, blocks: &mut [Value]) {
        for block in blocks.iter_mut() {
            let type_name = block
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();

            let Some(data) = block.get_mut("data") else {
                continue;
            };

            match type_name.as_str() {
                "paragraph" | "heading" => {
                    sanitize_value_field(data, "text");
                }
                "quote" => {
                    sanitize_value_field(data, "text");
                    sanitize_value_field(data, "caption");
                }
                "list" => {
                    if let Some(items) = data.get_mut("items").and_then(|v| v.as_array_mut()) {
                        for item in items.iter_mut() {
                            if let Some(text) = item.as_str().map(sanitize_html) {
                                *item = Value::String(text);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Validate that a text field exists, is a string, and passes sanitization
/// without changes (i.e. contains no disallowed HTML).
fn validate_text_field(data: &Value, field: &str, block_type: &str, errors: &mut Vec<String>) {
    match data.get(field).and_then(|v| v.as_str()) {
        Some(text) => {
            let sanitized = sanitize_html(text);
            if sanitized != text {
                errors.push(format!(
                    "{block_type}: '{field}' contains disallowed HTML that was sanitized"
                ));
            }
        }
        None => {
            // Field missing is acceptable for validation-only; the schema
            // check handles required fields. We only flag if present and bad.
        }
    }
}

/// Sanitize a string field inside a JSON object in-place using ammonia.
fn sanitize_value_field(data: &mut Value, field: &str) {
    if let Some(text) = data.get(field).and_then(|v| v.as_str()).map(sanitize_html)
        && let Some(v) = data.as_object_mut().and_then(|obj| obj.get_mut(field))
    {
        *v = Value::String(text);
    }
}

/// Sanitize HTML input using ammonia with default settings.
///
/// Strips dangerous elements like `<script>`, event handlers, and
/// other XSS vectors while preserving safe formatting tags.
pub fn sanitize_html(input: &str) -> String {
    ammonia::clean(input)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn register_all_standard_types() {
        let registry = BlockTypeRegistry::with_standard_types();
        assert_eq!(registry.len(), 8);

        let expected = [
            "paragraph",
            "heading",
            "image",
            "list",
            "quote",
            "code",
            "delimiter",
            "embed",
        ];
        for name in &expected {
            assert!(
                registry.contains(name),
                "expected block type '{name}' to be registered"
            );
        }
    }

    #[test]
    fn standard_type_labels() {
        let registry = BlockTypeRegistry::with_standard_types();
        assert_eq!(registry.get("paragraph").unwrap().label, "Paragraph");
        assert_eq!(registry.get("heading").unwrap().label, "Heading");
        assert_eq!(registry.get("image").unwrap().label, "Image");
        assert_eq!(registry.get("list").unwrap().label, "List");
        assert_eq!(registry.get("quote").unwrap().label, "Quote");
        assert_eq!(registry.get("code").unwrap().label, "Code");
        assert_eq!(registry.get("delimiter").unwrap().label, "Delimiter");
        assert_eq!(registry.get("embed").unwrap().label, "Embed");
    }

    #[test]
    fn validate_valid_paragraph() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "text": "Hello world" });
        let errors = registry.validate_block("paragraph", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_valid_heading() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "text": "Title", "level": 2 });
        let errors = registry.validate_block("heading", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_valid_image() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({
            "file": { "url": "https://example.com/photo.jpg" },
            "caption": "A photo",
            "alt": "Photo alt text"
        });
        let errors = registry.validate_block("image", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_valid_list() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({
            "style": "ordered",
            "items": ["First", "Second", "Third"]
        });
        let errors = registry.validate_block("list", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_valid_quote() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "text": "To be or not to be", "caption": "Shakespeare" });
        let errors = registry.validate_block("quote", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_valid_code() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "code": "fn main() {}", "language": "rust" });
        let errors = registry.validate_block("code", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_valid_delimiter() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({});
        let errors = registry.validate_block("delimiter", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_valid_embed() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({
            "service": "youtube",
            "source": "https://www.youtube.com/watch?v=abc123"
        });
        let errors = registry.validate_block("embed", &data);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn reject_unknown_block_type() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({});
        let errors = registry.validate_block("carousel", &data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("unknown block type 'carousel'"));
    }

    #[test]
    fn paragraph_script_tag_stripped_by_ammonia() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({
            "text": "<p>Hello</p><script>alert('xss')</script>"
        });
        let errors = registry.validate_block("paragraph", &data);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("disallowed HTML"),
            "Expected sanitization error, got: {}",
            errors[0]
        );
    }

    #[test]
    fn heading_script_tag_stripped() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({
            "text": "Title<script>alert('xss')</script>",
            "level": 1
        });
        let errors = registry.validate_block("heading", &data);
        assert!(!errors.is_empty(), "Should detect disallowed HTML");
        assert!(errors[0].contains("disallowed HTML"));
    }

    #[test]
    fn image_missing_file_url_returns_error() {
        let registry = BlockTypeRegistry::with_standard_types();

        // Missing file entirely
        let data1 = serde_json::json!({ "caption": "A photo" });
        let errors1 = registry.validate_block("image", &data1);
        assert_eq!(errors1.len(), 1);
        assert!(errors1[0].contains("file.url"));

        // File present but url missing
        let data2 = serde_json::json!({ "file": {} });
        let errors2 = registry.validate_block("image", &data2);
        assert_eq!(errors2.len(), 1);
        assert!(errors2[0].contains("file.url"));

        // File present but url is empty
        let data3 = serde_json::json!({ "file": { "url": "" } });
        let errors3 = registry.validate_block("image", &data3);
        assert_eq!(errors3.len(), 1);
        assert!(errors3[0].contains("must not be empty"));
    }

    #[test]
    fn code_block_missing_code_field_returns_error() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "language": "rust" });
        let errors = registry.validate_block("code", &data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("missing required field 'code'"));
    }

    #[test]
    fn embed_missing_service_returns_error() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "source": "https://example.com" });
        let errors = registry.validate_block("embed", &data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("'service'"));
    }

    #[test]
    fn embed_missing_source_returns_error() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "service": "youtube" });
        let errors = registry.validate_block("embed", &data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("'source'"));
    }

    #[test]
    fn embed_missing_both_returns_two_errors() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({});
        let errors = registry.validate_block("embed", &data);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn sanitize_html_strips_script() {
        let input = "<p>Hello</p><script>alert('xss')</script>";
        let output = sanitize_html(input);
        assert!(
            !output.contains("<script>"),
            "Script tag should be stripped"
        );
        assert!(
            output.contains("<p>Hello</p>"),
            "Safe tags should be preserved"
        );
    }

    #[test]
    fn sanitize_html_preserves_safe_tags() {
        let input = "<p>Hello <strong>world</strong></p>";
        let output = sanitize_html(input);
        assert_eq!(output, input);
    }

    #[test]
    fn sanitize_html_strips_event_handlers() {
        let input = r#"<a href="/page" onclick="alert('xss')">Link</a>"#;
        let output = sanitize_html(input);
        assert!(!output.contains("onclick"));
    }

    #[test]
    fn sanitize_blocks_cleans_paragraph_text() {
        let registry = BlockTypeRegistry::with_standard_types();
        let mut blocks = vec![serde_json::json!({
            "type": "paragraph",
            "data": {
                "text": "<p>Hello</p><script>alert('xss')</script>"
            }
        })];
        registry.sanitize_blocks(&mut blocks);

        let text = blocks[0]["data"]["text"].as_str().unwrap();
        assert!(!text.contains("<script>"), "Script should be stripped");
        assert!(text.contains("Hello"), "Safe content should remain");
    }

    #[test]
    fn sanitize_blocks_cleans_heading_text() {
        let registry = BlockTypeRegistry::with_standard_types();
        let mut blocks = vec![serde_json::json!({
            "type": "heading",
            "data": {
                "text": "Title<script>bad</script>",
                "level": 2
            }
        })];
        registry.sanitize_blocks(&mut blocks);

        let text = blocks[0]["data"]["text"].as_str().unwrap();
        assert!(!text.contains("<script>"));
        assert!(text.contains("Title"));
    }

    #[test]
    fn sanitize_blocks_cleans_quote_text_and_caption() {
        let registry = BlockTypeRegistry::with_standard_types();
        let mut blocks = vec![serde_json::json!({
            "type": "quote",
            "data": {
                "text": "Quote<script>x</script>",
                "caption": "Author<script>y</script>"
            }
        })];
        registry.sanitize_blocks(&mut blocks);

        let text = blocks[0]["data"]["text"].as_str().unwrap();
        let caption = blocks[0]["data"]["caption"].as_str().unwrap();
        assert!(!text.contains("<script>"));
        assert!(!caption.contains("<script>"));
    }

    #[test]
    fn sanitize_blocks_cleans_list_items() {
        let registry = BlockTypeRegistry::with_standard_types();
        let mut blocks = vec![serde_json::json!({
            "type": "list",
            "data": {
                "style": "unordered",
                "items": [
                    "Safe item",
                    "<b>Bold</b><script>bad</script>"
                ]
            }
        })];
        registry.sanitize_blocks(&mut blocks);

        let items = blocks[0]["data"]["items"].as_array().unwrap();
        assert_eq!(items[0].as_str().unwrap(), "Safe item");
        assert!(!items[1].as_str().unwrap().contains("<script>"));
        assert!(items[1].as_str().unwrap().contains("<b>Bold</b>"));
    }

    #[test]
    fn sanitize_blocks_leaves_non_text_blocks_unchanged() {
        let registry = BlockTypeRegistry::with_standard_types();
        let mut blocks = vec![
            serde_json::json!({
                "type": "image",
                "data": {
                    "file": { "url": "https://example.com/photo.jpg" },
                    "caption": "Photo"
                }
            }),
            serde_json::json!({
                "type": "delimiter",
                "data": {}
            }),
        ];

        let before = blocks.clone();
        registry.sanitize_blocks(&mut blocks);
        assert_eq!(blocks, before);
    }

    #[test]
    fn list_with_script_in_items_detected() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({
            "style": "unordered",
            "items": ["Good", "<script>bad</script>"]
        });
        let errors = registry.validate_block("list", &data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("item 1"));
    }

    #[test]
    fn heading_invalid_level() {
        let registry = BlockTypeRegistry::with_standard_types();
        let data = serde_json::json!({ "text": "Title", "level": 7 });
        let errors = registry.validate_block("heading", &data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("level must be between"));
    }

    #[test]
    fn empty_registry() {
        let registry = BlockTypeRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn custom_block_type_registration() {
        let mut registry = BlockTypeRegistry::new();
        registry.register(BlockTypeDefinition {
            type_name: "custom_widget".to_string(),
            label: "Custom Widget".to_string(),
            schema: serde_json::json!({}),
            allowed_formats: vec![],
            plugin: "my_plugin".to_string(),
        });
        assert!(registry.contains("custom_widget"));
        assert_eq!(registry.get("custom_widget").unwrap().plugin, "my_plugin");
    }

    #[test]
    fn type_names_returns_all_registered() {
        let registry = BlockTypeRegistry::with_standard_types();
        let names = registry.type_names();
        assert_eq!(names.len(), 8);
        assert!(names.contains(&"paragraph".to_string()));
        assert!(names.contains(&"embed".to_string()));
    }

    #[test]
    fn default_registry_is_empty() {
        let registry = BlockTypeRegistry::default();
        assert!(registry.is_empty());
    }
}
