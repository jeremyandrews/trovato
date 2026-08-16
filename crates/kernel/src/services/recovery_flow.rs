//! The kernel-owned account-recovery flow (FR-7c, design §4.4/§4.5, Story 4.6).
//!
//! Story 4.5 froze the *contract* — the `tap_account_recovery` op protocol, its
//! JSON schema, and the owner-scoped fail-closed fold. This module builds the
//! *flow* that contract plugs into, and it is deliberately the only thing that
//! drives it.
//!
//! # The division of labour, restated
//!
//! A plugin owns a recovery **method**: it can describe what it offers, start a
//! challenge, and return a verdict. That is all three of the things it can do.
//! The kernel owns the **flow**: which account is being recovered, a
//! kernel-generated nonce, expiry, single-use, rate limiting, and what a success
//! is worth. A plugin never sees or drives flow state, which is why a rogue
//! plugin cannot skip a step: there is no step for it to skip.
//!
//! # Built-in paths ride through the tap, not beside it
//!
//! Design §4.5 asks for the two built-in methods to be implemented *through the
//! same recovery tap*, dogfooding the contract so the kernel has no second,
//! privileged recovery codepath. We do exactly that at the contract level: each
//! built-in provider speaks the frozen [`RecoveryTapInput`] →
//! [`RecoveryTapResult`] schema, and its answer is merged into the same
//! `Vec<TapResult>` the WASM dispatcher produces and folded by the same
//! `crate::recovery::fold_recovery_verify`.
//!
//! **The one deviation, stated plainly:** the built-ins are *in-process*
//! providers rather than bundled WASM plugins. They cannot be WASM, because the
//! plugin boundary exposes no host interface for sending email
//! (`crates/wit/kernel.wit` imports item-api, db, variables, request-context,
//! user-api, cache-api, plugin-api, logging, ai-api, crypto-api, http, queue —
//! and no mail), so a bundled WASM email provider physically cannot deliver a
//! code. The design anticipated this with its "or at minimum route them through
//! the same flow" clause, and this is that branch: identical schema, identical
//! fold, identical flow, identical rate limiting and auditing. What we do not
//! get is proof that the *WASM transport* works for the built-ins — but Story
//! 4.5's fixture-plugin test already proves that transport end to end, and the
//! integration tests here re-prove it with a real WASM recovery plugin driving
//! this same flow.
//!
//! # Namespacing, and why the built-ins are not special-cased in the fold
//!
//! Built-in methods are namespaced exactly like plugin ones —
//! `trovato_email_recovery:code` and `trovato_recovery_codes:code` — and the
//! fold applies the same owner check to them. A plugin that tried to claim
//! `trovato_email_recovery:code` would have its verdict ignored for the same
//! reason a plugin claiming another plugin's namespace does.

use anyhow::{Context, Result};
use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::recovery::{
    RecoveryAccount, RecoveryMethod, RecoveryTapInput, RecoveryTapResult, Verdict,
};
use crate::tap::TapResult;

/// The reserved plugin name owning the built-in emailed-code method.
pub const BUILTIN_EMAIL_PROVIDER: &str = "trovato_email_recovery";

/// The reserved plugin name owning the built-in pre-generated-code method.
pub const BUILTIN_CODES_PROVIDER: &str = "trovato_recovery_codes";

/// The namespaced method id of the emailed-code path.
pub const METHOD_EMAIL: &str = "trovato_email_recovery:code";

/// The namespaced method id of the pre-generated-code path.
pub const METHOD_RECOVERY_CODES: &str = "trovato_recovery_codes:code";

/// Site-config key enabling the built-in email recovery path.
pub const CONFIG_EMAIL_ENABLED: &str = "recovery_email_enabled";

/// Site-config key enabling the built-in pre-generated recovery codes.
pub const CONFIG_CODES_ENABLED: &str = "recovery_codes_enabled";

/// How long a recovery flow stays alive, in seconds.
///
/// The kernel's own bound. A plugin's `expires_in_secs` is advisory only
/// (design §4.2 says so explicitly): a method that claims a 24-hour window does
/// not get one.
pub const FLOW_TTL_SECS: i64 = 900;

/// How long the scoped credential-reset grant lasts after a successful recovery.
///
/// Short: it exists only to carry the user from "proved it is me" to "set a new
/// factor", and it is emphatically not a session (D-38).
pub const GRANT_TTL_SECS: i64 = 600;

/// How many pre-generated recovery codes are issued at a time.
pub const RECOVERY_CODE_BATCH: usize = 10;

/// Session key holding the in-flight recovery flow id.
pub const SESSION_RECOVERY_FLOW: &str = "recovery_flow";

