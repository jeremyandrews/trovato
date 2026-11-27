//! Encrypt a byte string at rest with AES-256-GCM, keyed off `ENCRYPTION_KEY`.
//!
//! One root key (`ENCRYPTION_KEY`, 32 bytes), one purpose label per caller,
//! HKDF-SHA256 between them: every caller gets its own derived key from the
//! same root, so a caller never has to be trusted with another's key and the
//! root is never used to encrypt anything directly. The nonce is a fresh
//! random 96 bits per call, stored alongside the ciphertext (nonce prepended),
//! because AES-GCM is broken by nonce reuse under a fixed key.
//!
//! The only caller today is TOTP secret storage (`services::totp`), with
//! purpose label [`PURPOSE_TOTP_SECRET`]. A later caller adds its own label
//! and gets a key that cannot decrypt anything sealed under this one.

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, bail};
use hkdf::Hkdf;
use sha2::Sha256;

/// The purpose label TOTP secret storage derives its key under.
pub const PURPOSE_TOTP_SECRET: &str = "trovato.totp.secret.v1";

/// The purpose label recovery-code hashing derives its key under.
pub const PURPOSE_RECOVERY_CODE: &str = "trovato.totp.recovery_code.v1";

/// A root key, derived into one key per purpose with HKDF-SHA256.
///
/// Built once from `ENCRYPTION_KEY` and held by [`crate::state::AppState`];
/// `None` when the site has not set `ENCRYPTION_KEY`, which is what makes TOTP
/// enrolment unavailable (`services::totp::enrolment_available`).
#[derive(Clone)]
pub struct SecretBox {
    root: [u8; 32],
}

impl SecretBox {
    /// Build from the 32-byte root key `Config::encryption_key` resolved.
    pub fn new(root: [u8; 32]) -> Self {
        Self { root }
    }

