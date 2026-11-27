//! TOTP (RFC 6238) enrolment, verification, and single-use recovery codes
//! (B304).
//!
//! Uses the `totp-rs` crate (features `otpauth`, `gen_secret`, every other
//! default feature off) for code generation and checking, and `qrcodegen`
//! (no `image` dependency) to render the enrolment QR code as inline SVG.
//! Secrets are sealed at rest with [`crate::secret_box::SecretBox`]; without
//! `ENCRYPTION_KEY` configured, [`enrolment_available`] is `false` and every
//! enrolment entry point refuses with a message naming the setting.

use anyhow::{Context, Result, bail};
use rand::Rng;
use sqlx::PgPool;
use totp_rs::{Algorithm, Builder, Secret, Totp};
use uuid::Uuid;

use crate::models::user_totp::{UserRecoveryCode, UserTotp};
use crate::secret_box::{PURPOSE_RECOVERY_CODE, PURPOSE_TOTP_SECRET, SecretBox};
use crate::state::AppState;

/// RFC 6238 parameters: SHA-1, 6 digits, 30-second step, one step of skew
/// either side (so a code generated just before or after the server's clock
/// tick still verifies).
const DIGITS: u8 = 6;
const SKEW: u16 = 1;
const STEP_SECONDS: u64 = 30;

/// How many recovery codes an enrolment gets.
pub const RECOVERY_CODE_COUNT: usize = 10;

/// Length, in characters, of one recovery code (before display grouping).
const RECOVERY_CODE_LEN: usize = 10;

/// Unpadded RFC 4648 base32 alphabet, used for recovery codes (matching the
/// TOTP secret's own encoding, so both are "read me a block of letters and
/// digits" in the same alphabet).
const BASE32_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Whether TOTP enrolment is available on this site: `ENCRYPTION_KEY` must be
/// configured, since the secret has nowhere safe to live without it. A
/// passkey still satisfies a role's second-factor requirement with no key
/// configured.
pub fn enrolment_available(state: &AppState) -> bool {
    state.secret_box().is_some()
}

/// The message every enrolment entry point shows when [`enrolment_available`]
/// is `false`.
pub const UNAVAILABLE_MESSAGE: &str = "An authenticator app cannot be set up on this site: the site owner has not \
     configured ENCRYPTION_KEY. A passkey can still be used as a second factor.";

