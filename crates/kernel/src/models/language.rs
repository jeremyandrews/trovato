//! Language model and CRUD operations.
//!
//! Languages are site-level configuration entities. Monolingual sites (v1.0 default)
//! use only 'en'; multilingual plugins (post-MVP) add additional languages.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Valid text direction values.
const VALID_DIRECTIONS: &[&str] = &["ltr", "rtl"];

/// Language record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Language {
    /// Language code (e.g., "en", "fr", "de").
    pub id: String,

    /// Human-readable label (e.g., "English").
    pub label: String,

    /// Sort weight for language ordering.
    pub weight: i32,

    /// Whether this is the site default language.
    pub is_default: bool,

    /// Text direction: "ltr" or "rtl".
    pub direction: String,
}

/// Input for creating or updating a language.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateLanguage {
    pub id: String,
    pub label: String,
    pub weight: Option<i32>,
    pub is_default: Option<bool>,
    pub direction: Option<String>,
}

/// Validate that a label is non-empty and at most 255 characters.
fn validate_label(label: &str) -> Result<()> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        anyhow::bail!("language label must not be empty");
    }
    if trimmed.len() > 255 {
        anyhow::bail!(
            "language label must be at most 255 characters, got {}",
            trimmed.len()
        );
    }
    Ok(())
}

/// Validate that a direction string is "ltr" or "rtl".
fn validate_direction(direction: &str) -> Result<()> {
    if VALID_DIRECTIONS.contains(&direction) {
        Ok(())
    } else {
        anyhow::bail!("invalid direction '{direction}': must be 'ltr' or 'rtl'")
    }
}

/// Validate that a language ID follows BCP 47 primary subtag format.
///
/// Accepts: lowercase alpha 2-3 chars, optionally followed by hyphen-separated
/// alphanumeric subtags (e.g., "en", "fr", "pt-br", "zh-hans").
fn validate_language_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 12 {
        anyhow::bail!("language ID must be 1-12 characters, got '{id}'");
    }

    let mut parts = id.split('-');

    // Primary subtag: 2-3 lowercase letters
    match parts.next() {
        Some(primary) if (2..=3).contains(&primary.len()) => {
            if !primary.bytes().all(|b| b.is_ascii_lowercase()) {
                anyhow::bail!("language ID primary subtag must be lowercase letters, got '{id}'");
            }
        }
        _ => {
            anyhow::bail!("language ID must start with a 2-3 letter primary subtag, got '{id}'");
        }
    }

    // Optional subtags: alphanumeric, 1-8 chars each
    for subtag in parts {
        if subtag.is_empty()
            || subtag.len() > 8
            || !subtag.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            anyhow::bail!(
                "language ID subtag must be 1-8 alphanumeric characters, got '{subtag}' in '{id}'"
            );
        }
    }

    Ok(())
}

impl Language {
    /// Find a language by ID.
    pub async fn find_by_id(pool: &PgPool, id: &str) -> Result<Option<Self>> {
        let lang = sqlx::query_as::<_, Language>(
            "SELECT id, label, weight, is_default, direction FROM language WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch language by id")?;

        Ok(lang)
    }

    /// Get the default language.
    pub async fn get_default(pool: &PgPool) -> Result<Option<Self>> {
        let lang = sqlx::query_as::<_, Language>(
            "SELECT id, label, weight, is_default, direction FROM language WHERE is_default = true",
        )
        .fetch_optional(pool)
        .await
        .context("failed to fetch default language")?;

        Ok(lang)
    }

    /// List all languages ordered by weight.
    pub async fn list_all(pool: &PgPool) -> Result<Vec<Self>> {
        let langs = sqlx::query_as::<_, Language>(
            "SELECT id, label, weight, is_default, direction FROM language ORDER BY weight, id",
        )
        .fetch_all(pool)
        .await
        .context("failed to list languages")?;

        Ok(langs)
    }

    /// Upsert a language (insert or update).
    ///
    /// If `is_default` is true, clears the default flag on all other languages first
    /// (the DB has a unique partial index enforcing at most one default).
    pub async fn upsert(pool: &PgPool, input: CreateLanguage) -> Result<Self> {
        // Trim id and label for consistency
        let id = input.id.trim().to_string();
        let label = input.label.trim().to_string();

        validate_language_id(&id)?;
        validate_label(&label)?;

        let weight = input.weight.unwrap_or(0);
        let is_default = input.is_default.unwrap_or(false);
        let direction = input.direction.unwrap_or_else(|| "ltr".to_string());

        validate_direction(&direction)?;

        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // If unsetting is_default, ensure this wouldn't orphan the site (no default left).
        // FOR UPDATE locks prevent concurrent upserts from racing past the check.
        if !is_default {
            let current_default: Option<String> = sqlx::query_scalar(
                "SELECT id FROM language WHERE is_default = true AND id != $1 LIMIT 1 FOR UPDATE",
            )
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .context("failed to check for other default languages")?;

            // Check if this language is currently the default
            let is_currently_default: Option<bool> =
                sqlx::query_scalar("SELECT is_default FROM language WHERE id = $1 FOR UPDATE")
                    .bind(&id)
                    .fetch_optional(&mut *tx)
                    .await
                    .context("failed to check current language default status")?;

            if is_currently_default == Some(true) && current_default.is_none() {
                anyhow::bail!(
                    "cannot remove default flag from language '{id}': no other default language exists"
                );
            }
        }

        // If setting as default, clear default on all other languages first
        if is_default {
            sqlx::query("UPDATE language SET is_default = false WHERE id != $1")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .context("failed to clear previous default language")?;
        }

        let lang = sqlx::query_as::<_, Language>(
            r#"
            INSERT INTO language (id, label, weight, is_default, direction)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (id) DO UPDATE SET
                label = EXCLUDED.label,
                weight = EXCLUDED.weight,
                is_default = EXCLUDED.is_default,
                direction = EXCLUDED.direction
            RETURNING id, label, weight, is_default, direction
            "#,
        )
        .bind(&id)
        .bind(&label)
        .bind(weight)
        .bind(is_default)
        .bind(&direction)
        .fetch_one(&mut *tx)
        .await
        .context("failed to upsert language")?;

        tx.commit().await.context("failed to commit transaction")?;

        Ok(lang)
    }

