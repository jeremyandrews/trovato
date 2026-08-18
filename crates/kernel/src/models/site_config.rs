//! Site configuration model for installation status and site settings.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Site configuration record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SiteConfig {
    /// Configuration key.
    pub key: String,

    /// Configuration value (JSON).
    pub value: serde_json::Value,

    /// When this config was last updated.
    pub updated: chrono::DateTime<chrono::Utc>,
}

/// Site setting naming who may create an account.
pub const USER_REGISTRATION_KEY: &str = "user_registration";

/// The boolean this setting replaces.
///
/// Read as a fallback so a site that opened registration through config import —
/// the only way it could be done — keeps working, and cleared the first time the
/// admin form saves, so the two cannot disagree afterwards.
pub const LEGACY_REGISTRATION_KEY: &str = "allow_user_registration";

/// Who may create an account.
///
/// The admin form offered three modes (open, admin_only, closed) and saved them,
/// while the register route gated on the unrelated boolean
/// `allow_user_registration` — so the selector changed nothing and the only way to
/// open registration was a config import of the boolean. This is the one key now,
/// and the route honours it.
///
/// Two modes rather than three: `admin_only` and `closed` differed in wording
/// only. Both close the public register route, and neither can stop an
/// administrator creating an account, which is the one account-creation path that
/// has to keep working. A stored `closed` still reads as closed to the public. A
/// genuine third mode would be registration *with approval*, which needs an
/// approval queue rather than a third label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    /// Anyone may register at `/user/register`.
    Open,
    /// Only an administrator creates accounts; the public route is closed.
    AdminOnly,
}

impl RegistrationMode {
    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::AdminOnly => "admin_only",
        }
    }

    /// Whether the public registration route is available.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }

    /// Resolve the mode from the two settings' raw values.
    ///
    /// Precedence: an explicit `user_registration` wins; otherwise the legacy
    /// boolean decides; otherwise closed. An unrecognised mode string is closed
    /// too — with registration, the safe direction for a value nobody can parse is
    /// "not open".
    ///
    /// Split from the database read so the precedence is testable without one.
    pub fn resolve(
        configured: Option<&serde_json::Value>,
        legacy: Option<&serde_json::Value>,
    ) -> Self {
        if let Some(value) = configured.and_then(|v| v.as_str()) {
            return match value {
                "open" => Self::Open,
                // `closed` is accepted and means closed; see the type docs.
                "admin_only" | "closed" => Self::AdminOnly,
                other => {
                    tracing::warn!(
                        value = %other,
                        "unrecognised {USER_REGISTRATION_KEY}; treating registration as closed"
                    );
                    Self::AdminOnly
                }
            };
        }

        if legacy.and_then(|v| v.as_bool()) == Some(true) {
            return Self::Open;
        }

        Self::AdminOnly
    }

    /// Load the effective mode.
    pub async fn load(pool: &PgPool) -> Self {
        let configured = SiteConfig::get(pool, USER_REGISTRATION_KEY)
            .await
            .ok()
            .flatten();
        let legacy = SiteConfig::get(pool, LEGACY_REGISTRATION_KEY)
            .await
            .ok()
            .flatten();

        Self::resolve(configured.as_ref(), legacy.as_ref())
    }

    /// Store the mode, and drop the boolean it supersedes.
    ///
    /// Clearing the legacy key is the migration: after one save there is one
    /// setting, and a stale `allow_user_registration` cannot contradict it.
    pub async fn save(self, pool: &PgPool) -> Result<()> {
        SiteConfig::set(
            pool,
            USER_REGISTRATION_KEY,
            serde_json::json!(self.as_str()),
        )
        .await?;
        SiteConfig::delete(pool, LEGACY_REGISTRATION_KEY).await?;
        Ok(())
    }
}

impl SiteConfig {
    /// Get a configuration value by key (default tenant).
    pub async fn get(pool: &PgPool, key: &str) -> Result<Option<serde_json::Value>> {
        Self::get_for_tenant(pool, key, crate::models::tenant::DEFAULT_TENANT_ID).await
    }

