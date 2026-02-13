//! Trovato test utilities.
//!
//! Helpers for integration testing: test fixtures, mock builders,
//! and assertion utilities for content system testing.

use serde_json::Value as JsonValue;
use uuid::Uuid;

/// Create a test item with default values.
pub fn test_item(item_type: &str, title: &str) -> TestItem {
    TestItem {
        id: Uuid::now_v7(),
        item_type: item_type.to_string(),
        title: title.to_string(),
        author_id: Uuid::nil(),
        status: 1,
        promote: 0,
        sticky: 0,
        fields: serde_json::json!({}),
        stage_id: "live".to_string(),
    }
}

/// A test item builder for creating test fixtures.
#[derive(Debug, Clone)]
pub struct TestItem {
    pub id: Uuid,
    pub item_type: String,
    pub title: String,
    pub author_id: Uuid,
    pub status: i16,
    pub promote: i16,
    pub sticky: i16,
    pub fields: JsonValue,
    pub stage_id: String,
}

impl TestItem {
    /// Set a custom ID.
    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    /// Set the author.
    pub fn with_author(mut self, author_id: Uuid) -> Self {
        self.author_id = author_id;
        self
    }

    /// Set as unpublished.
    pub fn unpublished(mut self) -> Self {
        self.status = 0;
        self
    }

    /// Set as published.
    pub fn published(mut self) -> Self {
        self.status = 1;
        self
    }

    /// Set as promoted.
    pub fn promoted(mut self) -> Self {
        self.promote = 1;
        self
    }

    /// Set as sticky.
    pub fn sticky(mut self) -> Self {
        self.sticky = 1;
        self
    }

    /// Set fields.
    pub fn with_fields(mut self, fields: JsonValue) -> Self {
        self.fields = fields;
        self
    }

    /// Add a single field.
    pub fn with_field(mut self, name: &str, value: JsonValue) -> Self {
        if let Some(obj) = self.fields.as_object_mut() {
            obj.insert(name.to_string(), value);
        }
        self
    }

    /// Add a text field.
    pub fn with_text_field(self, name: &str, value: &str, format: &str) -> Self {
        self.with_field(
            name,
            serde_json::json!({
                "value": value,
                "format": format
            }),
        )
    }

    /// Set stage.
    pub fn with_stage(mut self, stage_id: &str) -> Self {
        self.stage_id = stage_id.to_string();
        self
    }
}

/// Create a test user context.
pub fn test_user(permissions: &[&str]) -> TestUser {
    TestUser {
        id: Uuid::now_v7(),
        authenticated: true,
        permissions: permissions.iter().map(|s| s.to_string()).collect(),
    }
}

/// Create an anonymous test user.
pub fn anonymous_user() -> TestUser {
    TestUser {
        id: Uuid::nil(),
        authenticated: false,
        permissions: vec![],
    }
}

/// Create an admin test user.
pub fn admin_user() -> TestUser {
    TestUser {
        id: Uuid::now_v7(),
        authenticated: true,
        permissions: vec!["administer site".to_string()],
    }
}

/// A test user builder.
#[derive(Debug, Clone)]
pub struct TestUser {
    pub id: Uuid,
    pub authenticated: bool,
    pub permissions: Vec<String>,
}

impl TestUser {
    /// Set a custom ID.
    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    /// Add a permission.
    pub fn with_permission(mut self, perm: &str) -> Self {
        self.permissions.push(perm.to_string());
        self
    }

    /// Check if user has permission.
    pub fn has_permission(&self, perm: &str) -> bool {
        self.permissions.iter().any(|p| p == perm)
    }

    /// Check if user is admin.
    pub fn is_admin(&self) -> bool {
        self.has_permission("administer site")
    }
}

/// Assertion helpers for JSON content.
pub mod assert {
    use serde_json::Value;

    /// Assert that a JSON value has a specific key.
    pub fn has_key(value: &Value, key: &str) {
        assert!(
            value.get(key).is_some(),
            "Expected JSON to have key '{}', got: {}",
            key,
            value
        );
    }

    /// Assert that a JSON value equals expected.
    pub fn json_eq(actual: &Value, expected: &Value) {
        assert_eq!(
            actual,
            expected,
            "JSON mismatch:\nactual: {}\nexpected: {}",
            serde_json::to_string_pretty(actual).unwrap(),
            serde_json::to_string_pretty(expected).unwrap()
        );
    }

    /// Assert that a string contains a substring.
    pub fn contains(haystack: &str, needle: &str) {
        assert!(
            haystack.contains(needle),
            "Expected string to contain '{}'\nActual: {}",
            needle,
            haystack
        );
    }

    /// Assert that a string does not contain a substring.
    pub fn not_contains(haystack: &str, needle: &str) {
        assert!(
            !haystack.contains(needle),
            "Expected string to NOT contain '{}'\nActual: {}",
            needle,
            haystack
        );
    }
}

/// Content type builders for testing.
pub mod content_types {
    use serde_json::json;

    /// Create a simple page content type definition.
    pub fn page_type() -> serde_json::Value {
        json!({
            "machine_name": "page",
            "label": "Basic Page",
            "description": "A simple page",
            "fields": [
                {
                    "field_name": "body",
                    "field_type": "TextLong",
                    "label": "Body",
                    "required": false,
                    "cardinality": 1,
                    "settings": {}
                }
            ]
        })
    }

    /// Create a blog post content type definition.
    pub fn blog_type() -> serde_json::Value {
        json!({
            "machine_name": "blog",
            "label": "Blog Post",
            "description": "A blog article",
            "fields": [
                {
                    "field_name": "body",
                    "field_type": "TextLong",
                    "label": "Body",
                    "required": true,
                    "cardinality": 1,
                    "settings": {}
                },
                {
                    "field_name": "summary",
                    "field_type": {"Text": {"max_length": 255}},
                    "label": "Summary",
                    "required": false,
                    "cardinality": 1,
                    "settings": {}
                },
                {
                    "field_name": "tags",
                    "field_type": {"Text": {"max_length": 64}},
                    "label": "Tags",
                    "required": false,
                    "cardinality": -1,
                    "settings": {}
                }
            ]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_item_builder() {
        let item = test_item("blog", "Test Post")
            .unpublished()
            .promoted()
            .with_text_field("body", "Hello", "plain_text");

        assert_eq!(item.item_type, "blog");
        assert_eq!(item.title, "Test Post");
        assert_eq!(item.status, 0);
        assert_eq!(item.promote, 1);
        assert!(item.fields.get("body").is_some());
    }

    #[test]
    fn test_user_builder() {
        let user = test_user(&["access content", "create blog content"]);
        assert!(user.authenticated);
        assert!(user.has_permission("access content"));
        assert!(!user.has_permission("administer site"));
    }

    #[test]
    fn test_admin_user() {
        let user = admin_user();
        assert!(user.is_admin());
        assert!(user.authenticated);
    }

    #[test]
    fn test_anonymous_user() {
        let user = anonymous_user();
        assert!(!user.authenticated);
        assert_eq!(user.id, Uuid::nil());
    }

    #[test]
    fn test_assertions() {
        let json = serde_json::json!({"name": "test", "value": 42});
        assert::has_key(&json, "name");
        assert::has_key(&json, "value");

        assert::contains("hello world", "world");
        assert::not_contains("hello world", "foo");
    }

    #[test]
    fn test_content_types() {
        let page = content_types::page_type();
        assert_eq!(page["machine_name"], "page");

        let blog = content_types::blog_type();
        assert_eq!(blog["machine_name"], "blog");
        assert_eq!(blog["fields"].as_array().unwrap().len(), 3);
    }
}
