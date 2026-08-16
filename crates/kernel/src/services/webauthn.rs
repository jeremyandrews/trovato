//! WebAuthn relying-party configuration and ceremony state (FR-7a, D-34).
//!
//! WebAuthn is **kernel-native**, not a plugin (AD-4 / Law 4): a WASM plugin
//! cannot hold per-request ceremony state, cannot sit on the login critical path
//! before any plugin loads, and cannot speak the browser WebAuthn API. So the
//! `Webauthn` instance is a plain non-optional field on `AppState`, built from
//! the site origin at startup.
//!
//! # Where challenge state lives
//!
//! In the **session**, not a new store (design §1). A ceremony is two round
//! trips (options → browser → response) and the intermediate state is
//! per-session, short-lived, and single-use — exactly what a session is for. The
//! session is already Redis-backed, so this reuses the existing infrastructure
//! with no new challenge table and no new Redis client, and it binds the
//! challenge to the browser that asked for it for free.
//!
//! # PRF non-preclusion (FR-7a NOTE, AC-4)
//!
//! Nothing here disables or forecloses a future PRF extension. We use
//! `webauthn-rs`'s stock passkey builders, which request `cred_protect` / `uvm` /
//! `cred_props` and leave the extension set open; a post-1.0 Cairn key hierarchy
//! can request PRF at creation time without invalidating any credential
//! registered now. This is a "don't close the door" constraint, asserted by the
//! `registration_options_do_not_preclude_prf` unit test, not a 1.0 feature.

use anyhow::{Context, Result};
use url::Url;
use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration};
use webauthn_rs::{Webauthn, WebauthnBuilder};

/// Session key holding the in-flight passkey **registration** ceremony.
pub const SESSION_WEBAUTHN_REG: &str = "webauthn_reg";

/// Session key holding the in-flight passkey **authentication** ceremony.
pub const SESSION_WEBAUTHN_AUTH: &str = "webauthn_auth";

/// How long an in-flight ceremony stays usable, in seconds.
///
/// The browser-side timeout `webauthn-rs` puts in the challenge options is
/// advisory; this is the kernel's own bound, checked when `/finish` reloads the
/// state. Without it a ceremony would live as long as the session (24h), which
/// is far longer than a challenge should be replayable.
pub const CEREMONY_TTL_SECS: i64 = 300;

/// An in-flight registration ceremony, plus the kernel's own expiry stamp.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingRegistration {
    /// The `webauthn-rs` ceremony state (requires `danger-allow-state-serialisation`).
    pub state: PasskeyRegistration,
    /// Unix seconds when this ceremony stops being accepted.
    pub expires_at: i64,
    /// The account the ceremony was started for. Re-checked at `/finish` so a
    /// ceremony started as one user can never be completed as another.
    pub user_id: uuid::Uuid,
}

/// An in-flight authentication ceremony, plus the kernel's own expiry stamp.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingAuthentication {
    /// The `webauthn-rs` ceremony state.
    pub state: PasskeyAuthentication,
    /// Unix seconds when this ceremony stops being accepted.
    pub expires_at: i64,
    /// The account whose credentials were offered in the allow-list. The
    /// assertion is only accepted against this account.
    pub user_id: uuid::Uuid,
}

/// Whether a stored ceremony is still inside the kernel's TTL.
pub fn ceremony_is_live(expires_at: i64, now: i64) -> bool {
    now < expires_at
}

/// The expiry stamp for a ceremony starting now.
pub fn ceremony_expiry(now: i64) -> i64 {
    now + CEREMONY_TTL_SECS
}

