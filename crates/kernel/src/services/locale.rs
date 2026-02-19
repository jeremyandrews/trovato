//! Locale service for interface string translation.
//!
//! Loads translations into an in-memory cache and provides a
//! translate() method used by the Tera t() function.

use anyhow::{Context, Result};
use dashmap::DashMap;
use sqlx::PgPool;
use tracing::info;

/// Locale translation service.
#[derive(Clone)]
pub struct LocaleService {
    pool: PgPool,
    /// In-memory translation cache: key = "language:context:source" → translation.
    cache: DashMap<String, String>,
}

impl LocaleService {
    /// Create a new locale service.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cache: DashMap::new(),
        }
    }

    /// Load all translations for a language into the cache.
    pub async fn load_language(&self, language: &str) -> Result<usize> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            r#"
            SELECT source, translation, context
            FROM locale_string
            WHERE language = $1
            "#,
        )
        .bind(language)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        let count = rows.len();
        for (source, translation, context) in rows {
            let key = cache_key(language, &context, &source);
            self.cache.insert(key, translation);
        }

        info!(language = %language, count = count, "loaded locale translations");
        Ok(count)
    }

    /// Translate a source string.
    ///
    /// Falls back to the source string if no translation is found.
    pub fn translate(&self, source: &str, context: &str, language: &str) -> String {
        let key = cache_key(language, context, source);
        if let Some(translation) = self.cache.get(&key) {
            return translation.clone();
        }

        // Try without context
        if !context.is_empty() {
            let key = cache_key(language, "", source);
            if let Some(translation) = self.cache.get(&key) {
                return translation.clone();
            }
        }

        // Return source as fallback
        source.to_string()
    }

    /// Bulk insert translations from parsed .po data.
    pub async fn import_translations(
        &self,
        language: &str,
        translations: &[(String, String, String)], // (source, translation, context)
    ) -> Result<usize> {
        let mut count = 0usize;

        for (source, translation, context) in translations {
            sqlx::query(
                r#"
                INSERT INTO locale_string (id, source, translation, language, context)
                VALUES (gen_random_uuid(), $1, $2, $3, $4)
                ON CONFLICT (source, language, context) DO UPDATE SET translation = $2
                "#,
            )
            .bind(source)
            .bind(translation)
            .bind(language)
            .bind(context)
            .execute(&self.pool)
            .await
            .context("failed to import translation")?;

            // Update cache
            let key = cache_key(language, context, source);
            self.cache.insert(key, translation.clone());
            count += 1;
        }

        info!(language = %language, count = count, "imported translations");
        Ok(count)
    }

    /// Clear the translation cache.
    pub fn clear_cache(&self) {
        self.cache.clear();
    }
}

/// Build a cache key from language, context, and source.
///
/// Uses null byte separator (`\0`) to prevent collisions when source or
/// context strings contain colons (e.g., "12:00" as a source string).
fn cache_key(language: &str, context: &str, source: &str) -> String {
    format!("{language}\0{context}\0{source}")
}

impl std::fmt::Debug for LocaleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocaleService")
            .field("cache_size", &self.cache.len())
            .finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Helper: test translation lookup against a plain DashMap cache.
    fn translate(
        cache: &DashMap<String, String>,
        source: &str,
        context: &str,
        language: &str,
    ) -> String {
        let key = cache_key(language, context, source);
        if let Some(translation) = cache.get(&key) {
            return translation.clone();
        }
        if !context.is_empty() {
            let key = cache_key(language, "", source);
            if let Some(translation) = cache.get(&key) {
                return translation.clone();
            }
        }
        source.to_string()
    }

    #[test]
    fn cache_key_format() {
        assert_eq!(cache_key("fr", "", "Hello"), "fr\0\0Hello");
        assert_eq!(cache_key("fr", "menu", "Hello"), "fr\0menu\0Hello");
    }

    #[test]
    fn cache_key_no_collision_with_colons() {
        // Source strings containing colons should not collide
        let k1 = cache_key("en", "", "12:00");
        let k2 = cache_key("en", "12", "00");
        assert_ne!(k1, k2);
    }

    #[test]
    fn translate_returns_source_when_no_translation() {
        let cache = DashMap::new();
        assert_eq!(translate(&cache, "Hello", "", "en"), "Hello");
    }

    #[test]
    fn translate_returns_cached_translation() {
        let cache = DashMap::new();
        cache.insert("fr\0\0Hello".to_string(), "Bonjour".to_string());
        assert_eq!(translate(&cache, "Hello", "", "fr"), "Bonjour");
    }

    #[test]
    fn translate_context_fallback() {
        let cache = DashMap::new();
        cache.insert("fr\0\0Save".to_string(), "Enregistrer".to_string());
        assert_eq!(translate(&cache, "Save", "form", "fr"), "Enregistrer");
    }
}
