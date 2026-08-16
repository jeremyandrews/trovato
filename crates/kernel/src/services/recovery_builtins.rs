//! The two built-in recovery paths (FR-7c AC-2, design §4.5).
//!
//! Both implement [`RecoveryProvider`], which is the frozen
//! `tap_account_recovery` contract verbatim, so their answers are folded by the
//! same owner-scoped fail-closed fold as any plugin's. See
//! [`crate::services::recovery_flow`] for why they are in-process rather than
//! bundled WASM.
//!
//! # What each path is for, and what it costs
//!
//! - **Emailed code** — the familiar path, and the honest weak link. It
//!   reintroduces exactly the phishable factor a passkey removed, which is why
//!   an admin can switch it off and require something stronger. It is inert
//!   without `SMTP_HOST`, so a deployment with no mail configured does not
//!   silently advertise a method that can never deliver.
//! - **Pre-generated codes** — the offline path. No email, no second device: the
//!   user keeps the codes. Hashed at rest and single-use.
//!
//! Neither path can widen a flow: both answer `verify` with a bare verdict, the
//! same as any plugin, because that is all the frozen result shape allows.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::recovery::{RecoveryMethod, RecoveryTapInput, RecoveryTapResult, Verdict};
use crate::services::recovery_flow::{
    BUILTIN_CODES_PROVIDER, BUILTIN_EMAIL_PROVIDER, CONFIG_CODES_ENABLED, CONFIG_EMAIL_ENABLED,
    FLOW_TTL_SECS, METHOD_EMAIL, METHOD_RECOVERY_CODES, RECOVERY_CODE_BATCH, RecoveryProvider,
    generate_secret, hash_secret, secret_matches,
};

/// Length of an emailed one-time recovery code.
const EMAIL_CODE_LEN: usize = 8;

/// Length of a pre-generated recovery code.
const RECOVERY_CODE_LEN: usize = 12;

