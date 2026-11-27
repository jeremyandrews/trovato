//! API token model for headless CMS authentication.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

// No lookup cache, deliberately. A process-local cache of token lookups kept a
// revoked token working until its entry expired, kept an expired token working
// past `expires_at`, and could never be invalidated from another process (the
// MCP server, a second instance). The lookup is a unique-index equality on
// `token_hash`, so every request asks the database, and revocation takes effect
// on the next request in every process.

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
    pub async fn create<'e, E: sqlx::PgExecutor<'e>>(
        pool: E,
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

    /// Look up a token by its raw value. Returns `None` if not found, expired,
    /// or owned by an account that is not active.
    ///
    /// Always asks the database (see the note at the top of this module), so a
    /// token deleted or expired a moment ago is refused now. The owner's status
    /// is part of the same query, so a blocked account's tokens stop working at
    /// once, in every process that authenticates with them (the kernel and the
    /// MCP server alike), even before a block has deleted them.
    pub async fn find_by_token(pool: &PgPool, raw_token: &str) -> Result<Option<Self>> {
        let token_hash = hash_token(raw_token);

        let token = sqlx::query_as::<_, ApiToken>(
            r#"
            SELECT t.* FROM api_tokens t
            JOIN users u ON u.id = t.user_id
            WHERE t.token_hash = $1
              AND (t.expires_at IS NULL OR t.expires_at > NOW())
              AND u.status = 1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .context("failed to find API token")?;

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
    ///
    /// Returns whether a token was deleted. The next lookup of that token, in
    /// any process, finds nothing.
    pub async fn delete<'e, E: sqlx::PgExecutor<'e>>(
        pool: E,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<bool> {
        let result = sqlx::query("DELETE FROM api_tokens WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(pool)
            .await
            .context("failed to delete API token")?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete (revoke) every token a user holds, returning the ids of those
    /// that went (for the audit trail, which records each one).
    ///
    /// For when the account itself changes in a way that should end all of its
    /// credentials, such as being blocked.
    pub async fn delete_all_for_user<'e, E: sqlx::PgExecutor<'e>>(
        pool: E,
        user_id: Uuid,
    ) -> Result<Vec<Uuid>> {
        sqlx::query_scalar("DELETE FROM api_tokens WHERE user_id = $1 RETURNING id")
            .bind(user_id)
            .fetch_all(pool)
            .await
            .context("failed to delete a user's API tokens")
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
// Tests are allowed to use unwrap/expect freely.
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

    /// A token deleted after it was looked up is not served again.
    ///
    /// The lookup used to go through a process-local cache that kept a hit for
    /// a minute, so revocation took up to a minute to take effect — and never
    /// took effect in another process (the MCP server, a second instance) any
    /// sooner. Needs a database; without `DATABASE_URL` there is nothing to
    /// look a token up in, and the test says so and returns.
    #[tokio::test]
    async fn a_deleted_token_is_not_served_from_a_cache() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            eprintln!("DATABASE_URL unset; skipping the database-backed token test");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to the test database");
        crate::db::run_migrations(&pool)
            .await
            .expect("migrate the test database");

        let user_id = Uuid::now_v7();
        sqlx::query("INSERT INTO users (id, name, pass, mail, status) VALUES ($1, $2, '', $3, 1)")
            .bind(user_id)
            .bind(format!("api-token-unit-{}", user_id.simple()))
            .bind(format!("{}@example.com", user_id.simple()))
            .execute(&pool)
            .await
            .expect("create the token owner");

        let (token, raw) = ApiToken::create(&pool, user_id, "unit", None)
            .await
            .expect("mint");
        let first = ApiToken::find_by_token(&pool, &raw).await.expect("lookup");
        let deleted = ApiToken::delete(&pool, token.id, user_id)
            .await
            .expect("delete");
        let second = ApiToken::find_by_token(&pool, &raw).await.expect("lookup");

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("remove the token owner");

        assert!(first.is_some(), "the live token is found");
        assert!(deleted, "delete reports that it removed the token");
        assert!(
            second.is_none(),
            "a deleted token must not be served from a cache"
        );
    }

    #[test]
    fn test_token_generation() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 64);
    }
}