/// Build a [`Totp`] from raw secret bytes, with the account's identity for
/// the otpauth URL (`to_url`) when rendering a QR code, or empty strings for
/// a check-only build (identity plays no part in `check`/`generate`).
fn build_totp(secret_bytes: &[u8], account_name: &str, issuer: &str) -> Result<Totp> {
    let mut builder = Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(DIGITS)
        .with_skew(SKEW)
        .with_step_duration(STEP_SECONDS)
        .with_secret(secret_bytes.to_vec());
    if !account_name.is_empty() {
        builder = builder.with_account_name(account_name.to_string());
    }
    if !issuer.is_empty() {
        builder = builder.with_issuer(Some(issuer.to_string()));
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build TOTP instance: {e}"))
}

/// What the setup page shows once: the manual entry secret and the QR code.
pub struct NewEnrolment {
    /// Base32, grouped for reading by eye or by voice.
    pub manual_secret: String,
    /// Inline `<svg>...</svg>`, no script, no external resource.
    pub qr_svg: String,
}

/// Start (or restart) enrolment for `user_id`: a fresh secret, stored sealed
/// and unconfirmed, replacing any prior unconfirmed attempt. Fails with
/// [`UNAVAILABLE_MESSAGE`] semantics if [`enrolment_available`] is false (the
/// caller should check that first; this still refuses defensively).
pub async fn start_enrolment(
    state: &AppState,
    user_id: Uuid,
    account_name: &str,
    issuer: &str,
) -> Result<NewEnrolment> {
    let Some(secret_box) = state.secret_box() else {
        bail!(UNAVAILABLE_MESSAGE);
    };

    let secret = Secret::generate();
    let rendered = render_pending_setup(secret.as_bytes(), account_name, issuer)?;

    let sealed = secret_box.seal(PURPOSE_TOTP_SECRET, user_id.as_bytes(), secret.as_bytes());
    UserTotp::start_enrolment(state.db(), user_id, &sealed).await?;

    Ok(rendered)
}

/// Render the manual secret and QR code for a secret already on record (the
/// setup page's `GET`, re-derived from the stored sealed secret rather than
/// generating a new one each time it is viewed).
pub fn render_pending_setup(
    secret_bytes: &[u8],
    account_name: &str,
    issuer: &str,
) -> Result<NewEnrolment> {
    let totp = build_totp(secret_bytes, account_name, issuer)
        .context("failed to build a TOTP instance for the pending secret")?;
    let otpauth_url = totp
        .to_url()
        .context("failed to build the otpauth:// URL")?;
    let qr_svg = render_qr_svg(&otpauth_url)?;
    Ok(NewEnrolment {
        manual_secret: Secret::new(secret_bytes.to_vec().into_boxed_slice()).to_base32(),
        qr_svg,
    })
}

/// Check `code` against `user_id`'s **unconfirmed** enrolment; on success,
/// confirm it and issue recovery codes (shown once, by the caller, and never
/// stored in the clear). `Ok(None)` means the code did not verify, or there
/// is no pending enrolment; nothing is written in that case.
pub async fn confirm_enrolment(
    state: &AppState,
    user_id: Uuid,
    code: &str,
) -> Result<Option<Vec<String>>> {
    let Some(secret_box) = state.secret_box() else {
        bail!(UNAVAILABLE_MESSAGE);
    };
    let Some(row) = UserTotp::find(state.db(), user_id).await? else {
        return Ok(None);
    };
    if row.confirmed_at.is_some() {
        // Already confirmed: confirm is not re-confirm. The route guards
        // against this, but a racing second request must not reset
        // last_used_step on a working enrolment.
        return Ok(None);
    }

    let secret_bytes = secret_box
        .open(PURPOSE_TOTP_SECRET, user_id.as_bytes(), &row.secret)
        .context("failed to open the pending TOTP secret")?;
    let totp = build_totp(&secret_bytes, "", "")?;
    let now = current_unix_time();
    let Some(step) = totp.check(code.trim(), now) else {
        return Ok(None);
    };

    let (display_codes, hashes) = generate_recovery_codes(secret_box);

    let mut tx = state
        .db()
        .begin()
        .await
        .context("failed to start the enrolment-confirm transaction")?;
    let confirmed = sqlx::query(
        "UPDATE user_totp SET confirmed_at = NOW(), last_used_step = $2 \
         WHERE user_id = $1 AND confirmed_at IS NULL",
    )
    .bind(user_id)
    .bind(step as i64)
    .execute(&mut *tx)
    .await
    .context("failed to confirm the TOTP enrolment")?;
    if confirmed.rows_affected() == 0 {
        // Lost a race with another confirm of the same enrolment.
        tx.rollback().await.ok();
        return Ok(None);
    }
    UserRecoveryCode::replace_all(&mut tx, user_id, &hashes).await?;
    tx.commit()
        .await
        .context("failed to commit the enrolment-confirm transaction")?;

    Ok(Some(display_codes))
}

/// Check `code` against `user_id`'s confirmed enrolment, advancing
/// `last_used_step` on success so the same code can never verify twice.
/// `Ok(false)` for a wrong code, no enrolment, or no `ENCRYPTION_KEY`.
pub async fn verify_code(state: &AppState, user_id: Uuid, code: &str) -> Result<bool> {
    let Some(secret_box) = state.secret_box() else {
        return Ok(false);
    };
    let Some(row) = UserTotp::confirmed(state.db(), user_id).await? else {
        return Ok(false);
    };
    let secret_bytes = secret_box
        .open(PURPOSE_TOTP_SECRET, user_id.as_bytes(), &row.secret)
        .context("failed to open a confirmed TOTP secret")?;
    let totp = build_totp(&secret_bytes, "", "")?;
    let now = current_unix_time();
    let Some(step) = totp.check(code.trim(), now) else {
        return Ok(false);
    };
    UserTotp::accept_step(state.db(), user_id, step as i64)
        .await
        .context("failed to record the accepted TOTP step")
}

/// Verify a recovery code and spend it on success. `Ok(false)` for a wrong or
/// already-used code. Scans every unused code (there is no index on a hash
/// that must not be reversible) and compares in constant time.
pub async fn verify_recovery_code(state: &AppState, user_id: Uuid, code: &str) -> Result<bool> {
    use subtle::ConstantTimeEq;

    let Some(secret_box) = state.secret_box() else {
        return Ok(false);
    };
    let normalized = normalize_recovery_code(code);
    if normalized.is_empty() {
        return Ok(false);
    }
    let candidate = secret_box.hmac(PURPOSE_RECOVERY_CODE, normalized.as_bytes());

    for row in UserRecoveryCode::unused_for_user(state.db(), user_id).await? {
        if candidate.len() == row.code_hash.len()
            && bool::from(candidate.as_slice().ct_eq(&row.code_hash))
        {
            return UserRecoveryCode::mark_used(state.db(), row.id).await;
        }
    }
    Ok(false)
}

/// Remove `user_id`'s TOTP enrolment and recovery codes entirely. Returns
/// whether there was anything to remove.
pub async fn remove_enrolment(pool: &PgPool, user_id: Uuid) -> Result<bool> {
    // `ON DELETE CASCADE` on `user_recovery_codes.user_id` already removes
    // the codes; the explicit delete keeps the intent visible and does not
    // depend on remembering the cascade exists.
    UserRecoveryCode::delete_all_for_user(pool, user_id).await?;
    UserTotp::delete(pool, user_id).await
}

/// How many unused recovery codes `user_id` has left, for the status page.
pub async fn remaining_recovery_codes(pool: &PgPool, user_id: Uuid) -> Result<i64> {
    UserRecoveryCode::unused_count(pool, user_id).await
}

/// Normalize a submitted recovery code for comparison: upper-cased, with
/// whitespace and display hyphens stripped, so `abcde-fghij`, `ABCDE-FGHIJ`
/// and `ABCDEFGHIJ` all hash the same.
fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_uppercase)
        .collect()
}

