#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FR-7c Story 4.5 — freeze-supporting validation of the frozen
//! `tap_account_recovery` schema + the owner-scoped, fail-closed fold through
//! **real** SDK-compiled fixture plugins.
//!
//! This is the §7 spike productionised, and the `tap-csp-alter` / Story-2.4
//! discipline made a test: never freeze an unexercised tap. It drives two real
//! `wasm32-wasip1` plugins — `plugins/trovato_recovery_ref` (the legit owner of
//! `trovato_recovery_ref:code`) and `plugins/test_recovery_bystander` (a rogue
//! that forges `Verified` on any `verify`) — through the real kernel path:
//! `TapDispatcher::dispatch("tap_account_recovery", …)` → the plugin WASM → back
//! through `trovato_kernel::recovery::fold_recovery_verify` (design §4.3).
//!
//! # No infrastructure required
//!
//! The recovery plugins read nothing from the database; a lazy, never-connected
//! pool suffices. The tests therefore always run; CI builds the fixtures.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::recovery::{
    RecoveryAccount, RecoveryTapInput, RecoveryTapResult, RecoveryVerifyOutcome, Verdict,
    fold_recovery_verify,
};
use trovato_kernel::tap::{
    RequestServices, RequestState, TapDispatcher, TapRegistry, TapResult, UserContext,
};

const REF_PLUGIN: &str = "trovato_recovery_ref";
const ROGUE_PLUGIN: &str = "test_recovery_bystander";
const REF_METHOD: &str = "trovato_recovery_ref:code";

/// Repo `plugins/` directory (two levels up from this crate).
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// A dispatcher with both recovery fixtures loaded, plus the services template
/// needed to build a per-dispatch `RequestState`.
struct Harness {
    dispatcher: Arc<TapDispatcher>,
    services: RequestServices,
}

impl Harness {
    fn new() -> Self {
        let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
        for name in [REF_PLUGIN, ROGUE_PLUGIN] {
            runtime
                .load_plugin(&plugins_dir().join(name))
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to load fixture '{name}': {e:#}\n\
                         build it first: cargo build -p {name} --target wasm32-wasip1 --release \
                         && cp target/wasm32-wasip1/release/{name}.wasm plugins/{name}/"
                    )
                });
        }
        let runtime = Arc::new(runtime);
        let registry = Arc::new(TapRegistry::from_plugins(&runtime));
        let dispatcher = Arc::new(TapDispatcher::new(Arc::clone(&runtime), registry));

        // Lazy pool: the recovery fixtures never touch the database.
        let db = sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato")
            .expect("lazy pool");
        let services = RequestServices::for_background(db, None, None, reqwest::Client::new())
            .with_plugin_runtime(Arc::clone(&runtime));

        Self {
            dispatcher,
            services,
        }
    }

    /// Dispatch one recovery op through real kernel dispatch, returning every
    /// implementer's `TapResult` (weight order).
    async fn dispatch(&self, input: &RecoveryTapInput) -> Vec<TapResult> {
        let json = serde_json::to_string(input).unwrap();
        let user = UserContext::authenticated(uuid::Uuid::now_v7(), Vec::new());
        let state = RequestState::new(user, self.services.clone());
        self.dispatcher
            .dispatch("tap_account_recovery", &json, state)
            .await
    }
}

fn account() -> RecoveryAccount {
    RecoveryAccount {
        user_id: uuid::Uuid::now_v7(),
        email_present: true,
    }
}

/// Find a named plugin's raw output among the dispatch results.
fn output_of<'a>(results: &'a [TapResult], plugin: &str) -> Option<&'a str> {
    results
        .iter()
        .find(|r| r.plugin_name == plugin)
        .map(|r| r.output.as_str())
}

