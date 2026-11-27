#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin signs a JWT with ES256 using an operator-held key it never sees
//! (**the `crypto-api.sign-jwt-es256` host function**, B273).
//!
//! Before this, a plugin's only way to hold a signing key was a site variable
//! or an item field, readable by anything that reads content or config — no
//! route existed to an APNs-style provider token, which needs an asymmetric
//! signature over a header and claims the plugin controls, from a key it must
//! never see. The operator maps a plugin's `key-ref` to an environment
//! variable or a file holding a PEM PKCS#8 P-256 private key, in the
//! `crypto_signing_keys` site config entry
//! ([`trovato_kernel::services::crypto_keys`]); the host resolves, parses,
//! caches and signs with it, and the key never crosses into WASM memory.
//!
//! Each test drives a WAT probe against `host::register_all` — the same
//! linker production uses — over a live pool, with a plugin name and a
//! `CryptoKeyRegistry` backed by `site_config`.
//!
//! ## Prerequisites
//!
//! `DATABASE_URL`, migrated. No wasm fixture needed: the probe is a hand
//! written WAT module, not a compiled plugin.

mod common;

use std::sync::{Arc, Mutex};

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use sqlx::PgPool;
use trovato_kernel::host;
use trovato_kernel::models::SiteConfig;
use trovato_kernel::plugin::PluginState;
use trovato_kernel::services::crypto_keys::{
    CONFIG_KEY_SIGNING_KEYS, CryptoKeyRegistry, CryptoSigningKeyMapping,
};
use trovato_kernel::tap::{RequestServices, RequestState, UserContext};
use trovato_sdk::host_errors;
use wasmtime::{Engine, Linker, Module, Store};

/// The plugin name the probe calls under, and the one `crypto_signing_keys`
/// maps `jwtkey` and `wrongcurve` to.
const SIGNING_PLUGIN: &str = "crypto_jwt_probe";

/// A different plugin name, mapped to its own key (`not_yours`) but never the
/// one under `SIGNING_PLUGIN`'s name: the fixture for "a plugin cannot use
/// another plugin's key".
const OTHER_PLUGIN: &str = "crypto_jwt_probe_other";

const PRIVATE_KEY_ENV: &str = "TEST_B273_CRYPTO_JWT_P256_KEY";

/// A real PKCS#8 P-256 private key (`openssl ecparam -genkey -noout -name
/// prime256v1 | openssl pkcs8 -topk8 -nocrypt`), used only by this test file.
const PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgoIkpGQlUdZpDKE+i\n\
Hz5Oo/gWX+fvWcsvW9GZtHbRXR2hRANCAATrYJKx95YLzLL7yyl4zz2z+AHoValE\n\
QlEWjyc2nGS6UeCQyIpQ1PvAnlNSYxBfpNcQuNeu7u5yKhkLtZmWnbsq\n\
-----END PRIVATE KEY-----\n";

/// The public half of [`PRIVATE_KEY_PEM`], for the test to verify with —
/// never anything the plugin or the host sees.
const PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE62CSsfeWC8yy+8speM89s/gB6FWp\n\
REJRFo8nNpxkulHgkMiKUNT7wJ5TUmMQX6TXELjXru7ucioZC7WZlp27Kg==\n\
-----END PUBLIC KEY-----\n";

const WRONG_CURVE_ENV: &str = "TEST_B273_CRYPTO_JWT_P384_KEY";

/// A real PKCS#8 P-384 private key — parses as EC, but is not P-256, so
/// signing under ES256 with it must be refused.
const WRONG_CURVE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDD+9/VBzwFrPcAo4Xdh\n\
oANrxOIuqEB7QFkBn4xAPA4YGsT75VRZc7dP6wYelkFLLgOhZANiAARSA5DVccgN\n\
ZMkrCui07rkP8Ixw7cQAjxhCRLMJ6wzoJXcQLQpQXGpMDY9jp/z7+D0c4XExEIPO\n\
jkkGCwi7CB0yIxPMsGT9sWBF5xg1Sa2gH1oqfC+xq7VNcod+0bvnips=\n\
-----END PRIVATE KEY-----\n";

