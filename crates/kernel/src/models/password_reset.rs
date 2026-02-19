//! Password reset token model.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Password reset token validity period (1 hour).
const TOKEN_VALIDITY_HOURS: i64 = 1;

/// Password reset token record.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PasswordResetToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created: DateTime<Utc>,
}

impl PasswordResetToken {
    /// Create a new password reset token for a user.
    ///
    /// Returns (token_record, plain_token) where plain_token should be sent to the user.
    pub async fn create(pool: &PgPool, user_id: Uuid) -> Result<(Self, String)> {
        // Generate a secure random token
        let plain_token = generate_token();
        let token_hash = hash_token(&plain_token);

        let id = Uuid::now_v7();
        let expires_at = Utc::now() + Duration::hours(TOKEN_VALIDITY_HOURS);

        let record = sqlx::query_as::<_, PasswordResetToken>(
            r#"
            INSERT INTO password_reset_tokens (id, user_id, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(&token_hash)
        .bind(expires_at)
        .fetch_one(pool)
        .await
        .context("failed to create password reset token")?;

        Ok((record, plain_token))
    }

    /// Find a valid token by its plain text value.
    ///
    /// Returns None if token doesn't exist, is expired, or already used.
    pub async fn find_valid(pool: &PgPool, plain_token: &str) -> Result<Option<Self>> {
        let token_hash = hash_token(plain_token);

        let token = sqlx::query_as::<_, PasswordResetToken>(
            r#"
            SELECT * FROM password_reset_tokens
            WHERE token_hash = $1
              AND expires_at > NOW()
              AND used_at IS NULL
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .context("failed to find password reset token")?;

        Ok(token)
    }

    /// Mark a token as used.
    pub async fn mark_used(pool: &PgPool, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE password_reset_tokens SET used_at = NOW() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to mark token as used")?;

        Ok(())
    }

    /// Delete expired tokens (cleanup job).
    pub async fn cleanup_expired(pool: &PgPool) -> Result<u64> {
        let result = sqlx::query("DELETE FROM password_reset_tokens WHERE expires_at < NOW()")
            .execute(pool)
            .await
            .context("failed to cleanup expired tokens")?;

        Ok(result.rows_affected())
    }

    /// Invalidate all tokens for a user (e.g., after password change).
    pub async fn invalidate_user_tokens(pool: &PgPool, user_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE password_reset_tokens SET used_at = NOW() WHERE user_id = $1 AND used_at IS NULL")
            .bind(user_id)
            .execute(pool)
            .await
            .context("failed to invalidate user tokens")?;

        Ok(())
    }
}

/// Generate a secure random token.
fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

/// Hash a token for storage.
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
        let token = "test_token_12345";
        let hash1 = hash_token(token);
        let hash2 = hash_token(token);

        // Same token should produce same hash
        assert_eq!(hash1, hash2);

        // Different token should produce different hash
        let hash3 = hash_token("different_token");
        assert_ne!(hash1, hash3);

        // Hash should be hex-encoded SHA-256 (64 chars)
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_token_generation() {
        let token1 = generate_token();
        let token2 = generate_token();

        // Tokens should be different
        assert_ne!(token1, token2);

        // Tokens should be 64 hex chars (32 bytes)
        assert_eq!(token1.len(), 64);
    }
}
