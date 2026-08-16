//! WebAuthn/passkey credential model (FR-7a, design §2.1, D-35).
//!
//! Storage follows D-35: the `webauthn-rs` serialized [`Passkey`] blob is
//! authoritative for everything the library needs to verify an assertion, and
//! the sibling columns are a denormalized lookup/display convenience refreshed
//! from the blob after each successful authentication. Re-deriving the COSE key
//! or attestation into hand-rolled columns would couple us to the library's
//! internal representation; reading a few flat fields out of it does not.
//!
//! Credentials sit **parallel** to `users.pass` (D-33). Nothing here removes a
//! password, and nothing here is required for a password login to work.

use anyhow::{Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;
use webauthn_rs::prelude::{AuthenticationResult, Passkey};

/// A registered passkey.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WebauthnCredential {
    /// Row identifier (what management routes address a credential by).
    pub id: Uuid,
    /// Owning account.
    pub user_id: Uuid,
    /// Base64url (no pad) of the raw credential id the authenticator issued.
    pub credential_id: String,
    /// The authoritative serialized `webauthn-rs` [`Passkey`].
    pub passkey_json: serde_json::Value,
    /// Denormalized signature counter. `0` means the authenticator does not
    /// support counters (common for platform passkeys), not "never used".
    pub sign_count: i64,
    /// Transport hints reported at registration.
    pub transports: Vec<String>,
    /// Authenticator model id, when the attestation carried one.
    pub aaguid: Option<Uuid>,
    /// The credential indicated it may be backed up / synced across devices.
    pub backup_eligible: bool,
    /// The credential is currently backed up / synced.
    pub backup_state: bool,
    /// User-assigned label.
    pub device_name: Option<String>,
    /// D-37: when an authentication presented a regressed counter. The
    /// credential is flagged, never auto-revoked.
    pub flagged_at: Option<DateTime<Utc>>,
    /// Why the credential was flagged.
    pub flag_reason: Option<String>,
    /// Registration time.
    pub created_at: DateTime<Utc>,
    /// Last successful authentication.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Encode a raw credential id the way the `credential_id` column stores it.
pub fn encode_credential_id(raw: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// Flat, denormalized properties read out of a [`Passkey`] for the sibling
/// columns (D-35). This is the only place that reaches into the library's
/// credential internals, so the coupling has exactly one site.
struct PasskeyFacts {
    credential_id: String,
    sign_count: i64,
    transports: Vec<String>,
    aaguid: Option<Uuid>,
    backup_eligible: bool,
    backup_state: bool,
}

impl PasskeyFacts {
    /// Extract the denormalized columns from an authoritative `Passkey`.
    fn extract(passkey: &Passkey) -> Self {
        let credential_id = encode_credential_id(passkey.cred_id().as_ref());

        // `danger-credential-internals` is what makes the flat fields readable.
        // This is a read-only projection for display and the D-37 check; the
        // blob remains authoritative and verification stays inside the library.
        let cred: webauthn_rs::prelude::Credential = passkey.clone().into();

        let transports = cred
            .transports
            .as_ref()
            .map(|ts| {
                ts.iter()
                    .map(|t| {
                        // The transport enum serializes to exactly the
                        // usb|nfc|ble|internal|hybrid strings the column
                        // documents; go through serde rather than matching so a
                        // library-side addition cannot silently become "".
                        serde_json::to_value(t)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_else(|| "unknown".to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();

        let aaguid = match &cred.attestation.metadata {
            webauthn_rs::prelude::AttestationMetadata::Packed { aaguid } => Some(*aaguid),
            webauthn_rs::prelude::AttestationMetadata::Tpm { aaguid, .. } => Some(*aaguid),
            // Everything else (None, AndroidKey, AndroidSafetyNet, Apple, …)
            // carries no AAGUID; the column is nullable precisely for this.
            _ => None,
        };

        Self {
            credential_id,
            sign_count: i64::from(cred.counter),
            transports,
            aaguid,
            backup_eligible: cred.backup_eligible,
            backup_state: cred.backup_state,
        }
    }
}

/// Whether a presented signature counter is a regression against the stored one
/// (D-37).
///
/// The rule, and the caveat that stops it bricking common passkeys:
///
/// - If **both** counters are `0`, the authenticator does not support counters
///   (the overwhelmingly common platform-passkey case). There is nothing to
///   compare, so the check is **disabled** for that credential and this returns
///   `false`.
/// - Otherwise the counter must **strictly increase**. A presented counter at or
///   below the stored one signals a cloned authenticator, and this returns
///   `true`.
///
/// `webauthn-rs` enforces the same rule internally and returns
/// `WebauthnError::CredentialPossibleCompromise`; this function is the kernel's
/// own statement of the invariant, used to flag the right credential and to make
/// the semantics unit-testable without a real authenticator.
pub fn counter_regressed(stored: i64, presented: i64) -> bool {
    if stored == 0 && presented == 0 {
        // Counter unsupported by this authenticator — exempt.
        return false;
    }
    presented <= stored
}

impl WebauthnCredential {
    /// Persist a freshly registered passkey.
    ///
    /// The unique constraint on `credential_id` is the enforcement of "a
    /// credential belongs to exactly one account": re-registering an
    /// authenticator that is already known fails here rather than silently
    /// rebinding it.
    pub async fn create(
        pool: &PgPool,
        user_id: Uuid,
        passkey: &Passkey,
        device_name: Option<&str>,
    ) -> Result<Self> {
        let facts = PasskeyFacts::extract(passkey);
        let passkey_json =
            serde_json::to_value(passkey).context("failed to serialize passkey for storage")?;

        sqlx::query_as::<_, WebauthnCredential>(
            r#"
            INSERT INTO webauthn_credentials
                (user_id, credential_id, passkey_json, sign_count, transports,
                 aaguid, backup_eligible, backup_state, device_name)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(&facts.credential_id)
        .bind(&passkey_json)
        .bind(facts.sign_count)
        .bind(&facts.transports)
        .bind(facts.aaguid)
        .bind(facts.backup_eligible)
        .bind(facts.backup_state)
        .bind(device_name)
        .fetch_one(pool)
        .await
        .context("failed to store webauthn credential")
    }

    /// All credentials for an account, newest first.
    pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<Self>> {
        sqlx::query_as::<_, WebauthnCredential>(
            "SELECT * FROM webauthn_credentials WHERE user_id = $1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .context("failed to list webauthn credentials")
    }

    /// How many credentials an account has. Used by the ≥1-path invariant (D-33)
    /// without materializing the rows.
    pub async fn count_for_user(pool: &PgPool, user_id: Uuid) -> Result<i64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM webauthn_credentials WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(pool)
                .await
                .context("failed to count webauthn credentials")?;
        Ok(count)
    }

    /// Look one up by its row id, scoped to an owner so a handler can never
    /// address another account's credential by guessing an id.
    pub async fn find_owned(pool: &PgPool, id: Uuid, user_id: Uuid) -> Result<Option<Self>> {
        sqlx::query_as::<_, WebauthnCredential>(
            "SELECT * FROM webauthn_credentials WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .context("failed to load webauthn credential")
    }

    /// Look one up by the base64url credential id the authenticator presented.
    pub async fn find_by_credential_id(pool: &PgPool, credential_id: &str) -> Result<Option<Self>> {
        sqlx::query_as::<_, WebauthnCredential>(
            "SELECT * FROM webauthn_credentials WHERE credential_id = $1",
        )
        .bind(credential_id)
        .fetch_optional(pool)
        .await
        .context("failed to load webauthn credential by credential id")
    }

    /// Rename a credential, scoped to its owner.
    ///
    /// Returns `false` if no such credential belongs to the user.
    pub async fn rename(pool: &PgPool, id: Uuid, user_id: Uuid, name: &str) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE webauthn_credentials SET device_name = $1 WHERE id = $2 AND user_id = $3",
        )
        .bind(name)
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .context("failed to rename webauthn credential")?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete a credential, scoped to its owner.
    ///
    /// This does **not** enforce the ≥1-active-recovery-path invariant; that
    /// gate lives in the credential-management route (Story 4.3) where the
    /// account's whole recovery posture is in view.
    pub async fn delete_owned(pool: &PgPool, id: Uuid, user_id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM webauthn_credentials WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(pool)
            .await
            .context("failed to delete webauthn credential")?;
        Ok(result.rows_affected() > 0)
    }

    /// Deserialize the authoritative blob back into a `Passkey`.
    pub fn passkey(&self) -> Result<Passkey> {
        serde_json::from_value(self.passkey_json.clone())
            .context("stored passkey blob is not a valid Passkey")
    }

    /// Record a successful authentication: refresh the authoritative blob from
    /// the assertion result and re-denormalize the sibling columns.
    ///
    /// `Passkey::update_credential` is what applies the counter and backup-state
    /// changes to the blob; we then write the blob and the projection together
    /// so they cannot drift.
    pub async fn record_authentication(
        &self,
        pool: &PgPool,
        auth_result: &AuthenticationResult,
    ) -> Result<()> {
        let mut passkey = self.passkey()?;
        passkey.update_credential(auth_result);

        let facts = PasskeyFacts::extract(&passkey);
        let passkey_json =
            serde_json::to_value(&passkey).context("failed to serialize updated passkey")?;

        sqlx::query(
            r#"
            UPDATE webauthn_credentials
               SET passkey_json = $1,
                   sign_count = $2,
                   backup_eligible = $3,
                   backup_state = $4,
                   last_used_at = now()
             WHERE id = $5
            "#,
        )
        .bind(&passkey_json)
        .bind(facts.sign_count)
        .bind(facts.backup_eligible)
        .bind(facts.backup_state)
        .bind(self.id)
        .execute(pool)
        .await
        .context("failed to update webauthn credential after authentication")?;

        Ok(())
    }

    /// D-37: flag a credential whose counter regressed.
    ///
    /// Flag, do **not** revoke. Auto-revoking on a false positive is a
    /// self-inflicted lockout, and counter regressions have benign causes
    /// (authenticator restore, buggy firmware) as well as the malicious one.
    /// The authentication that tripped this was already rejected; the flag is
    /// how the user and an admin find out.
    pub async fn flag(&self, pool: &PgPool, reason: &str) -> Result<()> {
        sqlx::query(
            "UPDATE webauthn_credentials SET flagged_at = now(), flag_reason = $1 WHERE id = $2",
        )
        .bind(reason)
        .bind(self.id)
        .execute(pool)
        .await
        .context("failed to flag webauthn credential")?;
        Ok(())
    }

    /// A display label: the user-assigned name, else a stable fallback.
    pub fn display_name(&self) -> String {
        self.device_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Unnamed passkey".to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn counter_zero_on_both_sides_is_exempt_not_a_regression() {
        // The platform-passkey case. If this returned true, every sync'd passkey
        // would be rejected on its second use.
        assert!(!counter_regressed(0, 0));
    }

    #[test]
    fn strictly_increasing_counter_is_accepted() {
        assert!(!counter_regressed(1, 2));
        assert!(!counter_regressed(0, 1));
        assert!(!counter_regressed(41, 42));
    }

    #[test]
    fn equal_counter_is_a_regression_when_counters_are_supported() {
        // A replayed assertion presents the same counter it did the first time.
        assert!(counter_regressed(7, 7));
    }

    #[test]
    fn decreasing_counter_is_a_regression() {
        assert!(counter_regressed(9, 3));
    }

    #[test]
    fn presented_zero_against_a_nonzero_stored_counter_is_a_regression() {
        // Once an authenticator has proven it counts, a 0 is a regression, not a
        // sudden loss of counter support.
        assert!(counter_regressed(5, 0));
    }

    #[test]
    fn credential_id_encoding_is_url_safe_and_unpadded() {
        // Raw bytes chosen to force both '+'/'/' substitutions and padding.
        let raw = [0xfb_u8, 0xff, 0xbf, 0x00];
        let encoded = encode_credential_id(&raw);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }
}
