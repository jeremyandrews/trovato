//! TOTP second-factor and recovery-code storage (B304).
//!
//! `UserTotp.secret` is never the plain TOTP secret: it is the output of
//! [`crate::secret_box::SecretBox::seal`], purpose
//! [`crate::secret_box::PURPOSE_TOTP_SECRET`], AAD the user id. A row with
//! `confirmed_at` `NULL` is an unfinished enrolment (the setup page was shown
//! but never confirmed with a right code) and never counts as "has TOTP"
//! anywhere in the kernel — [`UserTotp::confirmed`] is the one query that
//! matters for that question. `last_used_step` is the RFC 6238 time-step of
//! the last accepted code; [`UserTotp::accept_step`] only advances it, so the
//! same code (or an earlier step) can never be replayed.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// One account's TOTP enrolment.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserTotp {
    pub user_id: Uuid,
    /// Sealed secret (see module docs); never the plain secret.
    pub secret: Vec<u8>,
    /// `NULL` until the setup page's confirmation code checks out.
    pub confirmed_at: Option<DateTime<Utc>>,
    pub last_used_step: i64,
    pub created_at: DateTime<Utc>,
}

impl UserTotp {
    /// The row for `user_id`, confirmed or not.
    pub async fn find(pool: &PgPool, user_id: Uuid) -> Result<Option<Self>> {
        sqlx::query_as::<_, Self>("SELECT * FROM user_totp WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .context("failed to fetch TOTP enrolment")
    }

    /// The row for `user_id`, only when it is confirmed. The one question
    /// "does this account have TOTP" should ask: an unconfirmed enrolment
    /// (a setup page shown but abandoned) must never gate a login or satisfy
    /// a role's second-factor requirement.
    pub async fn confirmed(pool: &PgPool, user_id: Uuid) -> Result<Option<Self>> {
        sqlx::query_as::<_, Self>(
            "SELECT * FROM user_totp WHERE user_id = $1 AND confirmed_at IS NOT NULL",
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch confirmed TOTP enrolment")
    }

    /// Whether `user_id` has a confirmed TOTP enrolment.
    pub async fn is_confirmed(pool: &PgPool, user_id: Uuid) -> bool {
        Self::confirmed(pool, user_id)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    /// Start (or restart) an enrolment: upsert an unconfirmed row with a fresh
    /// sealed secret, replacing any prior unconfirmed one. A confirmed
    /// enrolment can only be replaced through [`Self::delete`] followed by
    /// this — starting setup again does not silently invalidate a working
    /// authenticator; the confirm step is what does, deliberately, by
    /// overwriting this row only once the new secret is proven.
    pub async fn start_enrolment(pool: &PgPool, user_id: Uuid, sealed_secret: &[u8]) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO user_totp (user_id, secret, confirmed_at, last_used_step)
            VALUES ($1, $2, NULL, 0)
            ON CONFLICT (user_id) DO UPDATE
                SET secret = $2, confirmed_at = NULL, last_used_step = 0
                WHERE user_totp.confirmed_at IS NULL
            "#,
        )
        .bind(user_id)
        .bind(sealed_secret)
        .execute(pool)
        .await
        .context("failed to start TOTP enrolment")?;
        Ok(())
    }

    /// Mark the row confirmed. No-op (via the `WHERE`) on a row that does not
    /// exist or is already confirmed, so a caller cannot re-confirm (and
    /// thereby reset `last_used_step`) a working enrolment.
    pub async fn confirm(pool: &PgPool, user_id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE user_totp SET confirmed_at = NOW() \
             WHERE user_id = $1 AND confirmed_at IS NULL",
        )
        .bind(user_id)
        .execute(pool)
        .await
        .context("failed to confirm TOTP enrolment")?;
        Ok(result.rows_affected() > 0)
    }

    /// Accept time-step `step` for `user_id`, advancing `last_used_step` in
    /// the same statement. Returns `false` (and writes nothing) when `step` is
    /// not strictly greater than the stored value — the replay defence: the
    /// same code, or an earlier one within the skew window, can never verify
    /// twice.
    pub async fn accept_step(pool: &PgPool, user_id: Uuid, step: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE user_totp SET last_used_step = $2 \
             WHERE user_id = $1 AND confirmed_at IS NOT NULL AND last_used_step < $2",
        )
        .bind(user_id)
        .bind(step)
        .execute(pool)
        .await
        .context("failed to record the accepted TOTP step")?;
        Ok(result.rows_affected() > 0)
    }

    /// Remove the enrolment (and, via `ON DELETE CASCADE`, its recovery
    /// codes). Returns whether a row existed.
    pub async fn delete(pool: &PgPool, user_id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM user_totp WHERE user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await
            .context("failed to delete TOTP enrolment")?;
        Ok(result.rows_affected() > 0)
    }
}

/// One recovery code, stored as an HMAC, never in the clear.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRecoveryCode {
    pub id: Uuid,
    pub user_id: Uuid,
    pub code_hash: Vec<u8>,
    pub used_at: Option<DateTime<Utc>>,
}

impl UserRecoveryCode {
    /// Replace every recovery code for `user_id` with `hashes` (one insert
    /// per code), inside the caller's transaction so a confirm that fails
    /// after the TOTP row is written does not leave recovery codes from a
    /// previous enrolment still redeemable against the new secret.
    pub async fn replace_all(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        hashes: &[Vec<u8>],
    ) -> Result<()> {
        sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut **tx)
            .await
            .context("failed to clear prior recovery codes")?;
        for hash in hashes {
            sqlx::query(
                "INSERT INTO user_recovery_codes (id, user_id, code_hash) VALUES ($1, $2, $3)",
            )
            .bind(Uuid::now_v7())
            .bind(user_id)
            .bind(hash)
            .execute(&mut **tx)
            .await
            .context("failed to store a recovery code")?;
        }
        Ok(())
    }

    /// Every unused recovery code for `user_id`, for a constant-time scan
    /// against a submitted code (there is no way to look one up by hash
    /// without timing-leaking which prefix matched, so the caller compares
    /// against all of them).
    pub async fn unused_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<Self>> {
        sqlx::query_as::<_, Self>(
            "SELECT * FROM user_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .context("failed to list recovery codes")
    }

    /// How many unused recovery codes `user_id` has left.
    pub async fn unused_count(pool: &PgPool, user_id: Uuid) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await
        .context("failed to count recovery codes")
    }

    /// Mark one code used. Returns `false` (and writes nothing) if it was
    /// already spent — single-use, enforced in the `WHERE` so two concurrent
    /// requests racing the same code cannot both succeed.
    pub async fn mark_used(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE user_recovery_codes SET used_at = NOW() WHERE id = $1 AND used_at IS NULL",
        )
        .bind(id)
        .execute(pool)
        .await
        .context("failed to spend a recovery code")?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete every recovery code for `user_id` (the admin reset, and a
    /// removal of the TOTP enrolment itself — though `ON DELETE CASCADE`
    /// already does the latter; this is for callers that want it explicit,
    /// e.g. inside a transaction that has not yet deleted the `user_totp` row).
    pub async fn delete_all_for_user(pool: &PgPool, user_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await
            .context("failed to delete recovery codes")?;
        Ok(())
    }
}
