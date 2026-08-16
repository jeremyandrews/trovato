//! Email verification token model for user registration.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Email verification token validity period (24 hours).
const TOKEN_VALIDITY_HOURS: i64 = 24;

/// Token purpose: registration activation or email address change.
pub const PURPOSE_REGISTRATION: &str = "registration";
/// Token purpose: email address change verification.
pub const PURPOSE_EMAIL_CHANGE: &str = "email_change";

/// Email verification token record.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EmailVerificationToken {
    /// Token ID.
    pub id: Uuid,
    /// User this token belongs to.
    pub user_id: Uuid,
    /// SHA-256 hash of the plain token.
    pub token_hash: String,
    /// Token purpose (`"registration"` or `"email_change"`).
    pub purpose: String,
    /// When this token expires.
    pub expires_at: DateTime<Utc>,
    /// When this token was used (None if unused).
    pub used_at: Option<DateTime<Utc>>,
    /// When this token was created.
    pub created: DateTime<Utc>,
}

impl EmailVerificationToken {
    /// Create a new email verification token for a user.
    ///
    /// `purpose` should be [`PURPOSE_REGISTRATION`] or [`PURPOSE_EMAIL_CHANGE`].
    ///
    /// Returns `(token_record, plain_token)` where `plain_token` should be
    /// sent to the user via email.
    pub async fn create(pool: &PgPool, user_id: Uuid, purpose: &str) -> Result<(Self, String)> {
        let plain_token = generate_token();
        let token_hash = hash_token(&plain_token);

        let id = Uuid::now_v7();
        let expires_at = Utc::now() + Duration::hours(TOKEN_VALIDITY_HOURS);

        let record = sqlx::query_as::<_, EmailVerificationToken>(
            r#"
            INSERT INTO email_verification_tokens (id, user_id, token_hash, purpose, expires_at)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(&token_hash)
        .bind(purpose)
        .bind(expires_at)
        .fetch_one(pool)
        .await
        .context("failed to create email verification token")?;

        Ok((record, plain_token))
    }

    /// Find a valid token by its plain text value and purpose.
    ///
    /// Returns `None` if the token doesn't exist, is expired, already used,
    /// or doesn't match the expected purpose.
    pub async fn find_valid(
        pool: &PgPool,
        plain_token: &str,
        purpose: &str,
    ) -> Result<Option<Self>> {
        let token_hash = hash_token(plain_token);

        let token = sqlx::query_as::<_, EmailVerificationToken>(
            r#"
            SELECT * FROM email_verification_tokens
            WHERE token_hash = $1
              AND purpose = $2
              AND expires_at > NOW()
              AND used_at IS NULL
            "#,
        )
        .bind(&token_hash)
        .bind(purpose)
        .fetch_optional(pool)
        .await
        .context("failed to find email verification token")?;

        Ok(token)
    }

    /// Mark a token as used.
    pub async fn mark_used(pool: &PgPool, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE email_verification_tokens SET used_at = NOW() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to mark verification token as used")?;

        Ok(())
    }

    /// Delete expired tokens (cleanup job).
    ///
    /// Removes tokens whose expiry time has passed. Used tokens are retained
    /// until their expiry to preserve an audit trail.
    pub async fn cleanup_expired(pool: &PgPool) -> Result<u64> {
        let result = sqlx::query("DELETE FROM email_verification_tokens WHERE expires_at < NOW()")
            .execute(pool)
            .await
            .context("failed to cleanup expired verification tokens")?;

        Ok(result.rows_affected())
    }

    /// Invalidate all tokens for a user.
    pub async fn invalidate_user_tokens(pool: &PgPool, user_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE email_verification_tokens SET used_at = NOW() WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .execute(pool)
        .await
        .context("failed to invalidate user verification tokens")?;

        Ok(())
    }
}

/// Generate a secure random token.
fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

/// Hash a token for storage using SHA-256.
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
        let token = "test_verification_token";
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
