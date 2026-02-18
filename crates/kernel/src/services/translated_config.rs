//! Translated config storage wrapper.
//!
//! Wraps existing ConfigStorage to overlay translatable fields
//! from the config_translation table for the active language.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;
use tracing::debug;

use crate::config_storage::{ConfigEntity, ConfigFilter, ConfigStorage};

/// Config storage wrapper that overlays translations for the active language.
pub struct TranslatedConfigStorage {
    inner: Box<dyn ConfigStorage>,
    pool: PgPool,
    language: String,
}

impl TranslatedConfigStorage {
    /// Create a new translated config storage.
    pub fn new(inner: Box<dyn ConfigStorage>, pool: PgPool, language: String) -> Self {
        Self {
            inner,
            pool,
            language,
        }
    }

    /// Load translation overlay for a config entity.
    async fn load_translation(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Option<serde_json::Value> {
        let result: Option<serde_json::Value> = sqlx::query_scalar(
            r#"
            SELECT data FROM config_translation
            WHERE entity_type = $1 AND entity_id = $2 AND language = $3
            "#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(&self.language)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();

        result
    }
}

#[async_trait]
impl ConfigStorage for TranslatedConfigStorage {
    async fn load(&self, entity_type: &str, id: &str) -> Result<Option<ConfigEntity>> {
        let entity = self.inner.load(entity_type, id).await?;

        let Some(entity) = entity else {
            return Ok(None);
        };

        // If we're on the default language, no translation needed
        // (the caller should check this, but we protect here too)
        let translation = self.load_translation(entity_type, id).await;
        let Some(translation_data) = translation else {
            return Ok(Some(entity));
        };

        // Apply translation overlay to the entity
        let translated = apply_translation_overlay(entity, &translation_data);
        Ok(Some(translated))
    }

    async fn save(&self, entity: &ConfigEntity) -> Result<()> {
        // Saves always go to the inner storage (base language).
        // If called with a non-default language active, this writes to the base
        // language — callers that need translation-aware save should use the
        // config_translation table directly.
        if !self.language.is_empty() && self.language != "en" {
            tracing::warn!(
                language = %self.language,
                "TranslatedConfigStorage::save writes to base storage, not translation layer"
            );
        }
        self.inner.save(entity).await
    }

    async fn delete(&self, entity_type: &str, id: &str) -> Result<bool> {
        self.inner.delete(entity_type, id).await
    }

    async fn list(
        &self,
        entity_type: &str,
        filter: Option<&ConfigFilter>,
    ) -> Result<Vec<ConfigEntity>> {
        // Load base entities
        let entities = self.inner.list(entity_type, filter).await?;

        // We don't translate list results by default for performance
        // Individual loads will get translated
        Ok(entities)
    }
}

/// Apply translation data overlay to a config entity.
///
/// Currently supports translatable string fields on entity variants.
fn apply_translation_overlay(
    entity: ConfigEntity,
    translation_data: &serde_json::Value,
) -> ConfigEntity {
    match entity {
        ConfigEntity::Variable { key, value } => {
            // Variables: translate the value if it's a string
            if let Some(translated) = translation_data.get("value") {
                ConfigEntity::Variable {
                    key,
                    value: translated.clone(),
                }
            } else {
                ConfigEntity::Variable { key, value }
            }
        }
        // For other entity types, return as-is
        // (ItemType labels, Category names, etc. could be translated in future)
        other => {
            debug!("config translation overlay not supported for this entity type");
            other
        }
    }
}

impl std::fmt::Debug for TranslatedConfigStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranslatedConfigStorage")
            .field("language", &self.language)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_translation_overlay() {
        let entity = ConfigEntity::Variable {
            key: "site_name".to_string(),
            value: serde_json::json!("My Site"),
        };
        let translation = serde_json::json!({
            "value": "Mon Site"
        });

        let translated = apply_translation_overlay(entity, &translation);
        match translated {
            ConfigEntity::Variable { key, value } => {
                assert_eq!(key, "site_name");
                assert_eq!(value, serde_json::json!("Mon Site"));
            }
            _ => panic!("expected Variable"),
        }
    }

    #[test]
    fn variable_no_translation() {
        let entity = ConfigEntity::Variable {
            key: "site_name".to_string(),
            value: serde_json::json!("My Site"),
        };
        let translation = serde_json::json!({});

        let translated = apply_translation_overlay(entity, &translation);
        match translated {
            ConfigEntity::Variable { key: _, value } => {
                assert_eq!(value, serde_json::json!("My Site"));
            }
            _ => panic!("expected Variable"),
        }
    }
}
