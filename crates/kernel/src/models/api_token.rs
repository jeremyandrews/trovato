//! API token model for headless CMS authentication.

use std::sync::LazyLock;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Short-TTL cache for token lookups to avoid per-request DB hits.
/// Entries expire after 60 seconds. Revoked tokens may remain valid
/// in the cache for up to this duration.
static TOKEN_CACHE: LazyLock<moka::future::Cache<String, Option<ApiToken>>> = LazyLock::new(|| {
    moka::future::Cache::builder()
        .time_to_live(std::time::Duration::from_secs(60))
        .max_capacity(10_000)
        .build()
});

/// Maximum number of API tokens a single user may hold.
pub const MAX_TOKENS_PER_USER: i64 = 25;

/// API token record (never contains the raw token).
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct ApiToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    #[serde(skip)]
    pub token_hash: String,
    pub created: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl ApiToken {
    /// Create a new API token for a user.
    ///
    /// Returns `(ApiToken, raw_token)`. The raw token is shown once and never stored.
    pub async fn create(
        pool: &PgPool,
        user_id: Uuid,
        name: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(Self, String)> {
        let raw_token = generate_token();
        let token_hash = hash_token(&raw_token);
        let id = Uuid::now_v7();

        let record = sqlx::query_as::<_, ApiToken>(
            r#"
            INSERT INTO api_tokens (id, user_id, name, token_hash, expires_at)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(name)
        .bind(&token_hash)
        .bind(expires_at)
        .fetch_one(pool)
        .await
        .context("failed to create API token")?;

        Ok((record, raw_token))
    }

    /// Look up a token by its raw value. Returns `None` if not found or expired.
    ///
    /// Results are cached for 60 seconds to avoid per-request DB queries.
    pub async fn find_by_token(pool: &PgPool, raw_token: &str) -> Result<Option<Self>> {
        let token_hash = hash_token(raw_token);

        if let Some(cached) = TOKEN_CACHE.get(&token_hash).await {
            return Ok(cached);
        }

        let token = sqlx::query_as::<_, ApiToken>(
            r#"
            SELECT * FROM api_tokens
            WHERE token_hash = $1
              AND (expires_at IS NULL OR expires_at > NOW())
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .context("failed to find API token")?;

        TOKEN_CACHE.insert(token_hash, token.clone()).await;

        Ok(token)
    }

    /// Update the last_used timestamp.
    pub async fn touch_last_used(pool: &PgPool, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE api_tokens SET last_used = NOW() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to update last_used")?;
        Ok(())
    }

    /// List all tokens for a user (no raw values).
    pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<Self>> {
        let tokens = sqlx::query_as::<_, ApiToken>(
            "SELECT * FROM api_tokens WHERE user_id = $1 ORDER BY created DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .context("failed to list API tokens")?;

        Ok(tokens)
    }

    /// Count tokens for a user.
    pub async fn count_for_user(pool: &PgPool, user_id: Uuid) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM api_tokens WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .context("failed to count API tokens")?;
        Ok(row.0)
    }

    /// Delete (revoke) a token, scoped to the owning user.
    pub async fn delete(pool: &PgPool, id: Uuid, user_id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM api_tokens WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(pool)
            .await
            .context("failed to delete API token")?;

        Ok(result.rows_affected() > 0)
    }
}

/// Generate a 32-byte random hex token.
fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

/// SHA-256 hash a token for storage.
fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_token_hashing() {
        let token = "test_api_token_12345";
        let hash1 = hash_token(token);
        let hash2 = hash_token(token);
        assert_eq!(hash1, hash2);

        let hash3 = hash_token("different_token");
        assert_ne!(hash1, hash3);

        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_token_generation() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 64);
    }
}