// =============================================================================
// Harness
// =============================================================================

async fn probe_pool() -> PgPool {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run migrations");
    pool
}

/// Seed `crypto_signing_keys` with the three mappings every test here reads,
/// and the environment variables they name. Idempotent and safe to call from
/// every test: `set_env_default` never overwrites, and the `site_config` row
/// is the same value every time.
async fn seed_mappings(pool: &PgPool) {
    trovato_test_utils::env::set_env_default(PRIVATE_KEY_ENV, PRIVATE_KEY_PEM);
    trovato_test_utils::env::set_env_default(WRONG_CURVE_ENV, WRONG_CURVE_PEM);

    let mappings = vec![
        CryptoSigningKeyMapping {
            plugin: SIGNING_PLUGIN.to_string(),
            key_name: "jwtkey".to_string(),
            env_var: Some(PRIVATE_KEY_ENV.to_string()),
            file_path: None,
        },
        CryptoSigningKeyMapping {
            plugin: SIGNING_PLUGIN.to_string(),
            key_name: "wrongcurve".to_string(),
            env_var: Some(WRONG_CURVE_ENV.to_string()),
            file_path: None,
        },
        // Mapped to a different plugin, under a key-ref `SIGNING_PLUGIN` will
        // also ask for by name: proves a refusal cannot be used to borrow it.
        CryptoSigningKeyMapping {
            plugin: OTHER_PLUGIN.to_string(),
            key_name: "not_yours".to_string(),
            env_var: Some(PRIVATE_KEY_ENV.to_string()),
            file_path: None,
        },
    ];
    SiteConfig::set(
        pool,
        CONFIG_KEY_SIGNING_KEYS,
        serde_json::to_value(&mappings).expect("serialize mappings"),
    )
    .await
    .expect("seed crypto_signing_keys");
}

/// A WAT probe that calls `sign-jwt-es256` with the given key-ref, header and
/// claims JSON, writing the JWS (or nothing, on error) to a fixed offset.
fn sign_probe(key_ref: &str, header: &str, claims: &str) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"
(module
  (import "trovato:kernel/crypto-api" "sign-jwt-es256"
    (func $call (param i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 8)
  (data (i32.const 0) "{key_ref}")
  (data (i32.const 512) "{header}")
  (data (i32.const 1024) "{claims}")
  (func (export "run") (result i32)
    (call $call
      (i32.const 0) (i32.const {key_ref_len})
      (i32.const 512) (i32.const {header_len})
      (i32.const 1024) (i32.const {claims_len})
      (i32.const 4096) (i32.const 4096))))
"#,
        key_ref = esc(key_ref),
        header = esc(header),
        claims = esc(claims),
        key_ref_len = key_ref.len(),
        header_len = header.len(),
        claims_len = claims.len(),
    )
}

/// What a probe returned: the host code, and the JWS it wrote when positive.
struct Answer {
    code: i32,
    jws: String,
}