    /// Get a configuration value by key for a specific tenant.
    ///
    /// Falls back to the default tenant if the key is not found
    /// for the requested tenant.
    pub async fn get_for_tenant(
        pool: &PgPool,
        key: &str,
        tenant_id: uuid::Uuid,
    ) -> Result<Option<serde_json::Value>> {
        // Try tenant-specific config first
        let result = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT value FROM site_config WHERE key = $1 AND tenant_id = $2",
        )
        .bind(key)
        .bind(tenant_id)
        .fetch_optional(pool)
        .await
        .context("failed to get site config")?;

        if result.is_some() {
            return Ok(result);
        }

        // Fall back to default tenant
        if tenant_id != crate::models::tenant::DEFAULT_TENANT_ID {
            let fallback = sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT value FROM site_config WHERE key = $1 AND tenant_id = $2",
            )
            .bind(key)
            .bind(crate::models::tenant::DEFAULT_TENANT_ID)
            .fetch_optional(pool)
            .await
            .context("failed to get default tenant config")?;

            return Ok(fallback);
        }

        Ok(None)
    }

    /// Set a configuration value (default tenant).
    pub async fn set(pool: &PgPool, key: &str, value: serde_json::Value) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO site_config (key, value, updated)
            VALUES ($1, $2, NOW())
            ON CONFLICT (key) DO UPDATE SET value = $2, updated = NOW()
            "#,
        )
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .context("failed to set site config")?;

        Ok(())
    }

    /// Get a config value with secret reference resolution.
    ///
    /// String values prefixed with `env:` are resolved from environment
    /// variables at read time. The prefix `literal:` escapes the `env:` prefix
    /// for values that literally start with `env:`.
    ///
    /// Examples:
    /// - `"env:OPENAI_API_KEY"` → reads `OPENAI_API_KEY` from env
    /// - `"literal:env:NOT_A_SECRET"` → returns `"env:NOT_A_SECRET"`
    /// - `"plain value"` → returns `"plain value"` unchanged
    pub async fn get_resolved(pool: &PgPool, key: &str) -> Result<Option<serde_json::Value>> {
        let value = Self::get(pool, key).await?;
        Ok(value.map(Self::resolve_secret_refs))
    }

    /// Resolve secret references in a JSON value.
    ///
    /// Recursively processes strings with `env:` and `literal:` prefixes.
    /// Non-string values are returned unchanged.
    fn resolve_secret_refs(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => {
                serde_json::Value::String(Self::resolve_secret_string(&s))
            }
            serde_json::Value::Object(map) => {
                let resolved: serde_json::Map<String, serde_json::Value> = map
                    .into_iter()
                    .map(|(k, v)| (k, Self::resolve_secret_refs(v)))
                    .collect();
                serde_json::Value::Object(resolved)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.into_iter().map(Self::resolve_secret_refs).collect())
            }
            other => other,
        }
    }

    /// Resolve a single string value's secret reference.
    fn resolve_secret_string(s: &str) -> String {
        if let Some(var_name) = s.strip_prefix("env:") {
            // Read from environment variable
            std::env::var(var_name).unwrap_or_else(|_| {
                tracing::warn!(var = var_name, "secret config references missing env var");
                String::new()
            })
        } else if let Some(rest) = s.strip_prefix("literal:") {
            // Escape mechanism: strip the literal: prefix and return the rest
            rest.to_string()
        } else {
            s.to_string()
        }
    }

    /// Check if the site is installed.
    /// Delete a setting.
    ///
    /// Used when one key supersedes another, so a site is not left with two
    /// settings that can disagree.
    pub async fn delete(pool: &PgPool, key: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM site_config WHERE key = $1")
            .bind(key)
            .execute(pool)
            .await
            .context("failed to delete site config")?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn is_installed(pool: &PgPool) -> Result<bool> {
        let value = Self::get(pool, "installed").await?;
        Ok(value.map(|v| v.as_bool().unwrap_or(false)).unwrap_or(false))
    }

    /// Mark the site as installed.
    pub async fn mark_installed(pool: &PgPool) -> Result<()> {
        Self::set(pool, "installed", serde_json::json!(true)).await
    }

    /// Get the site name.
    pub async fn site_name(pool: &PgPool) -> Result<String> {
        let value = Self::get(pool, "site_name").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "Trovato".to_string()))
    }

    /// Set the site name.
    pub async fn set_site_name(pool: &PgPool, name: &str) -> Result<()> {
        Self::set(pool, "site_name", serde_json::json!(name)).await
    }

    /// Get the site slogan.
    pub async fn site_slogan(pool: &PgPool) -> Result<String> {
        let value = Self::get(pool, "site_slogan").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default())
    }

    /// Set the site slogan.
    pub async fn set_site_slogan(pool: &PgPool, slogan: &str) -> Result<()> {
        Self::set(pool, "site_slogan", serde_json::json!(slogan)).await
    }

    /// Get the site email.
    pub async fn site_mail(pool: &PgPool) -> Result<String> {
        let value = Self::get(pool, "site_mail").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default())
    }

    /// Set the site email.
    pub async fn set_site_mail(pool: &PgPool, mail: &str) -> Result<()> {
        Self::set(pool, "site_mail", serde_json::json!(mail)).await
    }

    /// Get the front page path.
    pub async fn front_page(pool: &PgPool) -> Result<Option<String>> {
        let value = Self::get(pool, "site_front_page").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .filter(|s| !s.is_empty()))
    }

    /// Set the front page path (e.g., "/item/{uuid}").
    pub async fn set_front_page(pool: &PgPool, path: &str) -> Result<()> {
        Self::set(pool, "site_front_page", serde_json::json!(path)).await
    }

    /// Get all configuration as a map.
    pub async fn all(
        pool: &PgPool,
    ) -> Result<std::collections::HashMap<String, serde_json::Value>> {
        let configs =
            sqlx::query_as::<_, SiteConfig>("SELECT key, value, updated FROM site_config")
                .fetch_all(pool)
                .await
                .context("failed to get all site configs")?;

        Ok(configs.into_iter().map(|c| (c.key, c.value)).collect())
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The defect in one assertion: the mode the admin form saves is the mode the
    /// route reads.
    #[test]
    fn an_explicit_mode_decides() {
        assert_eq!(
            RegistrationMode::resolve(Some(&json!("open")), None),
            RegistrationMode::Open
        );
        assert_eq!(
            RegistrationMode::resolve(Some(&json!("admin_only")), None),
            RegistrationMode::AdminOnly
        );
    }

    /// A stored `closed` from before the reduction to two modes still means closed
    /// to the public, so no site's behaviour changes under it.
    #[test]
    fn a_stored_closed_mode_is_closed_to_the_public() {
        let mode = RegistrationMode::resolve(Some(&json!("closed")), None);

        assert_eq!(mode, RegistrationMode::AdminOnly);
        assert!(!mode.is_open());
    }

    /// The migration path: a site that opened registration through the boolean —
    /// which was the only way — keeps working before it ever saves the form.
    #[test]
    fn the_legacy_boolean_still_opens_registration() {
        assert_eq!(
            RegistrationMode::resolve(None, Some(&json!(true))),
            RegistrationMode::Open
        );
        assert_eq!(
            RegistrationMode::resolve(None, Some(&json!(false))),
            RegistrationMode::AdminOnly
        );
    }

    /// An explicit mode wins over the boolean, so the form is in charge once it
    /// has been used.
    #[test]
    fn an_explicit_mode_overrides_the_legacy_boolean() {
        assert_eq!(
            RegistrationMode::resolve(Some(&json!("admin_only")), Some(&json!(true))),
            RegistrationMode::AdminOnly
        );
        assert_eq!(
            RegistrationMode::resolve(Some(&json!("open")), Some(&json!(false))),
            RegistrationMode::Open
        );
    }

    /// Nothing configured means closed, which is what the boolean defaulted to.
    #[test]
    fn nothing_configured_is_closed() {
        assert_eq!(
            RegistrationMode::resolve(None, None),
            RegistrationMode::AdminOnly
        );
    }

    /// A value nobody can parse must not open registration.
    #[test]
    fn an_unparseable_mode_fails_closed() {
        for value in [json!("opne"), json!(true), json!(1), json!(null)] {
            assert_eq!(
                RegistrationMode::resolve(Some(&value), None),
                RegistrationMode::AdminOnly,
                "{value} must not open registration"
            );
        }
    }

    /// The stored spelling is a wire format: the form posts these strings and
    /// config import writes them.
    #[test]
    fn the_stored_spellings_are_stable() {
        assert_eq!(RegistrationMode::Open.as_str(), "open");
        assert_eq!(RegistrationMode::AdminOnly.as_str(), "admin_only");
    }
}