/// Session key holding the D-38 scoped credential-reset grant.
pub const SESSION_RECOVERY_GRANT: &str = "recovery_grant";

/// Where a recovery flow has got to (design §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowState {
    /// The account is bound and the method chooser has been presented.
    Started,
    /// A method was chosen and its challenge initiated.
    AwaitingVerification,
}

/// The kernel's record of one recovery attempt.
///
/// Lives in Redis under the flow nonce with a TTL, so the flow is bounded even
/// if the user simply walks away. The plugin never sees this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryFlow {
    /// The kernel-generated nonce. Opaque to plugins.
    pub flow_id: String,
    /// The account being recovered.
    pub user_id: Uuid,
    /// Whether the account has a deliverable email (the `describe` hint).
    pub email_present: bool,
    /// Where the flow has got to.
    pub state: FlowState,
    /// The chosen method, once one has been.
    pub method_id: Option<String>,
    /// Unix seconds when the flow expires.
    pub expires_at: i64,
}

/// The D-38 scoped grant: what a completed recovery is worth.
///
/// **Not a session.** It authorizes exactly one thing — setting a new
/// credential on the named account — and then the normal session is established
/// through `setup_session`, so `cycle_id` fires. A recovery success never hands
/// out a standing authenticated session, which is what bounds the blast radius
/// of the weakest link in the whole system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryGrant {
    /// The account the grant is for.
    pub user_id: Uuid,
    /// The flow that produced it, for the audit trail.
    pub flow_id: String,
    /// Unix seconds when the grant stops being honoured.
    pub expires_at: i64,
}

impl RecoveryGrant {
    /// Whether the grant is still live.
    pub fn is_live(&self, now: i64) -> bool {
        now < self.expires_at
    }
}

/// Hash a recovery secret for storage. Never store the plaintext.
pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

/// Generate a human-transcribable recovery secret.
///
/// Base32-ish alphabet with the ambiguous characters removed (no O/0, no I/1/L),
/// because these are read off a screen, written down, and typed back in. An
/// alphabet that produces support tickets is a worse alphabet.
pub fn generate_secret(len: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Constant-time comparison of a submitted secret against a stored hash.
///
/// Both sides are fixed-length hex digests, so this leaks nothing through
/// timing about how much of a code was correct.
pub fn secret_matches(submitted: &str, stored_hash: &str) -> bool {
    use subtle::ConstantTimeEq;
    let submitted_hash = hash_secret(submitted.trim());
    submitted_hash
        .as_bytes()
        .ct_eq(stored_hash.as_bytes())
        .into()
}

// ─── Built-in providers ──────────────────────────────────────────────────────

/// A recovery method the kernel implements in-process.
///
/// The signature is the frozen tap contract verbatim: a provider sees exactly
/// what a WASM plugin sees and can return exactly what a WASM plugin can return.
/// That is what makes "the built-ins ride through the tap" true at the level
/// that matters — the schema and the fold — rather than merely asserted.
#[async_trait]
pub trait RecoveryProvider: Send + Sync {
    /// The reserved plugin name this provider answers as. The fold's
    /// owner-namespace check is applied to it unchanged.
    fn provider_name(&self) -> &'static str;

    /// Whether the site has this path switched on.
    async fn is_enabled(&self) -> bool;

    /// Handle one op, or decline (`None`) to answer this dispatch at all —
    /// exactly as a plugin that does not implement the tap casts no vote.
    async fn handle(&self, input: &RecoveryTapInput) -> Option<RecoveryTapResult>;
}

/// Run the built-in providers over one input and render their answers as
/// `TapResult`s, so they enter the same fold as the WASM ones.
pub async fn dispatch_builtins(
    providers: &[std::sync::Arc<dyn RecoveryProvider>],
    input: &RecoveryTapInput,
) -> Vec<TapResult> {
    let mut results = Vec::new();
    for provider in providers {
        if !provider.is_enabled().await {
            continue;
        }
        if let Some(output) = provider.handle(input).await {
            // Serialize and hand back the same opaque string a WASM plugin
            // would, rather than short-circuiting into the typed value: the
            // fold must see built-in and plugin answers in identical form.
            match serde_json::to_string(&output) {
                Ok(json) => results.push(TapResult {
                    plugin_name: provider.provider_name().to_string(),
                    output: json,
                }),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        provider = provider.provider_name(),
                        "failed to encode a built-in recovery result; treating as no vote"
                    );
                }
            }
        }
    }
    results
}