/// Run one `sign-jwt-es256` probe as `plugin_name`, over a registry backed by
/// `pool`'s `site_config`.
async fn run(
    pool: &PgPool,
    plugin_name: &str,
    key_ref: &str,
    header: &str,
    claims: &str,
) -> Answer {
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    host::register_all(&mut linker).expect("register host functions");

    let wat = sign_probe(key_ref, header, claims);
    let module = Module::new(&engine, &wat).expect("compile sign-jwt-es256 probe WAT");

    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_crypto_keys(Arc::new(CryptoKeyRegistry::new(pool.clone())));
    let request = RequestState::new(UserContext::anonymous(), services);
    let state = PluginState::new(request, plugin_name.to_string());
    let mut store = Store::new(&engine, state);

    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("module instantiates against the kernel linker");
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .expect("module exports run");
    let code = run.call_async(&mut store, ()).await.expect("run completes");

    let jws = if code > 0 {
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let mut buf = vec![0u8; code as usize];
        memory.read(&store, 4096, &mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        String::new()
    };
    Answer { code, jws }
}

// =============================================================================
// Tests
// =============================================================================

/// A plugin signs a token and the test verifies it with the public key: the
/// compact JWS decodes under ES256 with the public half of the key the
/// operator mapped, its `kid` is the one the plugin asked for, and its claims
/// round-trip.
#[tokio::test]
async fn a_plugin_signs_a_jwt_the_test_verifies_with_the_public_key() {
    let pool = probe_pool().await;
    seed_mappings(&pool).await;

    let answer = run(
        &pool,
        SIGNING_PLUGIN,
        "jwtkey",
        r#"{"kid":"the-key-id"}"#,
        r#"{"iss":"team-id","iat":1000000000}"#,
    )
    .await;
    assert!(
        answer.code > 0,
        "signing must succeed: code {}",
        answer.code
    );

    let mut validation = Validation::new(Algorithm::ES256);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    let decoding_key =
        DecodingKey::from_ec_pem(PUBLIC_KEY_PEM.as_bytes()).expect("parse test public key");
    let decoded =
        jsonwebtoken::decode::<serde_json::Value>(&answer.jws, &decoding_key, &validation)
            .unwrap_or_else(|e| {
                panic!("the host's JWS did not verify with the matching public key: {e}")
            });

    assert_eq!(decoded.header.alg, Algorithm::ES256);
    assert_eq!(decoded.header.kid.as_deref(), Some("the-key-id"));
    assert_eq!(decoded.claims["iss"], "team-id");
    assert_eq!(decoded.claims["iat"], 1_000_000_000);
}

/// A key-ref nothing maps is refused, not signed with anything.
#[tokio::test]
async fn an_unmapped_key_ref_is_refused() {
    let pool = probe_pool().await;
    seed_mappings(&pool).await;

    let answer = run(&pool, SIGNING_PLUGIN, "no-such-key", "{}", "{}").await;
    assert_eq!(answer.code, host_errors::ERR_CRYPTO_KEY_UNMAPPED);
}

/// `not_yours` is mapped, just not to `SIGNING_PLUGIN` — the same refusal as a
/// key-ref nothing maps at all, so the answer cannot be used to tell "no such
/// key" from "that key belongs to someone else".
#[tokio::test]
async fn a_plugin_cannot_use_another_plugins_key() {
    let pool = probe_pool().await;
    seed_mappings(&pool).await;

    let mine = run(&pool, SIGNING_PLUGIN, "no-such-key", "{}", "{}").await;
    let theirs = run(&pool, SIGNING_PLUGIN, "not_yours", "{}", "{}").await;
    assert_eq!(theirs.code, host_errors::ERR_CRYPTO_KEY_UNMAPPED);
    assert_eq!(
        theirs.code, mine.code,
        "a key mapped to another plugin must refuse exactly like an unmapped one"
    );

    // The owning plugin, naming the very same key-ref, signs successfully —
    // proving the refusal above was about identity, not a broken mapping.
    let as_owner = run(&pool, OTHER_PLUGIN, "not_yours", "{}", "{}").await;
    assert!(
        as_owner.code > 0,
        "the plugin the key is actually mapped to must still be able to use it: code {}",
        as_owner.code
    );
}

/// A key that parses as EC but is not P-256 (here, P-384) is refused, not
/// silently signed with the wrong curve.
#[tokio::test]
async fn a_wrong_curve_key_is_refused() {
    let pool = probe_pool().await;
    seed_mappings(&pool).await;

    let answer = run(&pool, SIGNING_PLUGIN, "wrongcurve", "{}", "{}").await;
    assert_eq!(answer.code, host_errors::ERR_CRYPTO_KEY_INVALID);
}

/// A header naming an `alg` other than `ES256` is refused, not silently
/// overridden: the plugin should not be able to ask the host to sign under a
/// different algorithm than the key is held for.
#[tokio::test]
async fn an_alg_override_is_refused() {
    let pool = probe_pool().await;
    seed_mappings(&pool).await;

    let answer = run(&pool, SIGNING_PLUGIN, "jwtkey", r#"{"alg":"HS256"}"#, "{}").await;
    assert_eq!(answer.code, host_errors::ERR_CRYPTO_JWT_ALG_OVERRIDE);

    // The same header, naming the algorithm the host already forces, signs.
    let answer = run(&pool, SIGNING_PLUGIN, "jwtkey", r#"{"alg":"ES256"}"#, "{}").await;
    assert!(
        answer.code > 0,
        "naming the algorithm the host already uses must not be refused"
    );
}

/// Malformed header or claims JSON is refused before anything is signed.
#[tokio::test]
async fn malformed_json_is_refused() {
    let pool = probe_pool().await;
    seed_mappings(&pool).await;

    let bad_header = run(&pool, SIGNING_PLUGIN, "jwtkey", "not json", "{}").await;
    assert_eq!(bad_header.code, host_errors::ERR_CRYPTO_JWT_INVALID_JSON);

    let bad_claims = run(&pool, SIGNING_PLUGIN, "jwtkey", "{}", "not json").await;
    assert_eq!(bad_claims.code, host_errors::ERR_CRYPTO_JWT_INVALID_JSON);
}

// =============================================================================
// The key never appears in logs
// =============================================================================

/// Every tracing event fired while the wrapped future runs, as one joined
/// string: enough to grep for a secret, not a structured log reader.
#[derive(Clone, Default)]
struct JoinedEvents(Arc<Mutex<String>>);

struct EventText(String);

impl tracing::field::Visit for EventText {
    fn record_debug(&mut self, _field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{value:?} "));
    }
    fn record_str(&mut self, _field: &tracing::field::Field, value: &str) {
        self.0.push_str(value);
        self.0.push(' ');
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for JoinedEvents {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut text = EventText(String::new());
        event.record(&mut text);
        let mut joined = self.0.lock().unwrap();
        joined.push_str(event.metadata().target());
        joined.push(' ');
        joined.push_str(&text.0);
        joined.push('\n');
    }
}

/// Capture every log line a run of probes produces — the happy path and every
/// refusal this file exercises — and assert none of them carry the signing
/// key's PEM text or its base64 body.
#[tokio::test]
async fn the_signing_key_never_appears_in_the_log() {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    common::scoped_subscribers_see_every_callsite();
    let pool = probe_pool().await;
    seed_mappings(&pool).await;

    let events = JoinedEvents::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());

    async {
        let _ = run(
            &pool,
            SIGNING_PLUGIN,
            "jwtkey",
            r#"{"kid":"k1"}"#,
            r#"{"iss":"x"}"#,
        )
        .await;
        let _ = run(&pool, SIGNING_PLUGIN, "no-such-key", "{}", "{}").await;
        let _ = run(&pool, SIGNING_PLUGIN, "wrongcurve", "{}", "{}").await;
        let _ = run(&pool, SIGNING_PLUGIN, "jwtkey", r#"{"alg":"HS256"}"#, "{}").await;
    }
    .with_subscriber(subscriber)
    .await;

    let log = events.0.lock().unwrap().clone();
    // The key's distinctive base64 body, a PEM armor line, and the raw PEM
    // constant itself: any of these landing in the log would be the bug.
    assert!(!log.contains("MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgoIkpGQlUdZpDKE"));
    assert!(!log.contains("BEGIN PRIVATE KEY"));
    assert!(!log.contains(PRIVATE_KEY_PEM));
    assert!(!log.contains(WRONG_CURVE_PEM));
}