/// Read a boolean site-config flag, defaulting when unset or unparseable.
async fn config_flag(pool: &PgPool, key: &str, default: bool) -> bool {
    crate::models::SiteConfig::get(pool, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

// ─── Emailed one-time code ───────────────────────────────────────────────────

/// The built-in emailed-code recovery path.
pub struct EmailRecoveryProvider {
    pool: PgPool,
    email: Option<std::sync::Arc<crate::services::email::EmailService>>,
    site_name: String,
}

impl EmailRecoveryProvider {
    /// Create the provider. `email` is `None` when `SMTP_HOST` is unconfigured,
    /// which makes the whole path inert rather than broken.
    pub fn new(
        pool: PgPool,
        email: Option<std::sync::Arc<crate::services::email::EmailService>>,
        site_name: String,
    ) -> Self {
        Self {
            pool,
            email,
            site_name,
        }
    }
}

#[async_trait]
impl RecoveryProvider for EmailRecoveryProvider {
    fn provider_name(&self) -> &'static str {
        BUILTIN_EMAIL_PROVIDER
    }

    async fn is_enabled(&self) -> bool {
        // Two gates, and both must pass: the admin switch, and the physical
        // ability to deliver. A path that cannot send is not a recovery path.
        self.email.is_some() && config_flag(&self.pool, CONFIG_EMAIL_ENABLED, true).await
    }

    async fn handle(&self, input: &RecoveryTapInput) -> Option<RecoveryTapResult> {
        match input {
            RecoveryTapInput::Describe { account, .. } => {
                Some(RecoveryTapResult::Methods {
                    methods: vec![RecoveryMethod {
                        method_id: METHOD_EMAIL.to_string(),
                        display_name: "Emailed recovery code".to_string(),
                        // Available only if the account actually has an address
                        // to send to.
                        available: account.email_present,
                    }],
                })
            }

            RecoveryTapInput::Initiate {
                flow_id,
                account,
                method_id,
            } => {
                if method_id != METHOD_EMAIL {
                    return None;
                }
                let Some(email_service) = self.email.as_ref() else {
                    return Some(RecoveryTapResult::Initiated {
                        status: "unavailable".to_string(),
                        challenge_hint: "Email recovery is not configured.".to_string(),
                        expires_in_secs: 0,
                    });
                };

                let Ok(Some(user)) =
                    crate::models::User::find_by_id(&self.pool, account.user_id).await
                else {
                    return Some(RecoveryTapResult::Initiated {
                        status: "unavailable".to_string(),
                        challenge_hint: "Email recovery is not available for this account."
                            .to_string(),
                        expires_in_secs: 0,
                    });
                };
                if user.mail.trim().is_empty() {
                    return Some(RecoveryTapResult::Initiated {
                        status: "unavailable".to_string(),
                        challenge_hint: "Email recovery is not available for this account."
                            .to_string(),
                        expires_in_secs: 0,
                    });
                }

                let code = generate_secret(EMAIL_CODE_LEN);
                let expires_at = Utc::now() + chrono::Duration::seconds(FLOW_TTL_SECS);

                // Bound to the FLOW, not merely to the account: a code minted for
                // one recovery attempt is not redeemable in another. The upsert
                // replaces any earlier challenge for this flow, so re-initiating
                // invalidates the previous code rather than leaving two live.
                let stored = sqlx::query(
                    r#"
                    INSERT INTO recovery_email_challenges (user_id, flow_id, code_hash, expires_at)
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (flow_id) DO UPDATE
                        SET code_hash = EXCLUDED.code_hash,
                            expires_at = EXCLUDED.expires_at,
                            used_at = NULL
                    "#,
                )
                .bind(account.user_id)
                .bind(flow_id)
                .bind(hash_secret(&code))
                .bind(expires_at)
                .execute(&self.pool)
                .await;

                if let Err(e) = stored {
                    tracing::error!(error = %e, "failed to store an email recovery challenge");
                    return Some(RecoveryTapResult::Initiated {
                        status: "unavailable".to_string(),
                        challenge_hint: "Could not start email recovery.".to_string(),
                        expires_in_secs: 0,
                    });
                }

                let body = format!(
                    "Someone asked to recover your account at {site}.\n\n\
                     Your recovery code is: {code}\n\n\
                     It expires in {minutes} minutes and can be used once.\n\n\
                     If this was not you, ignore this email and consider changing your \
                     password — someone knows or guessed your account name.\n",
                    site = self.site_name,
                    minutes = FLOW_TTL_SECS / 60,
                );
                if let Err(e) = email_service
                    .send(
                        &user.mail,
                        &format!("Account recovery code for {}", self.site_name),
                        &body,
                    )
                    .await
                {
                    tracing::error!(error = %e, "failed to send a recovery code email");
                    return Some(RecoveryTapResult::Initiated {
                        status: "unavailable".to_string(),
                        challenge_hint: "Could not send the recovery email.".to_string(),
                        expires_in_secs: 0,
                    });
                }

                Some(RecoveryTapResult::Initiated {
                    status: "initiated".to_string(),
                    // No secrets in the hint (the frozen schema says so), and no
                    // echo of the address, which would make this an enumeration
                    // oracle for anyone who got this far.
                    challenge_hint: "We sent a recovery code to the email address on file."
                        .to_string(),
                    expires_in_secs: FLOW_TTL_SECS as u64,
                })
            }

            RecoveryTapInput::Verify {
                flow_id,
                account,
                method_id,
                response,
            } => {
                if method_id != METHOD_EMAIL {
                    return None;
                }
                Some(RecoveryTapResult::Verdict {
                    verdict: self.verify_code(account.user_id, flow_id, response).await,
                })
            }
        }
    }
}

impl EmailRecoveryProvider {
    /// Check a submitted code and consume it.
    async fn verify_code(&self, user_id: Uuid, flow_id: &str, response: &str) -> Verdict {
        let row: Result<Option<(Uuid, String)>, _> = sqlx::query_as(
            r#"
            SELECT id, code_hash FROM recovery_email_challenges
            WHERE flow_id = $1 AND user_id = $2 AND used_at IS NULL AND expires_at > now()
            "#,
        )
        .bind(flow_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await;

        let Ok(Some((id, code_hash))) = row else {
            // No live challenge for this (flow, account). Reject rather than
            // abstain: an owner that knows the method was never initiated has a
            // real opinion, and abstaining would fall through to the fold's
            // fail-closed default anyway with less information in the audit.
            return Verdict::Rejected;
        };

        if !secret_matches(response, &code_hash) {
            return Verdict::Rejected;
        }

        // Consume it. A code that verified once must never verify again, so the
        // UPDATE is conditional on it still being unused: two concurrent
        // submissions cannot both win.
        let consumed = sqlx::query(
            "UPDATE recovery_email_challenges SET used_at = now() WHERE id = $1 AND used_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await;

        match consumed {
            Ok(result) if result.rows_affected() == 1 => Verdict::Verified,
            Ok(_) => Verdict::Rejected,
            Err(e) => {
                tracing::error!(error = %e, "failed to consume an email recovery challenge");
                // Fail closed: if we cannot prove the code was consumed, we must
                // not grant on it.
                Verdict::Rejected
            }
        }
    }
}

// ─── Pre-generated recovery codes ────────────────────────────────────────────

/// The built-in pre-generated-code recovery path.
pub struct RecoveryCodesProvider {
    pool: PgPool,
}

impl RecoveryCodesProvider {
    /// Create the provider.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// How many unused codes an account holds.
    pub async fn unused_count(pool: &PgPool, user_id: Uuid) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
    }

    /// Issue a fresh batch, invalidating any previous one.
    ///
    /// Returns the plaintext codes. They are shown once and are not recoverable
    /// afterwards — only their hashes are stored. Regenerating deletes the old
    /// batch so a user cannot end up unsure which of two printouts is live.
    pub async fn generate(pool: &PgPool, user_id: Uuid) -> anyhow::Result<Vec<String>> {
        let mut tx = pool.begin().await?;

        sqlx::query("DELETE FROM recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

        let mut codes = Vec::with_capacity(RECOVERY_CODE_BATCH);
        for _ in 0..RECOVERY_CODE_BATCH {
            let code = generate_secret(RECOVERY_CODE_LEN);
            sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES ($1, $2)")
                .bind(user_id)
                .bind(hash_secret(&code))
                .execute(&mut *tx)
                .await?;
            codes.push(code);
        }

        tx.commit().await?;
        Ok(codes)
    }
}

#[async_trait]
impl RecoveryProvider for RecoveryCodesProvider {
    fn provider_name(&self) -> &'static str {
        BUILTIN_CODES_PROVIDER
    }

    async fn is_enabled(&self) -> bool {
        config_flag(&self.pool, CONFIG_CODES_ENABLED, true).await
    }

    async fn handle(&self, input: &RecoveryTapInput) -> Option<RecoveryTapResult> {
        match input {
            RecoveryTapInput::Describe { account, .. } => {
                let available = Self::unused_count(&self.pool, account.user_id).await > 0;
                Some(RecoveryTapResult::Methods {
                    methods: vec![RecoveryMethod {
                        method_id: METHOD_RECOVERY_CODES.to_string(),
                        display_name: "Saved recovery code".to_string(),
                        available,
                    }],
                })
            }

            RecoveryTapInput::Initiate { method_id, .. } => {
                if method_id != METHOD_RECOVERY_CODES {
                    return None;
                }
                // Nothing to send: the user already holds the codes. Saying so
                // is the whole challenge.
                Some(RecoveryTapResult::Initiated {
                    status: "initiated".to_string(),
                    challenge_hint: "Enter one of the recovery codes you saved.".to_string(),
                    expires_in_secs: FLOW_TTL_SECS as u64,
                })
            }

            RecoveryTapInput::Verify {
                account,
                method_id,
                response,
                ..
            } => {
                if method_id != METHOD_RECOVERY_CODES {
                    return None;
                }
                Some(RecoveryTapResult::Verdict {
                    verdict: self.verify_code(account.user_id, response).await,
                })
            }
        }
    }
}

impl RecoveryCodesProvider {
    /// Check and consume one of the account's saved codes.
    async fn verify_code(&self, user_id: Uuid, response: &str) -> Verdict {
        let submitted_hash = hash_secret(response.trim());

        // Match on the hash in SQL rather than pulling every code out and
        // comparing in Rust: the hash is the lookup key, and this also means a
        // code belonging to another account cannot match here even if its
        // plaintext collided, because the row is scoped by user_id.
        //
        // The conditional UPDATE ... RETURNING is what makes redemption atomic:
        // two concurrent submissions of the same code cannot both succeed.
        let consumed: Result<Option<(Uuid,)>, _> = sqlx::query_as(
            r#"
            UPDATE recovery_codes
               SET used_at = now()
             WHERE id = (
                   SELECT id FROM recovery_codes
                    WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL
                    LIMIT 1
                   )
            RETURNING id
            "#,
        )
        .bind(user_id)
        .bind(&submitted_hash)
        .fetch_optional(&self.pool)
        .await;

        match consumed {
            Ok(Some(_)) => Verdict::Verified,
            Ok(None) => Verdict::Rejected,
            Err(e) => {
                tracing::error!(error = %e, "failed to redeem a recovery code");
                Verdict::Rejected
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn code_lengths_are_usable_and_not_guessable() {
        // 31-character alphabet: 8 chars is ~40 bits, 12 is ~59. Combined with
        // the recovery rate limit, both are far out of brute-force reach, and
        // both are short enough to type.
        const { assert!(EMAIL_CODE_LEN >= 8) };
        const { assert!(RECOVERY_CODE_LEN >= 10) };
        const { assert!(RECOVERY_CODE_BATCH >= 5) };
    }

    #[test]
    fn a_generated_batch_has_no_duplicates() {
        let codes: Vec<String> = (0..RECOVERY_CODE_BATCH)
            .map(|_| generate_secret(RECOVERY_CODE_LEN))
            .collect();
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
    }
}
