//! Operator-held signing keys for plugin JWT signing (B273).
//!
//! `crypto-api.sign-jwt-es256` lets a plugin ask the host to sign a JWT with a
//! key the operator holds, by a short name the plugin calls a `key-ref`,
//! without the key material ever reaching the plugin. The operator maps a
//! plugin's `key-ref` to an environment variable or a file holding a PEM
//! PKCS#8 P-256 private key (the `.p8` format Apple issues for APNs) in the
//! `crypto_signing_keys` site config entry; a plugin may use only the keys
//! mapped for it. This mirrors how an AI provider's API key is configured
//! (`services::ai_provider::AiProviderConfig::api_key_env`): the database
//! holds a reference to where the secret lives, never the secret itself.

use std::collections::HashMap;
use std::sync::Arc;

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::models::SiteConfig;

/// `site_config` key holding the operator's key-ref mappings, a JSON array of
/// [`CryptoSigningKeyMapping`].
pub const CONFIG_KEY_SIGNING_KEYS: &str = "crypto_signing_keys";

/// One operator-configured mapping from a plugin's `key-ref` to where its key
/// material lives. Exactly one of `env_var`/`file_path` should be set; a
/// mapping naming both or neither resolves as unmapped rather than refusing
/// the rest of the list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoSigningKeyMapping {
    /// The plugin this key is mapped to. Another plugin naming the same
    /// `key_name` gets [`KeyError::Unmapped`], the same answer as a `key_name`
    /// nothing maps at all.
    pub plugin: String,
    /// The name the plugin calls this key by (`sign-jwt-es256`'s `key-ref`).
    pub key_name: String,
    /// The environment variable holding the PEM key, if this mapping names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    /// The file path holding the PEM key, if this mapping names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

/// Why a key did not resolve to a usable signing key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// No mapping names this `(plugin, key-ref)` pair, the mapping names
    /// neither or both of its sources, or the named source could not be read.
    Unmapped,
    /// The source read, but is not a usable P-256 PKCS#8 private key.
    Invalid,
}

/// Why building a JWS from a plugin's header and claims failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtBuildError {
    /// `header-json` or `claims-json` is not valid JSON, or `header-json` is
    /// not a JSON object.
    InvalidJson,
    /// The header names an `alg` other than `ES256`.
    AlgOverride,
}

/// Parses and caches operator-held signing keys for the life of the process.
///
/// A key is read from its environment variable or file and parsed once, the
/// first time a plugin asks for it; every later call for the same
/// `(plugin, key-ref)` reuses the parsed key. The cache has no eviction: a
/// rotated key needs a restart, the same as the AI provider's `api_key_env`
/// this mirrors reads its environment variable fresh only because it never
/// parses anything expensive out of it.
pub struct CryptoKeyRegistry {
    db: PgPool,
    cache: RwLock<HashMap<(String, String), Arc<EncodingKey>>>,
}

