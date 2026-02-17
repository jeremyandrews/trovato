//! Content translation service.
//!
//! Provides field-level content translation with language overlay on base items.

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

/// Content translation record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct ItemTranslation {
    pub item_id: Uuid,
    pub language: String,
    pub title: String,
    pub fields: serde_json::Value,
    pub created: i64,
    pub changed: i64,
}

/// Content translation service.
#[derive(Clone)]
pub struct TranslationService {
    pool: PgPool,
}

impl TranslationService {
    /// Create a new translation service.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Load a translation for an item in a specific language.
    pub async fn load(&self, item_id: Uuid, language: &str) -> Result<Option<ItemTranslation>> {
        let translation = sqlx::query_as::<_, ItemTranslation>(
            r#"
            SELECT item_id, language, title, fields, created, changed
            FROM item_translation
            WHERE item_id = $1 AND language = $2
            "#,
        )
        .bind(item_id)
        .bind(language)
        .fetch_optional(&self.pool)
        .await;

        match translation {
            Ok(t) => Ok(t),
            Err(e) => {
                debug!(error = %e, "item_translation table may not exist yet");
                Ok(None)
            }
        }
    }

    /// Save a translation for an item.
    pub async fn save(
        &self,
        item_id: Uuid,
        language: &str,
        title: &str,
        fields: serde_json::Value,
    ) -> Result<ItemTranslation> {
        let now = chrono::Utc::now().timestamp();

        let translation = sqlx::query_as::<_, ItemTranslation>(
            r#"
            INSERT INTO item_translation (item_id, language, title, fields, created, changed)
            VALUES ($1, $2, $3, $4, $5, $5)
            ON CONFLICT (item_id, language) DO UPDATE SET
                title = $3,
                fields = $4,
                changed = $5
            RETURNING item_id, language, title, fields, created, changed
            "#,
        )
        .bind(item_id)
        .bind(language)
        .bind(title)
        .bind(&fields)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .context("failed to save item translation")?;

        Ok(translation)
    }

    /// List all translations for an item.
    pub async fn list_for_item(&self, item_id: Uuid) -> Result<Vec<ItemTranslation>> {
        let translations = sqlx::query_as::<_, ItemTranslation>(
            r#"
            SELECT item_id, language, title, fields, created, changed
            FROM item_translation
            WHERE item_id = $1
            ORDER BY language
            "#,
        )
        .bind(item_id)
        .fetch_all(&self.pool)
        .await;

        match translations {
            Ok(t) => Ok(t),
            Err(e) => {
                debug!(error = %e, "item_translation table may not exist yet");
                Ok(Vec::new())
            }
        }
    }

    /// Delete a translation for an item.
    pub async fn delete(&self, item_id: Uuid, language: &str) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM item_translation WHERE item_id = $1 AND language = $2")
                .bind(item_id)
                .bind(language)
                .execute(&self.pool)
                .await
                .context("failed to delete item translation")?;

        Ok(result.rows_affected() > 0)
    }

    /// Overlay translation fields onto a base item's fields.
    ///
    /// Translation fields take precedence; base fields are used as fallback
    /// for any fields not present in the translation.
    pub fn overlay_fields(
        base_fields: &serde_json::Value,
        translation_fields: &serde_json::Value,
    ) -> serde_json::Value {
        match (base_fields, translation_fields) {
            (serde_json::Value::Object(base), serde_json::Value::Object(overlay)) => {
                let mut merged = base.clone();
                for (key, value) in overlay {
                    // Only overlay non-null values
                    if !value.is_null() {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                serde_json::Value::Object(merged)
            }
            _ => base_fields.clone(),
        }
    }
}

impl std::fmt::Debug for TranslationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranslationService").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_replaces_fields() {
        let base = serde_json::json!({
            "field_body": "English body",
            "field_tags": ["a", "b"]
        });
        let overlay = serde_json::json!({
            "field_body": "French body"
        });

        let result = TranslationService::overlay_fields(&base, &overlay);
        assert_eq!(result["field_body"], "French body");
        // Untranslated fields preserved
        assert_eq!(result["field_tags"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn overlay_skips_null() {
        let base = serde_json::json!({
            "field_body": "English body"
        });
        let overlay = serde_json::json!({
            "field_body": null
        });

        let result = TranslationService::overlay_fields(&base, &overlay);
        assert_eq!(result["field_body"], "English body");
    }

    #[test]
    fn overlay_with_non_objects() {
        let base = serde_json::json!("not an object");
        let overlay = serde_json::json!({"key": "value"});

        let result = TranslationService::overlay_fields(&base, &overlay);
        assert_eq!(result, serde_json::json!("not an object"));
    }
}