    /// Derive the AES-256-GCM key for one purpose label.
    fn derive_key(&self, purpose: &str) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(None, &self.root);
        let mut key = [0u8; 32];
        // The only failure mode is an output longer than HKDF-SHA256 can
        // produce (255 * 32 bytes); 32 is nowhere near that.
        hk.expand(purpose.as_bytes(), &mut key)
            .expect("32-byte HKDF-SHA256 output never fails");
        key
    }

    /// Seal `plaintext` under `purpose`, authenticating `aad` (associated
    /// data the caller must present unchanged to decrypt, e.g. the owning
    /// user's id — binds the ciphertext to its row so one row's secret
    /// cannot be copied into another's).
    ///
    /// Returns `nonce || ciphertext`, the one blob a caller stores.
    pub fn seal(&self, purpose: &str, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let key = self.derive_key(purpose);
        let cipher = Aes256Gcm::new_from_slice(&key).expect("32-byte key is always valid");
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let payload = aes_gcm::aead::Payload {
            msg: plaintext,
            aad,
        };
        // The key is freshly derived and used once; the only way `encrypt`
        // fails is a plaintext over AES-GCM's ~64 GiB limit, which a TOTP
        // secret or recovery code never approaches.
        let ciphertext = cipher
            .encrypt(&nonce, payload)
            .expect("AES-256-GCM encryption of a short plaintext never fails");
        let mut out = Vec::with_capacity(nonce.len() + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        out
    }

    /// HMAC-SHA256 of `message` under the key derived for `purpose`.
    ///
    /// Used where a value must be looked up by a deterministic digest rather
    /// than opened (recovery codes: a candidate is hashed and compared
    /// against every stored hash, never decrypted).
    pub fn hmac(&self, purpose: &str, message: &[u8]) -> Vec<u8> {
        use hmac::Mac;
        let key = self.derive_key(purpose);
        // Any length key is valid for HMAC; `key` is a fixed 32 bytes.
        let mut mac = <hmac::Hmac<Sha256> as Mac>::new_from_slice(&key)
            .expect("a 32-byte key is always valid for HMAC");
        mac.update(message);
        mac.finalize().into_bytes().to_vec()
    }

    /// Open a blob [`seal`](Self::seal) produced, with the same `purpose` and
    /// `aad` it was sealed with.
    pub fn open(&self, purpose: &str, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        const NONCE_LEN: usize = 12;
        if sealed.len() < NONCE_LEN {
            bail!("sealed value is shorter than a nonce");
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
        let key = self.derive_key(purpose);
        let cipher = Aes256Gcm::new_from_slice(&key).expect("32-byte key is always valid");
        let nonce = Nonce::from_slice(nonce_bytes);
        let payload = aes_gcm::aead::Payload {
            msg: ciphertext,
            aad,
        };
        cipher
            .decrypt(nonce, payload)
            .map_err(|_| {
                anyhow::anyhow!("decryption failed: wrong key, wrong aad, or tampered data")
            })
            .context("failed to open sealed value")
    }
}

/// Parse `ENCRYPTION_KEY` as given in the environment: 64 hex characters, or
/// base64 (standard or URL-safe, padded or not), either decoding to exactly 32
/// bytes. `None` input is not an error here — an absent key disables TOTP
/// enrolment rather than failing startup, which [`Config`](crate::config::Config)
/// reports instead.
pub fn parse_encryption_key(raw: &str) -> Result<[u8; 32]> {
    let raw = raw.trim();
    let bytes = if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(raw).context("ENCRYPTION_KEY is not valid hex")?
    } else {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(raw))
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(raw))
            .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw))
            .context("ENCRYPTION_KEY must be 32 bytes, given as 64 hex characters or base64")?
    };
    if bytes.len() != 32 {
        bail!(
            "ENCRYPTION_KEY must decode to exactly 32 bytes, got {} bytes",
            bytes.len()
        );
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_opens_round_trip() {
        let sb = SecretBox::new([7u8; 32]);
        let sealed = sb.seal(PURPOSE_TOTP_SECRET, b"user-1", b"a totp secret");
        let opened = sb.open(PURPOSE_TOTP_SECRET, b"user-1", &sealed).unwrap();
        assert_eq!(opened, b"a totp secret");
    }

    #[test]
    fn two_seals_of_the_same_plaintext_differ() {
        let sb = SecretBox::new([7u8; 32]);
        let a = sb.seal(PURPOSE_TOTP_SECRET, b"user-1", b"same plaintext");
        let b = sb.seal(PURPOSE_TOTP_SECRET, b"user-1", b"same plaintext");
        assert_ne!(a, b, "a fresh random nonce must make each seal unique");
    }

    #[test]
    fn wrong_aad_does_not_open() {
        let sb = SecretBox::new([7u8; 32]);
        let sealed = sb.seal(PURPOSE_TOTP_SECRET, b"user-1", b"secret");
        assert!(sb.open(PURPOSE_TOTP_SECRET, b"user-2", &sealed).is_err());
    }

    #[test]
    fn wrong_purpose_does_not_open() {
        let sb = SecretBox::new([7u8; 32]);
        let sealed = sb.seal(PURPOSE_TOTP_SECRET, b"user-1", b"secret");
        assert!(sb.open(PURPOSE_RECOVERY_CODE, b"user-1", &sealed).is_err());
    }

    #[test]
    fn different_root_keys_cannot_open_each_others_data() {
        let a = SecretBox::new([1u8; 32]);
        let b = SecretBox::new([2u8; 32]);
        let sealed = a.seal(PURPOSE_TOTP_SECRET, b"user-1", b"secret");
        assert!(b.open(PURPOSE_TOTP_SECRET, b"user-1", &sealed).is_err());
    }

    #[test]
    fn tampered_ciphertext_does_not_open() {
        let sb = SecretBox::new([7u8; 32]);
        let mut sealed = sb.seal(PURPOSE_TOTP_SECRET, b"user-1", b"secret");
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(sb.open(PURPOSE_TOTP_SECRET, b"user-1", &sealed).is_err());
    }

    #[test]
    fn truncated_blob_does_not_open() {
        let sb = SecretBox::new([7u8; 32]);
        assert!(sb.open(PURPOSE_TOTP_SECRET, b"user-1", b"short").is_err());
    }

    #[test]
    fn hmac_is_deterministic_and_purpose_scoped() {
        let sb = SecretBox::new([5u8; 32]);
        let a = sb.hmac(PURPOSE_RECOVERY_CODE, b"ABCDEFGHIJ");
        let b = sb.hmac(PURPOSE_RECOVERY_CODE, b"ABCDEFGHIJ");
        assert_eq!(
            a, b,
            "the same message under the same purpose hashes the same"
        );
        let different_message = sb.hmac(PURPOSE_RECOVERY_CODE, b"ZZZZZZZZZZ");
        assert_ne!(a, different_message);
        let different_purpose = sb.hmac(PURPOSE_TOTP_SECRET, b"ABCDEFGHIJ");
        assert_ne!(a, different_purpose);
    }

    #[test]
    fn hex_key_parses() {
        let hex_key = "0".repeat(63) + "1";
        let key = parse_encryption_key(&hex_key).unwrap();
        assert_eq!(key, {
            let mut k = [0u8; 32];
            k[31] = 1;
            k
        });
    }

    #[test]
    fn base64_key_parses() {
        use base64::Engine;
        let bytes = [9u8; 32];
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let key = parse_encryption_key(&encoded).unwrap();
        assert_eq!(key, bytes);
    }

    #[test]
    fn wrong_length_is_rejected() {
        assert!(parse_encryption_key("deadbeef").is_err());
        use base64::Engine;
        let short = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        assert!(parse_encryption_key(&short).is_err());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_encryption_key("not a key at all, not hex, not base64 either!!").is_err());
    }
}
