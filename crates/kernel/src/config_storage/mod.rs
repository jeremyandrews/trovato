//! Configuration storage abstraction layer.
//!
//! This module provides a unified interface for accessing all config entities.
//! The key principle: **all config reads/writes must go through ConfigStorage**.
//!
//! This enables future stage-aware config by simply swapping the implementation
//! with a decorator, without changing any call sites.
//!
//! # Entity Types
//!
//! - `item_type` - Content type definitions (includes field definitions in settings)
//! - `search_field_config` - Search index configuration per content type/field
//! - `category` - Category definitions
//! - `tag` - Tag (term) definitions within categories
//! - `variable` - Site configuration variables
//!
//! # Usage
//!
//! ```ignore
//! // Load a config entity
//! let item_type = storage.load("item_type", "blog").await?;
//!
//! // List all entities of a type
//! let categories = storage.list("category", None).await?;
//! ```

mod direct;
mod stage_aware;
pub mod yaml;

use std::fmt;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use direct::DirectConfigStorage;
pub use stage_aware::StageAwareConfigStorage;

use crate::models::{Category, ItemType, Language, Tag};

/// A configuration entity that can be stored and retrieved.
///
/// This enum covers all config entity types that need stage-aware access.
/// Each variant wraps the corresponding model type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entity_type", content = "data")]
pub enum ConfigEntity {
    /// Content type definition (includes field definitions in settings).
    #[serde(rename = "item_type")]
    ItemType(ItemType),

    /// Search field configuration (which fields are indexed for search).
    #[serde(rename = "search_field_config")]
    SearchFieldConfig(SearchFieldConfig),

    /// Category definition.
    #[serde(rename = "category")]
    Category(Category),

    /// Tag (term) within a category.
    #[serde(rename = "tag")]
    Tag(Tag),

    /// Site configuration variable.
    #[serde(rename = "variable")]
    Variable {
        key: String,
        value: serde_json::Value,
    },

    /// Language definition.
    #[serde(rename = "language")]
    Language(Language),
}

impl ConfigEntity {
    /// Get the entity type name.
    pub fn entity_type(&self) -> &'static str {
        match self {
            Self::ItemType(_) => "item_type",
            Self::SearchFieldConfig(_) => "search_field_config",
            Self::Category(_) => "category",
            Self::Tag(_) => "tag",
            Self::Variable { .. } => "variable",
            Self::Language(_) => "language",
        }
    }

    /// Get the entity ID as a string.
    pub fn id(&self) -> String {
        match self {
            Self::ItemType(t) => t.type_name.clone(),
            Self::SearchFieldConfig(f) => f.id.to_string(),
            Self::Category(c) => c.id.clone(),
            Self::Tag(t) => t.id.to_string(),
            Self::Variable { key, .. } => key.clone(),
            Self::Language(l) => l.id.clone(),
        }
    }

    /// Try to extract an ItemType from this entity.
    pub fn as_item_type(&self) -> Option<&ItemType> {
        match self {
            Self::ItemType(t) => Some(t),
            _ => None,
        }
    }

    /// Try to extract a SearchFieldConfig from this entity.
    pub fn as_search_field_config(&self) -> Option<&SearchFieldConfig> {
        match self {
            Self::SearchFieldConfig(f) => Some(f),
            _ => None,
        }
    }

    /// Try to extract a Category from this entity.
    pub fn as_category(&self) -> Option<&Category> {
        match self {
            Self::Category(c) => Some(c),
            _ => None,
        }
    }

    /// Try to extract a Tag from this entity.
    pub fn as_tag(&self) -> Option<&Tag> {
        match self {
            Self::Tag(t) => Some(t),
            _ => None,
        }
    }

    /// Try to extract a Variable from this entity.
    pub fn as_variable(&self) -> Option<(&str, &serde_json::Value)> {
        match self {
            Self::Variable { key, value } => Some((key, value)),
            _ => None,
        }
    }

    /// Consume and convert to ItemType if possible.
    pub fn into_item_type(self) -> Option<ItemType> {
        match self {
            Self::ItemType(t) => Some(t),
            _ => None,
        }
    }

    /// Consume and convert to Category if possible.
    pub fn into_category(self) -> Option<Category> {
        match self {
            Self::Category(c) => Some(c),
            _ => None,
        }
    }

    /// Consume and convert to Tag if possible.
    pub fn into_tag(self) -> Option<Tag> {
        match self {
            Self::Tag(t) => Some(t),
            _ => None,
        }
    }

    /// Consume and convert to SearchFieldConfig if possible.
    pub fn into_search_field_config(self) -> Option<SearchFieldConfig> {
        match self {
            Self::SearchFieldConfig(f) => Some(f),
            _ => None,
        }
    }

    /// Try to extract a Language from this entity.
    pub fn as_language(&self) -> Option<&Language> {
        match self {
            Self::Language(l) => Some(l),
            _ => None,
        }
    }

    /// Consume and convert to Language if possible.
    pub fn into_language(self) -> Option<Language> {
        match self {
            Self::Language(l) => Some(l),
            _ => None,
        }
    }
}