impl CryptoKeyRegistry {
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            cache: RwLock::new(HashMap::new()),
        }
    }

    async fn mappings(&self) -> Vec<CryptoSigningKeyMapping> {
        match SiteConfig::get(&self.db, CONFIG_KEY_SIGNING_KEYS).await {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Resolve the ES256 encoding key `plugin` mapped under `key_ref`.
    ///
    /// Never returns, logs or puts the key material in an error: a lookup
    /// failure answers [`KeyError`], never the PEM text or the path/variable
    /// name it came from.
    pub async fn resolve(&self, plugin: &str, key_ref: &str) -> Result<Arc<EncodingKey>, KeyError> {
        let cache_key = (plugin.to_string(), key_ref.to_string());
        if let Some(key) = self.cache.read().get(&cache_key) {
            return Ok(key.clone());
        }

        let mappings = self.mappings().await;
        let mapping = mappings
            .iter()
            .find(|m| m.plugin == plugin && m.key_name == key_ref)
            .ok_or(KeyError::Unmapped)?;

        let pem = match (&mapping.env_var, &mapping.file_path) {
            (Some(var), None) => std::env::var(var).map_err(|_| KeyError::Unmapped)?,
            (None, Some(path)) => std::fs::read_to_string(path).map_err(|_| KeyError::Unmapped)?,
            _ => return Err(KeyError::Unmapped),
        };

        let encoding_key =
            EncodingKey::from_ec_pem(pem.as_bytes()).map_err(|_| KeyError::Invalid)?;

        // `from_ec_pem` accepts any EC curve's PKCS#8 bytes; only signing with
        // it proves P-256. A throwaway signature over an empty claim set,
        // discarded, catches a P-384/P-521 key here rather than on the
        // plugin's first real call.
        jsonwebtoken::encode(
            &Header::new(Algorithm::ES256),
            &Value::Object(Default::default()),
            &encoding_key,
        )
        .map_err(|_| KeyError::Invalid)?;

        let encoding_key = Arc::new(encoding_key);
        self.cache.write().insert(cache_key, encoding_key.clone());
        Ok(encoding_key)
    }
}

/// Build the compact JWS: the plugin's claims verbatim, and its header with
/// `alg` forced to `ES256`.
///
/// The plugin may set `kid` and the other registered JWS header parameters
/// (`typ`, `cty`, `jku`, `jwk`, `x5u`, `x5c`, `x5t`, `x5t#S256`); a header
/// naming `alg` as anything but `ES256` is refused rather than overridden.
pub fn sign_jwt_es256(
    key: &EncodingKey,
    header_json: &str,
    claims_json: &str,
) -> Result<String, JwtBuildError> {
    let mut header_value: Value =
        serde_json::from_str(header_json).map_err(|_| JwtBuildError::InvalidJson)?;
    let claims_value: Value =
        serde_json::from_str(claims_json).map_err(|_| JwtBuildError::InvalidJson)?;

    let header_obj = header_value
        .as_object_mut()
        .ok_or(JwtBuildError::InvalidJson)?;
    match header_obj.get("alg") {
        Some(Value::String(alg)) if alg == "ES256" => {}
        Some(_) => return Err(JwtBuildError::AlgOverride),
        None => {}
    }
    header_obj.insert("alg".to_string(), Value::String("ES256".to_string()));

    let header: Header =
        serde_json::from_value(header_value).map_err(|_| JwtBuildError::InvalidJson)?;

    jsonwebtoken::encode(&header, &claims_value, key).map_err(|_| JwtBuildError::InvalidJson)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const EC_P256_PEM_A: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgrbVjpJYkdP5ZEb5j\n\
nXlF2WJj/h9wWSBf4TaAudfBXPmhRANCAARHidkUUbX1SJ2BBg3aB0NePbSIDt4x\n\
MTKtyaM2vwBmEXyPrGrRMJQrxGqjQF4pvgRLjDRBpZmKBLSRbn2ENhpD\n\
-----END PRIVATE KEY-----\n";

    #[test]
    fn sign_jwt_es256_forces_alg_and_keeps_kid() {
        let key = EncodingKey::from_ec_pem(EC_P256_PEM_A.as_bytes()).unwrap();
        let jws = sign_jwt_es256(&key, r#"{"kid":"abc123"}"#, r#"{"iss":"team","iat":1}"#)
            .expect("signs with a valid P-256 key");
        let parts: Vec<&str> = jws.split('.').collect();
        assert_eq!(parts.len(), 3, "a compact JWS has three parts");
    }

    #[test]
    fn sign_jwt_es256_refuses_an_alg_override() {
        let key = EncodingKey::from_ec_pem(EC_P256_PEM_A.as_bytes()).unwrap();
        let result = sign_jwt_es256(&key, r#"{"alg":"HS256"}"#, "{}");
        assert_eq!(result, Err(JwtBuildError::AlgOverride));
    }

    #[test]
    fn sign_jwt_es256_accepts_an_alg_that_already_says_es256() {
        let key = EncodingKey::from_ec_pem(EC_P256_PEM_A.as_bytes()).unwrap();
        let result = sign_jwt_es256(&key, r#"{"alg":"ES256","kid":"k1"}"#, "{}");
        assert!(result.is_ok());
    }

    #[test]
    fn sign_jwt_es256_refuses_malformed_header_json() {
        let key = EncodingKey::from_ec_pem(EC_P256_PEM_A.as_bytes()).unwrap();
        assert_eq!(
            sign_jwt_es256(&key, "not json", "{}"),
            Err(JwtBuildError::InvalidJson)
        );
        assert_eq!(
            sign_jwt_es256(&key, "[]", "{}"),
            Err(JwtBuildError::InvalidJson)
        );
    }

    #[test]
    fn sign_jwt_es256_refuses_malformed_claims_json() {
        let key = EncodingKey::from_ec_pem(EC_P256_PEM_A.as_bytes()).unwrap();
        assert_eq!(
            sign_jwt_es256(&key, "{}", "not json"),
            Err(JwtBuildError::InvalidJson)
        );
    }

    /// A P-384 key parses as EC (`from_ec_pem` accepts any curve), but signing
    /// with it under the ES256 algorithm fails — the check [`CryptoKeyRegistry::resolve`]
    /// relies on to answer [`KeyError::Invalid`] for a key that is not P-256.
    #[test]
    fn a_wrong_curve_key_parses_but_cannot_sign_as_es256() {
        const EC_P384_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDD+9/VBzwFrPcAo4Xdh\n\
oANrxOIuqEB7QFkBn4xAPA4YGsT75VRZc7dP6wYelkFLLgOhZANiAARSA5DVccgN\n\
ZMkrCui07rkP8Ixw7cQAjxhCRLMJ6wzoJXcQLQpQXGpMDY9jp/z7+D0c4XExEIPO\n\
jkkGCwi7CB0yIxPMsGT9sWBF5xg1Sa2gH1oqfC+xq7VNcod+0bvnips=\n\
-----END PRIVATE KEY-----\n";

        let key = EncodingKey::from_ec_pem(EC_P384_PEM.as_bytes())
            .expect("a P-384 PKCS#8 key parses as EC");
        let result = jsonwebtoken::encode(
            &Header::new(Algorithm::ES256),
            &Value::Object(Default::default()),
            &key,
        );
        assert!(result.is_err(), "ES256 signing must refuse a P-384 key");
    }
}