/// Group a normalized recovery code for display: `ABCDE-FGHIJ`.
fn format_recovery_code_for_display(code: &str) -> String {
    let half = RECOVERY_CODE_LEN / 2;
    if code.len() != RECOVERY_CODE_LEN {
        return code.to_string();
    }
    format!("{}-{}", &code[..half], &code[half..])
}

/// Generate [`RECOVERY_CODE_COUNT`] fresh recovery codes: the plain,
/// display-grouped strings to show once, and their HMAC digests to store.
fn generate_recovery_codes(secret_box: &SecretBox) -> (Vec<String>, Vec<Vec<u8>>) {
    let mut rng = rand::thread_rng();
    let mut display = Vec::with_capacity(RECOVERY_CODE_COUNT);
    let mut hashes = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _ in 0..RECOVERY_CODE_COUNT {
        let code: String = (0..RECOVERY_CODE_LEN)
            .map(|_| BASE32_ALPHABET[rng.gen_range(0..BASE32_ALPHABET.len())] as char)
            .collect();
        let hash = secret_box.hmac(
            PURPOSE_RECOVERY_CODE,
            normalize_recovery_code(&code).as_bytes(),
        );
        display.push(format_recovery_code_for_display(&code));
        hashes.push(hash);
    }
    (display, hashes)
}