impl fmt::Display for ConfigEntity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.entity_type(), self.id())
    }
}

/// Search field configuration.
///
/// Defines which fields are indexed for full-text search and their weights.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SearchFieldConfig {
    /// Unique identifier.
    pub id: Uuid,

    /// Content type this config applies to.
    pub bundle: String,

    /// Field name to index (from JSONB fields).
    pub field_name: String,

    /// Search weight: A (highest) to D (lowest).
    pub weight: String,
}

/// Filter criteria for listing config entities.
///
/// This is intentionally simple for v1.0 - can be extended post-MVP.
#[derive(Debug, Clone, Default)]
pub struct ConfigFilter {
    /// Filter by a specific field value.
    pub field: Option<String>,

    /// Value to match for the field.
    pub value: Option<String>,

    /// Maximum number of results.
    pub limit: Option<usize>,

    /// Number of results to skip.
    pub offset: Option<usize>,
}

impl ConfigFilter {
    /// Create a new empty filter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by a specific field.
    pub fn with_field(mut self, field: impl Into<String>, value: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self.value = Some(value.into());
        self
    }

    /// Limit results.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Skip results.
    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }
}

/// The core trait for config entity storage.
///
/// All config entity access MUST go through this trait. This is critical
/// for enabling stage-aware config in the future - by wrapping the
/// implementation with a decorator, we can inject stage context without
/// changing any call sites.
///
/// # Drupal Workspaces Lesson
///
/// If any code bypasses this trait with raw SQL, stage awareness breaks.
/// Keep the interface small and stable.
#[async_trait]
pub trait ConfigStorage: Send + Sync {
    /// Load a single config entity by type and ID.
    ///
    /// Returns `None` if the entity doesn't exist.
    async fn load(&self, entity_type: &str, id: &str) -> Result<Option<ConfigEntity>>;

    /// Save a config entity (insert or update).
    ///
    /// The entity type and ID are extracted from the entity itself.
    async fn save(&self, entity: &ConfigEntity) -> Result<()>;

    /// Delete a config entity by type and ID.
    ///
    /// Returns `true` if an entity was deleted, `false` if it didn't exist.
    async fn delete(&self, entity_type: &str, id: &str) -> Result<bool>;

    /// List config entities of a given type, optionally filtered.
    async fn list(
        &self,
        entity_type: &str,
        filter: Option<&ConfigFilter>,
    ) -> Result<Vec<ConfigEntity>>;

    /// Check if a config entity exists.
    async fn exists(&self, entity_type: &str, id: &str) -> Result<bool> {
        Ok(self.load(entity_type, id).await?.is_some())
    }
}

/// Entity type constants for use with ConfigStorage.
pub mod entity_types {
    /// Content type definitions.
    pub const ITEM_TYPE: &str = "item_type";