/// Collect the `methods` answers out of a describe dispatch.
///
/// Composition is a **union of independent methods, never a quorum** (design
/// §4.5): every provider contributes its namespaced methods and the user picks
/// one way back in. A method whose owner does not actually own its namespace is
/// dropped, so a plugin cannot advertise a method it could never legitimately
/// verify.
pub fn collect_methods(results: &[TapResult]) -> Vec<RecoveryMethod> {
    let mut methods = Vec::new();
    for result in results {
        let Ok(RecoveryTapResult::Methods { methods: offered }) =
            serde_json::from_str::<RecoveryTapResult>(&result.output)
        else {
            continue;
        };
        for method in offered {
            if method
                .method_id
                .starts_with(&format!("{}:", result.plugin_name))
            {
                methods.push(method);
            } else {
                tracing::warn!(
                    plugin = %result.plugin_name,
                    method_id = %method.method_id,
                    "recovery describe: dropped a method whose namespace the advertiser does not own"
                );
            }
        }
    }
    methods
}

/// Whether a plugin owns a namespaced method id.
///
/// The same rule the fold applies, exposed so `initiate` can refuse to dispatch
/// a method to a non-owner rather than relying on the fold to clean up later.
pub fn owns_method(plugin_name: &str, method_id: &str) -> bool {
    method_id.starts_with(&format!("{plugin_name}:"))
}

// ─── Flow storage ────────────────────────────────────────────────────────────

/// The Redis key holding one flow.
fn flow_key(flow_id: &str) -> String {
    format!("recovery_flow:{flow_id}")
}

/// Kernel-owned recovery flow storage: nonce, expiry, single-use.
#[derive(Clone)]
pub struct RecoveryFlowStore {
    redis: redis::Client,
}

impl RecoveryFlowStore {
    /// Create the store over the kernel's Redis client.
    pub fn new(redis: redis::Client) -> Self {
        Self { redis }
    }

    /// Start a flow for an account, minting the kernel-owned nonce.
    pub async fn start(
        &self,
        user_id: Uuid,
        email_present: bool,
        now: i64,
    ) -> Result<RecoveryFlow> {
        let flow = RecoveryFlow {
            // A v4 UUID: unguessable, and never derived from the account, so the
            // nonce cannot leak who is being recovered.
            flow_id: Uuid::new_v4().to_string(),
            user_id,
            email_present,
            state: FlowState::Started,
            method_id: None,
            expires_at: now + FLOW_TTL_SECS,
        };
        self.put(&flow).await?;
        Ok(flow)
    }

    /// Write a flow and (re)set its TTL.
    pub async fn put(&self, flow: &RecoveryFlow) -> Result<()> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis for the recovery flow")?;
        let encoded = serde_json::to_string(flow).context("failed to encode the recovery flow")?;
        let _: () = conn
            .set_ex(flow_key(&flow.flow_id), encoded, FLOW_TTL_SECS as u64)
            .await
            .context("failed to store the recovery flow")?;
        Ok(())
    }

    /// Load a live flow. An expired or unknown nonce is simply `None`.
    pub async fn get(&self, flow_id: &str, now: i64) -> Result<Option<RecoveryFlow>> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis for the recovery flow")?;
        let raw: Option<String> = conn
            .get(flow_key(flow_id))
            .await
            .context("failed to read the recovery flow")?;
        let Some(flow) = raw.and_then(|v| serde_json::from_str::<RecoveryFlow>(&v).ok()) else {
            return Ok(None);
        };
        // Belt and braces over the Redis TTL: never honour a flow past its own
        // stamp, even if the key outlived it.
        if now >= flow.expires_at {
            return Ok(None);
        }
        Ok(Some(flow))
    }

    /// Burn a flow. Called on success and on terminal failure, so a nonce is
    /// single-use in both directions.
    pub async fn burn(&self, flow_id: &str) -> Result<()> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis for the recovery flow")?;
        let _: Result<i64, _> = conn.del(flow_key(flow_id)).await;
        Ok(())
    }
}

impl std::fmt::Debug for RecoveryFlowStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryFlowStore").finish()
    }
}

/// Build the account context handed to providers for a flow.
pub fn account_of(flow: &RecoveryFlow) -> RecoveryAccount {
    RecoveryAccount {
        user_id: flow.user_id,
        email_present: flow.email_present,
    }
}

