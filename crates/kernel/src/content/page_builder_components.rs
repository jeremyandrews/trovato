//! Page builder component registry.
//!
//! Defines the available Puck components and their prop schemas. The registry
//! is used by:
//! - The `/api/v1/page-builder/components` endpoint (editor reads this on init)
//! - Server-side validation of saved Puck JSON
//! - Admin UI component picker

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Component definition for the Puck editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentDefinition {
    /// PascalCase type name (e.g., `"Hero"`, `"Columns"`).
    pub type_name: String,
    /// Human-readable label for the editor UI.
    pub label: String,
    /// Category for grouping in the component picker.
    pub category: String,
    /// JSON Schema-like description of component props.
    pub props: serde_json::Value,
    /// Whether this component has drop zones for child components.
    pub has_zones: bool,
    /// Named zones (e.g., `["zone-0", "zone-1"]`, `["content"]`).
    pub zone_names: Vec<String>,
}

/// Registry of available page builder components.
pub struct ComponentRegistry {
    components: HashMap<String, ComponentDefinition>,
}

impl ComponentRegistry {
    /// Create a registry with all standard components registered.
    pub fn new() -> Self {
        let mut registry = Self {
            components: HashMap::new(),
        };
        registry.register_standard_components();
        registry
    }

    /// Get a component definition by type name.
    pub fn get(&self, type_name: &str) -> Option<&ComponentDefinition> {
        self.components.get(type_name)
    }

    /// Get all component definitions.
    pub fn all(&self) -> Vec<&ComponentDefinition> {
        let mut all: Vec<_> = self.components.values().collect();
        all.sort_by(|a, b| a.category.cmp(&b.category).then(a.label.cmp(&b.label)));
        all
    }