/// Derive the relying-party id (the effective domain) from the site URL.
///
/// The RP ID is the registrable domain the credential is scoped to. WebAuthn
/// binds credentials to it, so it must be the bare host — no scheme, no port,
/// no path. A credential registered against the wrong RP ID is simply unusable,
/// which is why this is derived from one source (`SITE_URL`) rather than
/// configured separately and allowed to drift from the origin.
pub fn rp_id_from_site_url(site_url: &str) -> Result<String> {
    let url =
        Url::parse(site_url).with_context(|| format!("SITE_URL is not a valid URL: {site_url}"))?;
    url.host_str()
        .map(str::to_string)
        .with_context(|| format!("SITE_URL has no host to use as the WebAuthn RP ID: {site_url}"))
}

/// Build the relying-party instance from the site URL.
///
/// `rp_name` is what the browser shows the user in the platform passkey prompt.
pub fn build_webauthn(site_url: &str, rp_name: &str) -> Result<Webauthn> {
    let rp_id = rp_id_from_site_url(site_url)?;
    let rp_origin =
        Url::parse(site_url).with_context(|| format!("SITE_URL is not a valid URL: {site_url}"))?;

    WebauthnBuilder::new(&rp_id, &rp_origin)
        .context("failed to build the WebAuthn relying party")?
        .rp_name(rp_name)
        .build()
        .context("failed to finalize the WebAuthn relying party")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn rp_id_is_the_bare_host() {
        assert_eq!(
            rp_id_from_site_url("https://example.com").unwrap(),
            "example.com"
        );
        // A port must not leak into the RP ID — credentials scoped to
        // "localhost:8080" would be unusable.
        assert_eq!(
            rp_id_from_site_url("http://localhost:8080").unwrap(),
            "localhost"
        );
        assert_eq!(
            rp_id_from_site_url("https://cms.example.com/admin").unwrap(),
            "cms.example.com"
        );
    }

    #[test]
    fn rp_id_rejects_a_hostless_url() {
        assert!(rp_id_from_site_url("not-a-url").is_err());
        assert!(rp_id_from_site_url("file:///tmp/x").is_err());
    }

    #[test]
    fn relying_party_builds_from_a_plain_site_url() {
        assert!(build_webauthn("http://localhost:8080", "Trovato").is_ok());
        assert!(build_webauthn("https://example.com", "Trovato").is_ok());
    }

    #[test]
    fn ceremony_ttl_is_bounded_and_short() {
        // Long enough for a human to touch a key, far shorter than a session.
        const { assert!(CEREMONY_TTL_SECS > 0) };
        const { assert!(CEREMONY_TTL_SECS <= 600) };
    }

    #[test]
    fn ceremony_expires() {
        let now = 1_000_000;
        let exp = ceremony_expiry(now);
        assert!(ceremony_is_live(exp, now));
        assert!(ceremony_is_live(exp, exp - 1));
        // Exactly at the stamp is already dead: the comparison is strict.
        assert!(!ceremony_is_live(exp, exp));
        assert!(!ceremony_is_live(exp, exp + 1));
    }

    #[test]
    fn registration_options_do_not_preclude_prf() {
        // FR-7a AC-4 (PRF non-preclusion, Cairn post-1.0). We must never emit
        // creation options that foreclose a later PRF extension request. The
        // check that matters is that we do not disable extensions wholesale:
        // the stock passkey builder leaves the extension set open, so a future
        // PRF request is additive rather than a re-registration.
        let webauthn = build_webauthn("https://example.com", "Trovato").unwrap();
        let (ccr, _state) = webauthn
            .start_passkey_registration(uuid::Uuid::now_v7(), "claire", "Claire", None)
            .unwrap();
        let json = serde_json::to_value(&ccr).unwrap();
        let extensions = &json["publicKey"]["extensions"];
        // Extensions are present and open — not `false`, not absent-because-disabled.
        assert!(
            extensions.is_object(),
            "creation options must carry an open extension map so PRF stays reachable: {json}"
        );
        // And nothing pins the credential to a shape a PRF request would break.
        assert!(
            json["publicKey"]["challenge"].is_string(),
            "expected a well-formed creation challenge: {json}"
        );
    }
}