    /// Search field configuration per content type.
    pub const SEARCH_FIELD_CONFIG: &str = "search_field_config";

    /// Category definitions.
    pub const CATEGORY: &str = "category";

    /// Tag (term) definitions within categories.
    pub const TAG: &str = "tag";

    /// Site configuration variables.
    pub const VARIABLE: &str = "variable";

    /// Language definitions.
    pub const LANGUAGE: &str = "language";
}

/// Helper to parse a tag ID from a string (UUID format).
pub fn parse_tag_id(id: &str) -> Result<Uuid> {
    id.parse::<Uuid>()
        .map_err(|e| anyhow::anyhow!("invalid tag ID '{id}': {e}"))
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn config_entity_type_names() {
        let item_type = ConfigEntity::ItemType(ItemType {
            type_name: "blog".to_string(),
            label: "Blog".to_string(),
            description: None,
            has_title: true,
            title_label: None,
            plugin: "blog".to_string(),
            settings: serde_json::json!({}),
        });

        assert_eq!(item_type.entity_type(), "item_type");
        assert_eq!(item_type.id(), "blog");
    }

    #[test]
    fn config_entity_variable() {
        let var = ConfigEntity::Variable {
            key: "site_name".to_string(),
            value: serde_json::json!("My Site"),
        };

        assert_eq!(var.entity_type(), "variable");
        assert_eq!(var.id(), "site_name");

        let (key, value) = var.as_variable().unwrap();
        assert_eq!(key, "site_name");
        assert_eq!(value, &serde_json::json!("My Site"));
    }

    #[test]
    fn config_filter_builder() {
        let filter = ConfigFilter::new()
            .with_field("plugin", "blog")
            .with_limit(10)
            .with_offset(5);

        assert_eq!(filter.field, Some("plugin".to_string()));
        assert_eq!(filter.value, Some("blog".to_string()));
        assert_eq!(filter.limit, Some(10));
        assert_eq!(filter.offset, Some(5));
    }

    #[test]
    fn config_entity_serialization() {
        let entity = ConfigEntity::Variable {
            key: "test".to_string(),
            value: serde_json::json!(42),
        };

        let json = serde_json::to_string(&entity).unwrap();
        assert!(json.contains("variable"), "Expected 'variable' in: {json}");
        assert!(json.contains("test"), "Expected 'test' in: {json}");

        let parsed: ConfigEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entity_type(), "variable");
    }

    #[test]
    fn config_entity_display() {
        let entity = ConfigEntity::Variable {
            key: "site_name".to_string(),
            value: serde_json::json!("Test"),
        };

        assert_eq!(format!("{entity}"), "variable:site_name");
    }

    #[test]
    fn config_entity_language() {
        let lang = ConfigEntity::Language(Language {
            id: "en".to_string(),
            label: "English".to_string(),
            weight: 0,
            is_default: true,
            direction: "ltr".to_string(),
        });

        assert_eq!(lang.entity_type(), "language");
        assert_eq!(lang.id(), "en");
        assert!(lang.as_language().is_some());
        assert_eq!(format!("{lang}"), "language:en");
    }

    /// Verify ConfigEntity::Language survives JSON round-trip (the path
    /// StageAwareConfigStorage uses: serde_json::to_value → serde_json::from_value).
    #[test]
    fn config_entity_language_json_round_trip() {
        let original = ConfigEntity::Language(Language {
            id: "ar".to_string(),
            label: "Arabic".to_string(),
            weight: 3,
            is_default: false,
            direction: "rtl".to_string(),
        });

        let json = serde_json::to_value(&original).expect("serialize");
        let restored: ConfigEntity = serde_json::from_value(json).expect("deserialize");

        assert_eq!(restored.entity_type(), "language");
        let lang = restored.into_language().expect("into_language");
        assert_eq!(lang.id, "ar");
        assert_eq!(lang.label, "Arabic");
        assert_eq!(lang.weight, 3);
        assert!(!lang.is_default);
        assert_eq!(lang.direction, "rtl");
    }
}