    /// Delete a language.
    ///
    /// Prevents deletion if:
    /// - The language is the site default
    /// - Items or URL aliases still reference it
    ///
    /// All checks and the delete run in a single transaction with
    /// `FOR UPDATE` to prevent TOCTOU races.
    pub async fn delete(pool: &PgPool, id: &str) -> Result<bool> {
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Lock the row and check if it's the default
        let is_default: Option<bool> =
            sqlx::query_scalar("SELECT is_default FROM language WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .context("failed to check language default status")?;

        match is_default {
            Some(true) => anyhow::bail!("cannot delete the default language"),
            Some(false) => {}
            None => return Ok(false),
        }

        // Check for referencing items
        let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE language = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to count items referencing language")?;

        if item_count > 0 {
            anyhow::bail!("cannot delete language '{id}': {item_count} item(s) still reference it");
        }

        // Check for referencing URL aliases
        let alias_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM url_alias WHERE language = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .context("failed to count url_aliases referencing language")?;

        if alias_count > 0 {
            anyhow::bail!(
                "cannot delete language '{id}': {alias_count} URL alias(es) still reference it"
            );
        }

        let result = sqlx::query("DELETE FROM language WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to delete language")?;

        tx.commit().await.context("failed to commit transaction")?;

        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn language_struct_creation() {
        let lang = Language {
            id: "en".to_string(),
            label: "English".to_string(),
            weight: 0,
            is_default: true,
            direction: "ltr".to_string(),
        };

        assert_eq!(lang.id, "en");
        assert_eq!(lang.label, "English");
        assert!(lang.is_default);
        assert_eq!(lang.direction, "ltr");
    }

    #[test]
    fn language_equality() {
        let a = Language {
            id: "en".to_string(),
            label: "English".to_string(),
            weight: 0,
            is_default: true,
            direction: "ltr".to_string(),
        };
        let b = Language {
            id: "en".to_string(),
            label: "English".to_string(),
            weight: 0,
            is_default: true,
            direction: "ltr".to_string(),
        };
        let c = Language {
            id: "fr".to_string(),
            label: "French".to_string(),
            weight: 1,
            is_default: false,
            direction: "ltr".to_string(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn language_serialization_round_trip() {
        let lang = Language {
            id: "ar".to_string(),
            label: "Arabic".to_string(),
            weight: 5,
            is_default: false,
            direction: "rtl".to_string(),
        };

        let json = serde_json::to_string(&lang).unwrap();
        let parsed: Language = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, "ar");
        assert_eq!(parsed.label, "Arabic");
        assert_eq!(parsed.weight, 5);
        assert!(!parsed.is_default);
        assert_eq!(parsed.direction, "rtl");
    }

    #[test]
    fn create_language_input() {
        let input = CreateLanguage {
            id: "fr".to_string(),
            label: "French".to_string(),
            weight: None,
            is_default: None,
            direction: None,
        };

        assert_eq!(input.id, "fr");
        assert_eq!(input.label, "French");
    }

    #[test]
    fn validate_direction_accepts_ltr() {
        assert!(validate_direction("ltr").is_ok());
    }

    #[test]
    fn validate_direction_accepts_rtl() {
        assert!(validate_direction("rtl").is_ok());
    }

    #[test]
    fn validate_direction_rejects_invalid() {
        assert!(validate_direction("up").is_err());
        assert!(validate_direction("").is_err());
        assert!(validate_direction("xyz").is_err());
    }

    #[test]
    fn validate_label_accepts_valid() {
        assert!(validate_label("English").is_ok());
        assert!(validate_label("中文").is_ok());
        assert!(validate_label("  Trimmed  ").is_ok());
    }

    #[test]
    fn validate_label_rejects_invalid() {
        assert!(validate_label("").is_err(), "empty");
        assert!(validate_label("   ").is_err(), "whitespace only");
        let long = "a".repeat(256);
        assert!(validate_label(&long).is_err(), "too long");
    }

    #[test]
    fn validate_language_id_accepts_valid() {
        assert!(validate_language_id("en").is_ok());
        assert!(validate_language_id("fr").is_ok());
        assert!(validate_language_id("de").is_ok());
        assert!(validate_language_id("pt-br").is_ok());
        assert!(validate_language_id("zh-hans").is_ok());
        assert!(validate_language_id("ast").is_ok()); // 3-letter primary
    }

    #[test]
    fn validate_language_id_rejects_invalid() {
        assert!(validate_language_id("").is_err(), "empty");
        assert!(validate_language_id("e").is_err(), "too short");
        assert!(validate_language_id("EN").is_err(), "uppercase");
        assert!(validate_language_id("en us").is_err(), "space");
        assert!(validate_language_id("../foo").is_err(), "path traversal");
        assert!(validate_language_id("<script>").is_err(), "html");
        assert!(validate_language_id("en-").is_err(), "trailing hyphen");
        assert!(
            validate_language_id("abcdefghijklm").is_err(),
            "too long overall"
        );
    }
}