#[tokio::test(flavor = "multi_thread")]
async fn describe_round_trips_the_frozen_methods_shape() {
    let h = Harness::new();
    let results = h
        .dispatch(&RecoveryTapInput::Describe {
            flow_id: "flow-1".to_string(),
            account: account(),
            locale: Some("en".to_string()),
        })
        .await;

    // The owner advertises exactly its namespaced method through real WASM.
    let out = output_of(&results, REF_PLUGIN).expect("ref plugin responded");
    let parsed: RecoveryTapResult = serde_json::from_str(out).expect("methods result parses");
    match parsed {
        RecoveryTapResult::Methods { methods } => {
            assert_eq!(methods.len(), 1);
            assert_eq!(methods[0].method_id, REF_METHOD);
            assert!(methods[0].available);
        }
        other => panic!("expected Methods, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn initiate_round_trips_the_frozen_initiated_shape() {
    let h = Harness::new();
    let results = h
        .dispatch(&RecoveryTapInput::Initiate {
            flow_id: "flow-1".to_string(),
            account: account(),
            method_id: REF_METHOD.to_string(),
        })
        .await;

    let out = output_of(&results, REF_PLUGIN).expect("ref plugin responded");
    let parsed: RecoveryTapResult = serde_json::from_str(out).expect("initiated result parses");
    match parsed {
        RecoveryTapResult::Initiated {
            status,
            challenge_hint,
            expires_in_secs,
        } => {
            assert_eq!(status, "initiated");
            assert!(!challenge_hint.is_empty());
            assert_eq!(expires_in_secs, 900);
        }
        other => panic!("expected Initiated, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_result_carries_a_verdict_and_no_account_id() {
    // The §4.2 escalation invariant, confirmed empirically on the wire: a verify
    // result is a bare verdict — no user_id, no account, no session material.
    let h = Harness::new();
    let results = h
        .dispatch(&RecoveryTapInput::Verify {
            flow_id: "flow-1".to_string(),
            account: account(),
            method_id: REF_METHOD.to_string(),
            response: "123456".to_string(),
        })
        .await;

    let out = output_of(&results, REF_PLUGIN).expect("ref plugin responded");
    assert!(
        !out.contains("user_id") && !out.contains("account"),
        "verify result must carry no account identity: {out}"
    );
    assert!(matches!(
        serde_json::from_str::<RecoveryTapResult>(out).unwrap(),
        RecoveryTapResult::Verdict {
            verdict: Verdict::Verified
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_verified_grants_and_the_rogue_genuinely_forged_verified() {
    let h = Harness::new();
    let results = h
        .dispatch(&RecoveryTapInput::Verify {
            flow_id: "flow-1".to_string(),
            account: account(),
            method_id: REF_METHOD.to_string(),
            response: "123456".to_string(), // the fixture's correct code
        })
        .await;

    // The rogue REALLY returned a forged Verified on the owner's method_id — so
    // the fold ignoring it is a real security property, not a vacuous one.
    let rogue_out = output_of(&results, ROGUE_PLUGIN).expect("rogue responded");
    assert!(matches!(
        serde_json::from_str::<RecoveryTapResult>(rogue_out).unwrap(),
        RecoveryTapResult::Verdict {
            verdict: Verdict::Verified
        }
    ));

    // Fold: owner Verified, no owner Rejected ⇒ Granted (the rogue's forged vote,
    // being a non-owner of `trovato_recovery_ref:`, is ignored).
    assert_eq!(
        fold_recovery_verify(&results, REF_METHOD),
        RecoveryVerifyOutcome::Granted
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_rejected_beats_the_rogues_forged_verified() {
    let h = Harness::new();
    let results = h
        .dispatch(&RecoveryTapInput::Verify {
            flow_id: "flow-1".to_string(),
            account: account(),
            method_id: REF_METHOD.to_string(),
            response: "000000".to_string(), // wrong code ⇒ owner Rejected
        })
        .await;

    // The rogue still forged Verified…
    let rogue_out = output_of(&results, ROGUE_PLUGIN).expect("rogue responded");
    assert!(matches!(
        serde_json::from_str::<RecoveryTapResult>(rogue_out).unwrap(),
        RecoveryTapResult::Verdict {
            verdict: Verdict::Verified
        }
    ));

    // …yet a genuine owner Rejected beats it: recovery is denied.
    assert_eq!(
        fold_recovery_verify(&results, REF_METHOD),
        RecoveryVerifyOutcome::Rejected
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rogue_forged_verified_on_a_foreign_method_is_ignored_fail_closed() {
    // Dispatch a method NOBODY loaded owns. The owner returns NoOpinion; the rogue
    // forges Verified. Neither is a namespace owner ⇒ no owner vote ⇒ fail-closed.
    let h = Harness::new();
    let results = h
        .dispatch(&RecoveryTapInput::Verify {
            flow_id: "flow-1".to_string(),
            account: account(),
            method_id: "acme_ghost:trusted-contact".to_string(),
            response: "whatever".to_string(),
        })
        .await;

    assert_eq!(
        fold_recovery_verify(&results, "acme_ghost:trusted-contact"),
        RecoveryVerifyOutcome::Denied
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trapped_owner_casts_no_vote_fail_closed() {
    // The rogue OWNS `test_recovery_bystander:*`; on the __trap__ sentinel it
    // panics — a real WASM trap the dispatcher logs and skips, so it casts no
    // vote. With no owner vote, the fold falls through to fail-closed.
    let h = Harness::new();
    let method = "test_recovery_bystander:trap";
    let results = h
        .dispatch(&RecoveryTapInput::Verify {
            flow_id: "flow-1".to_string(),
            account: account(),
            method_id: method.to_string(),
            response: "__trap__".to_string(),
        })
        .await;

    // The trapped owner produced no TapResult at all (dispatcher skipped it).
    assert!(
        output_of(&results, ROGUE_PLUGIN).is_none(),
        "a trapped handler must produce no result"
    );
    assert_eq!(
        fold_recovery_verify(&results, method),
        RecoveryVerifyOutcome::Denied
    );
}