    /// Validate a Puck page JSON against the registry.
    ///
    /// Returns a list of validation errors (empty = valid).
    pub fn validate_page(&self, page: &serde_json::Value) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(content) = page.get("content").and_then(|c| c.as_array()) {
            for (i, component) in content.iter().enumerate() {
                self.validate_component(i, component, &mut errors);
            }
        }
        errors
    }

    fn validate_component(
        &self,
        index: usize,
        component: &serde_json::Value,
        errors: &mut Vec<String>,
    ) {
        let type_name = component.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if !self.components.contains_key(type_name) {
            errors.push(format!("Component {index}: unknown type '{type_name}'"));
        }
        // Heading level >= 2 (H1 reserved for page title)
        if let Some(level) = component
            .pointer("/props/headingLevel")
            .and_then(|l| l.as_u64())
            && level < 2
        {
            errors.push(format!(
                "Component {index} ({type_name}): headingLevel must be >= 2"
            ));
        }
        // Image alt text required unless decorative
        if let Some(url) = component
            .pointer("/props/imageUrl")
            .and_then(|u| u.as_str())
            && !url.is_empty()
        {
            let is_decorative = component
                .pointer("/props/isDecorative")
                .and_then(|d| d.as_bool())
                .unwrap_or(false);
            let has_alt = component
                .pointer("/props/imageAlt")
                .and_then(|a| a.as_str())
                .is_some_and(|a| !a.is_empty());
            if !is_decorative && !has_alt {
                errors.push(format!(
                    "Component {index} ({type_name}): image requires alt text or isDecorative=true"
                ));
            }
        }
        // Recurse into zones
        if let Some(zones) = component.get("zones").and_then(|z| z.as_object()) {
            for (_, children) in zones {
                if let Some(arr) = children.as_array() {
                    for (j, child) in arr.iter().enumerate() {
                        self.validate_component(j, child, errors);
                    }
                }
            }
        }
    }

    fn register(&mut self, def: ComponentDefinition) {
        self.components.insert(def.type_name.clone(), def);
    }

    fn register_standard_components(&mut self) {
        // --- Layout ---
        self.register(ComponentDefinition {
            type_name: "Columns".into(),
            label: "Columns".into(),
            category: "layout".into(),
            props: serde_json::json!({
                "layout": { "type": "string", "enum": ["1/2+1/2", "2/3+1/3", "1/3+2/3", "1/3+1/3+1/3", "1/4+1/4+1/4+1/4"], "default": "1/2+1/2" },
                "gap": { "type": "string", "default": "2rem" }
            }),
            has_zones: true,
            zone_names: vec!["zone-0".into(), "zone-1".into()],
        });

        self.register(ComponentDefinition {
            type_name: "SectionWrapper".into(),
            label: "Section".into(),
            category: "layout".into(),
            props: serde_json::json!({
                "backgroundColor": { "type": "string" },
                "padding": { "type": "string", "enum": ["none", "default", "large"], "default": "default" },
                "maxWidth": { "type": "string", "enum": ["default", "wide", "full"], "default": "default" },
                "ariaLabel": { "type": "string" },
                "lang": { "type": "string" }
            }),
            has_zones: true,
            zone_names: vec!["content".into()],
        });

        // --- Content ---
        self.register(ComponentDefinition {
            type_name: "Hero".into(),
            label: "Hero Section".into(),
            category: "content".into(),
            props: serde_json::json!({
                "title": { "type": "string", "required": true },
                "subtitle": { "type": "string" },
                "backgroundImage": { "type": "string" },
                "imageAlt": { "type": "string" },
                "isDecorative": { "type": "boolean", "default": false },
                "ctaText": { "type": "string" },
                "ctaUrl": { "type": "string" },
                "variant": { "type": "string", "enum": ["standard", "split", "minimal"], "default": "standard" },
                "backgroundColor": { "type": "string" },
                "headingLevel": { "type": "integer", "minimum": 2, "maximum": 6, "default": 2 },
                "lang": { "type": "string" }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "TextBlock".into(),
            label: "Text".into(),
            category: "content".into(),
            props: serde_json::json!({
                "content": { "type": "string", "format": "markdown" },
                "lang": { "type": "string" }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "CardGrid".into(),
            label: "Card Grid".into(),
            category: "content".into(),
            props: serde_json::json!({
                "columns": { "type": "integer", "minimum": 2, "maximum": 4, "default": 3 },
                "variant": { "type": "string", "enum": ["standard", "feature", "compact"], "default": "standard" },
                "cardHeadingLevel": { "type": "integer", "minimum": 2, "maximum": 6, "default": 3 },
                "cards": { "type": "array" }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "ContentFeature".into(),
            label: "Content Feature".into(),
            category: "content".into(),
            props: serde_json::json!({
                "title": { "type": "string" },
                "body": { "type": "string", "format": "markdown" },
                "imageUrl": { "type": "string" },
                "imageAlt": { "type": "string" },
                "imagePosition": { "type": "string", "enum": ["left", "right"], "default": "left" },
                "linkUrl": { "type": "string" },
                "linkText": { "type": "string" },
                "headingLevel": { "type": "integer", "minimum": 2, "maximum": 6, "default": 2 }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "Accordion".into(),
            label: "Accordion".into(),
            category: "content".into(),
            props: serde_json::json!({
                "items": { "type": "array" },
                "allowMultiple": { "type": "boolean", "default": false },
                "headingLevel": { "type": "integer", "minimum": 2, "maximum": 6, "default": 3 },
                "lang": { "type": "string" }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "BlockquoteExtended".into(),
            label: "Quote".into(),
            category: "content".into(),
            props: serde_json::json!({
                "text": { "type": "string", "required": true },
                "attribution": { "type": "string" },
                "role": { "type": "string" },
                "imageUrl": { "type": "string" },
                "sourceUrl": { "type": "string" },
                "lang": { "type": "string" }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "TableOfContents".into(),
            label: "Table of Contents".into(),
            category: "content".into(),
            props: serde_json::json!({
                "maxDepth": { "type": "integer", "minimum": 2, "maximum": 6, "default": 3 }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        // --- CTA ---
        self.register(ComponentDefinition {
            type_name: "Cta".into(),
            label: "Call to Action".into(),
            category: "cta".into(),
            props: serde_json::json!({
                "heading": { "type": "string" },
                "body": { "type": "string" },
                "buttonText": { "type": "string" },
                "buttonUrl": { "type": "string" },
                "variant": { "type": "string", "enum": ["inline", "fullWidth", "callout"], "default": "inline" },
                "backgroundColor": { "type": "string" },
                "headingLevel": { "type": "integer", "minimum": 2, "maximum": 6, "default": 2 }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        // --- Media ---
        self.register(ComponentDefinition {
            type_name: "LogoRow".into(),
            label: "Logo Row".into(),
            category: "media".into(),
            props: serde_json::json!({
                "title": { "type": "string" },
                "logos": { "type": "array" },
                "headingLevel": { "type": "integer", "minimum": 2, "maximum": 6, "default": 3 }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "YouTubeEmbed".into(),
            label: "YouTube Video".into(),
            category: "media".into(),
            props: serde_json::json!({
                "videoId": { "type": "string", "required": true },
                "title": { "type": "string", "required": true },
                "aspectRatio": { "type": "string", "enum": ["16:9", "4:3"], "default": "16:9" },
                "transcriptUrl": { "type": "string" },
                "lang": { "type": "string" }
            }),
            has_zones: false,
            zone_names: vec![],
        });

        self.register(ComponentDefinition {
            type_name: "SummaryBox".into(),
            label: "Summary Box".into(),
            category: "content".into(),
            props: serde_json::json!({
                "title": { "type": "string" },
                "content": { "type": "string" },
                "variant": { "type": "string", "enum": ["info", "warning", "success"], "default": "info" },
                "headingLevel": { "type": "integer", "minimum": 2, "maximum": 6, "default": 3 },
                "lang": { "type": "string" }
            }),
            has_zones: true,
            zone_names: vec!["content".into()],
        });
    }
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_12_components() {
        let registry = ComponentRegistry::new();
        assert_eq!(registry.components.len(), 13); // 12 + SummaryBox
        assert!(registry.get("Hero").is_some());
        assert!(registry.get("Columns").is_some());
        assert!(registry.get("TextBlock").is_some());
        assert!(registry.get("CardGrid").is_some());
        assert!(registry.get("Cta").is_some());
        assert!(registry.get("Accordion").is_some());
        assert!(registry.get("ContentFeature").is_some());
        assert!(registry.get("LogoRow").is_some());
        assert!(registry.get("SummaryBox").is_some());
        assert!(registry.get("SectionWrapper").is_some());
        assert!(registry.get("BlockquoteExtended").is_some());
        assert!(registry.get("YouTubeEmbed").is_some());
        assert!(registry.get("TableOfContents").is_some());
    }

    #[test]
    fn validate_page_catches_unknown_component() {
        let registry = ComponentRegistry::new();
        let page = serde_json::json!({
            "content": [
                { "type": "NonexistentWidget", "props": {} }
            ]
        });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("NonexistentWidget"));
    }

    #[test]
    fn validate_page_catches_h1_heading() {
        let registry = ComponentRegistry::new();
        let page = serde_json::json!({
            "content": [
                { "type": "Hero", "props": { "headingLevel": 1 } }
            ]
        });
        let errors = registry.validate_page(&page);
        assert!(!errors.is_empty());
        assert!(errors[0].contains("headingLevel"));
    }

    #[test]
    fn validate_page_catches_missing_alt() {
        let registry = ComponentRegistry::new();
        let page = serde_json::json!({
            "content": [
                { "type": "ContentFeature", "props": { "imageUrl": "/img/photo.jpg" } }
            ]
        });
        let errors = registry.validate_page(&page);
        assert!(!errors.is_empty());
        assert!(errors[0].contains("alt text"));
    }

    #[test]
    fn validate_page_allows_decorative_image() {
        let registry = ComponentRegistry::new();
        let page = serde_json::json!({
            "content": [
                { "type": "ContentFeature", "props": { "imageUrl": "/img/bg.jpg", "isDecorative": true } }
            ]
        });
        let errors = registry.validate_page(&page);
        assert!(errors.is_empty());
    }

    #[test]
    fn validate_valid_page() {
        let registry = ComponentRegistry::new();
        let page = serde_json::json!({
            "content": [
                { "type": "Hero", "props": { "title": "Hello", "variant": "standard" } },
                { "type": "TextBlock", "props": { "content": "Some text" } }
            ]
        });
        let errors = registry.validate_page(&page);
        assert!(errors.is_empty());
    }
}