/// Render `data` (the otpauth:// URL) as an inline SVG QR code: no script, no
/// external resource, no `image` crate — just a path of 1x1 modules.
fn render_qr_svg(data: &str) -> Result<String> {
    use qrcodegen::{QrCode, QrCodeEcc};
    use std::fmt::Write;

    let qr = QrCode::encode_text(data, QrCodeEcc::Medium)
        .map_err(|_| anyhow::anyhow!("TOTP enrolment URL is too long to encode as a QR code"))?;

    const BORDER: i32 = 4;
    let size = qr.size();
    let dim = size + BORDER * 2;

    let mut svg = String::new();
    write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {dim} {dim}\" \
         role=\"img\" aria-label=\"Scan with your authenticator app\">"
    )?;
    svg.push_str("<rect width=\"100%\" height=\"100%\" fill=\"#ffffff\"/>");
    svg.push_str("<path fill=\"#000000\" d=\"");
    for y in 0..size {
        for x in 0..size {
            if qr.get_module(x, y) {
                write!(svg, "M{},{}h1v1h-1z", x + BORDER, y + BORDER)?;
            }
        }
    }
    svg.push_str("\"/></svg>");
    Ok(svg)
}

/// Current Unix time, as `Totp::check` wants it.
fn current_unix_time() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_totp_checks_its_own_current_code() {
        let secret = Secret::generate();
        let totp = build_totp(secret.as_bytes(), "user@example.com", "Trovato").unwrap();
        let now = current_unix_time();
        let code = totp.generate(now).to_string();
        assert!(totp.check(&code, now).is_some());
    }

    #[test]
    fn to_url_names_the_issuer_and_account() {
        let secret = Secret::generate();
        let totp = build_totp(secret.as_bytes(), "jane", "Trovato").unwrap();
        let url = totp.to_url().unwrap();
        assert!(url.starts_with("otpauth://totp/"));
        assert!(url.contains("Trovato"));
        assert!(url.contains("jane"));
    }

    #[test]
    fn qr_svg_has_no_script_or_style_and_contains_a_path() {
        let svg =
            render_qr_svg("otpauth://totp/Trovato:jane?secret=ABCDEFGH&issuer=Trovato").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(!svg.contains("<script"));
        assert!(!svg.contains("style="));
        assert!(svg.contains("<path"));
    }

    #[test]
    fn recovery_code_normalization_ignores_case_hyphens_and_whitespace() {
        assert_eq!(normalize_recovery_code("abcde-fghij"), "ABCDEFGHIJ");
        assert_eq!(normalize_recovery_code(" ABCDE-FGHIJ \n"), "ABCDEFGHIJ");
        assert_eq!(normalize_recovery_code("ABCDEFGHIJ"), "ABCDEFGHIJ");
    }

    #[test]
    fn generated_recovery_codes_are_distinct_and_hash_deterministically() {
        let secret_box = SecretBox::new([9u8; 32]);
        let (display, hashes) = generate_recovery_codes(&secret_box);
        assert_eq!(display.len(), RECOVERY_CODE_COUNT);
        assert_eq!(hashes.len(), RECOVERY_CODE_COUNT);
        // Vanishingly unlikely to collide; a real collision would be a bug.
        let unique: std::collections::HashSet<_> = display.iter().collect();
        assert_eq!(unique.len(), RECOVERY_CODE_COUNT);

        // Re-hashing the displayed code (after stripping the display hyphen)
        // reproduces the stored hash.
        for (code, hash) in display.iter().zip(hashes.iter()) {
            let recomputed = secret_box.hmac(
                PURPOSE_RECOVERY_CODE,
                normalize_recovery_code(code).as_bytes(),
            );
            assert_eq!(&recomputed, hash);
        }
    }

    #[test]
    fn display_grouping_splits_in_half() {
        assert_eq!(
            format_recovery_code_for_display("ABCDEFGHIJ"),
            "ABCDE-FGHIJ"
        );
    }
}