/// Whether a verdict should keep the flow open rather than end it.
pub fn verdict_keeps_flow_open(verdict: Verdict) -> bool {
    matches!(verdict, Verdict::Pending)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn secrets_use_an_unambiguous_alphabet() {
        // These are read off a screen and typed back in; O/0 and I/1/L confusion
        // is a support ticket, not a security property, but it is still a bug.
        let secret = generate_secret(200);
        assert_eq!(secret.chars().count(), 200);
        for forbidden in ['O', '0', 'I', '1', 'L'] {
            assert!(
                !secret.contains(forbidden),
                "the alphabet must exclude {forbidden}"
            );
        }
        assert!(
            secret
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn secrets_are_not_predictable() {
        let a = generate_secret(16);
        let b = generate_secret(16);
        assert_ne!(a, b);
    }

    #[test]
    fn hashing_is_stable_and_one_way() {
        let secret = "ABCD2345";
        assert_eq!(hash_secret(secret), hash_secret(secret));
        assert_ne!(hash_secret(secret), secret);
        assert_eq!(hash_secret(secret).len(), 64);
    }

    #[test]
    fn secret_matching_accepts_the_right_code_and_rejects_others() {
        let secret = generate_secret(10);
        let stored = hash_secret(&secret);
        assert!(secret_matches(&secret, &stored));
        assert!(!secret_matches("WRONGCODE1", &stored));
        assert!(!secret_matches("", &stored));
    }

    #[test]
    fn secret_matching_tolerates_surrounding_whitespace() {
        // Copy-paste from an email routinely carries a trailing newline.
        let secret = generate_secret(10);
        let stored = hash_secret(&secret);
        assert!(secret_matches(&format!("  {secret}\n"), &stored));
    }

    #[test]
    fn built_in_method_ids_are_namespaced_by_their_owner() {
        assert!(owns_method(BUILTIN_EMAIL_PROVIDER, METHOD_EMAIL));
        assert!(owns_method(BUILTIN_CODES_PROVIDER, METHOD_RECOVERY_CODES));
        // ...and cross-claims do not hold.
        assert!(!owns_method(BUILTIN_CODES_PROVIDER, METHOD_EMAIL));
        assert!(!owns_method(BUILTIN_EMAIL_PROVIDER, METHOD_RECOVERY_CODES));
    }

    #[test]
    fn ownership_is_delimiter_exact() {
        // "p" must not own "p_evil:code" — the same prefix-collision guard the
        // frozen fold applies.
        assert!(!owns_method("p", "p_evil:code"));
        assert!(owns_method("p", "p:code"));
    }

    fn methods_result(plugin: &str, ids: &[&str]) -> TapResult {
        let methods: Vec<RecoveryMethod> = ids
            .iter()
            .map(|id| RecoveryMethod {
                method_id: (*id).to_string(),
                display_name: "m".into(),
                available: true,
            })
            .collect();
        TapResult {
            plugin_name: plugin.to_string(),
            output: serde_json::to_string(&RecoveryTapResult::Methods { methods }).unwrap(),
        }
    }

    #[test]
    fn describe_composes_as_a_union() {
        let results = vec![
            methods_result("alpha", &["alpha:one", "alpha:two"]),
            methods_result("beta", &["beta:one"]),
        ];
        let methods = collect_methods(&results);
        assert_eq!(methods.len(), 3, "every provider's methods are offered");
    }

    #[test]
    fn describe_drops_a_method_the_advertiser_does_not_own() {
        // A plugin advertising someone else's namespace could otherwise lure a
        // user into a method it will never be allowed to verify — a denial-of-
        // recovery, and a confusing one.
        let results = vec![methods_result(
            "rogue",
            &["rogue:ok", "trovato_email_recovery:code"],
        )];
        let methods = collect_methods(&results);
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].method_id, "rogue:ok");
    }

    #[test]
    fn describe_ignores_a_wrong_shape_answer() {
        let results = vec![TapResult {
            plugin_name: "p".into(),
            output: r#"{"result":"verdict","verdict":"Verified"}"#.into(),
        }];
        assert!(collect_methods(&results).is_empty());
    }

    #[test]
    fn a_grant_expires() {
        let grant = RecoveryGrant {
            user_id: Uuid::now_v7(),
            flow_id: "f".into(),
            expires_at: 1000,
        };
        assert!(grant.is_live(999));
        assert!(!grant.is_live(1000));
        assert!(!grant.is_live(1001));
    }

    #[test]
    fn the_kernel_ttl_is_short_and_independent_of_any_plugin_advisory() {
        // A plugin's `expires_in_secs` is advisory (design §4.2). Whatever it
        // claims, the flow dies at the kernel's bound.
        const { assert!(FLOW_TTL_SECS <= 3600) };
        const { assert!(GRANT_TTL_SECS <= FLOW_TTL_SECS) };
    }

    #[test]
    fn only_pending_keeps_a_flow_open() {
        assert!(verdict_keeps_flow_open(Verdict::Pending));
        assert!(!verdict_keeps_flow_open(Verdict::Verified));
        assert!(!verdict_keeps_flow_open(Verdict::Rejected));
        assert!(!verdict_keeps_flow_open(Verdict::NoOpinion));
    }
}
