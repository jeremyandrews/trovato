//! FR-7c account-recovery tap contract — the frozen `tap_account_recovery`
//! surface (Story 4.5) plus its owner-scoped, fail-closed kernel fold.
//!
//! This module is the **freeze-gating** slice of Epic 4 (design §4, D-30/D-31/
//! D-32): everything else in FR-7 (WebAuthn ceremonies, the recovery flow state
//! machine, the built-in email/code paths) is release-gating and builds after
//! PF-5. Here we declare only the contract the freeze must include.
//!
//! # The op protocol (one tap, three ops)
//!
//! A plugin provides one or more recovery *methods*; the kernel owns the
//! *flow* (account binding, a kernel-generated flow nonce, expiry, single-use,
//! rate limiting). The single tap `tap_account_recovery` carries an opaque JSON
//! payload discriminated by `op` — exactly the single-opaque-string convention
//! FR-8 froze for `tap_field_access` — with three ops:
//!
//! - `describe`  — "what methods do you offer for this account?" → [`RecoveryTapResult::Methods`]
//! - `initiate`  — "the user chose method M; begin the challenge" → [`RecoveryTapResult::Initiated`]
//! - `verify`    — "the user submitted response R; is it valid?" → [`RecoveryTapResult::Verdict`]
//!
//! # The schema (frozen at PF-5)
//!
//! [`RecoveryTapInput`] is internally tagged `#[serde(tag = "op")]`
//! (describe/initiate/verify); [`RecoveryTapResult`] is tagged
//! `#[serde(tag = "result")]` (methods/initiated/verdict). [`Verdict`] is
//! PascalCase on the wire (`Verified` | `Rejected` | `Pending` | `NoOpinion`).
//! **The `verify` result carries a verdict and nothing else** — no `user_id`, no
//! account, no session material — so a plugin cannot name a different account to
//! escalate into, cannot mint a token, and cannot widen the flow. That
//! structural absence is the first half of the "cannot escalate" guarantee.
//!
//! # The namespaced `method_id` rule
//!
//! Every `method_id` MUST be `<plugin_name>:<method>`, namespaced exactly as the
//! WASM boundary namespaces request-context keys. It is the second half of the
//! escalation guarantee: see the fold below.
//!
//! # The fold (frozen at PF-5): owner-scoped, fail-closed
//!
//! [`fold_recovery_verify`] aggregates the `verify` votes (design §4.3):
//!
//! 1. a plugin's verdict counts **only if it owns the `method_id` namespace**
//!    (`method_id.starts_with("<plugin_name>:")`); a verdict from a non-owner is
//!    ignored;
//! 2. among owner votes, **any owner `Rejected` fails** (a genuine owner rejecting
//!    beats anything);
//! 3. else **≥1 owner `Verified` grants** the plugin side (still subject to the
//!    kernel's own flow checks, built in Story 4.6);
//! 4. else (`Pending` keeps the flow open until TTL; `NoOpinion` / no owner vote /
//!    all traps) **fails closed** — the deliberate inverse of field-access's
//!    fail-open, because this is the primary auth boundary, not a refinement
//!    behind one. A trapped/errored handler casts no vote
//!    (`TapDispatcher::dispatch` logs and skips it), so a method that traps
//!    cannot grant.
//!
//! **Fold-audit amendment (DECIDED 2026-07-17):** when the fold ignores a
//! `Verified` from a **non-owner** (an attempted escalation) or an owner returns a
//! **wrong-shape / unparseable** response, that is surfaced — never silently
//! dropped. The fold *semantics* are unchanged; this is an observability side
//! effect. The interim surface here is a `tracing` warning; it upgrades to a
//! structured audit event once the Epic-4 audit module lands (Story 4.1), per the
//! kernel-internal audit-events direction.
//!
//! # Transport note (the spike surfaced this; document it on the SDK type)
//!
//! The WIT `tap-account-recovery: func(recovery-json: string) -> string` is
//! **transport-opaque only**. The `#[plugin_tap]` macro deserializes the raw JSON
//! *object* directly into the plugin's typed parameter, so a plugin author must
//! bind a type that deserializes from the object (the op-discriminated
//! [`RecoveryTapInput`]), **not** a literal `String` — binding `String` tries to
//! parse a JSON object as a JSON string and fails into an `{"error":…}` output
//! that the fold treats as no vote (silently skipped). This is exactly the
//! `tap_field_access` / `FieldAccessBatchInput` pattern.

use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::tap::TapResult;

/// The account context the kernel passes to a recovery plugin.
///
/// `email_present` is a hint for `describe`/`initiate` (whether the account has a
/// deliverable email); it is `#[serde(default)]` so the leaner `verify` account
/// (`{ "user_id": … }`) round-trips without it.
///
/// SYNC: An identical struct exists in `crates/plugin-sdk/src/types.rs`
/// (`RecoveryAccount`). The kernel serializes this; plugins deserialize it. Both
/// must have the same fields and serde attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAccount {
    /// The account being recovered.
    pub user_id: Uuid,
    /// Whether the account has a deliverable email (a hint for `describe`/`initiate`).
    #[serde(default)]
    pub email_present: bool,
}

/// Input to `tap_account_recovery`, discriminated by `op`
/// (`describe` | `initiate` | `verify`) — the frozen 1.0 recovery payload.
///
/// `flow_id` is a kernel-owned UUID opaque to the plugin; the plugin never sees
/// or drives flow state. See the [module docs](self) for the op protocol.
///
/// SYNC: An identical enum exists in `crates/plugin-sdk/src/types.rs`
/// (`RecoveryTapInput`). The kernel serializes this; plugins deserialize it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RecoveryTapInput {
    /// "What methods do you offer for this account?"
    Describe {
        /// Kernel-owned flow nonce (opaque to the plugin).
        flow_id: String,
        /// The account being recovered.
        account: RecoveryAccount,
        /// The user's locale hint (e.g. `"en"`), if any.
        #[serde(default)]
        locale: Option<String>,
    },
    /// "The user chose method `method_id`; begin the challenge."
    Initiate {
        /// Kernel-owned flow nonce (opaque to the plugin).
        flow_id: String,
        /// The account being recovered.
        account: RecoveryAccount,
        /// The chosen method, namespaced `<plugin_name>:<method>`.
        method_id: String,
    },
    /// "The user submitted `response`; is it valid for this flow?"
    Verify {
        /// Kernel-owned flow nonce (opaque to the plugin).
        flow_id: String,
        /// The account being recovered.
        account: RecoveryAccount,
        /// The method being verified, namespaced `<plugin_name>:<method>`.
        method_id: String,
        /// The user-submitted token/code, opaque to the kernel.
        response: String,
    },
}

/// One recovery method a plugin advertises for an account (a `describe` result).
///
/// SYNC: An identical struct exists in `crates/plugin-sdk/src/types.rs`
/// (`RecoveryMethod`). Plugins serialize this; the kernel deserializes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryMethod {
    /// Namespaced `<plugin_name>:<method>`.
    pub method_id: String,
    /// Human-readable name for the method chooser.
    pub display_name: String,
    /// Whether the method is currently available for this account.
    pub available: bool,
}

/// The `verify` verdict — PascalCase on the wire.
///
/// This is the *entire* payload a plugin may return from `verify`: no account
/// identifier, no session material. See the [module docs](self) for why.
///
/// SYNC: An identical enum exists in `crates/plugin-sdk/src/types.rs`
/// (`Verdict`). Serialized verbatim (`"Verified"` / `"Rejected"` / `"Pending"` /
/// `"NoOpinion"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// The response is valid; the owning plugin approves recovery.
    Verified,
    /// The response is invalid; the owning plugin rejects recovery.
    Rejected,
    /// Not yet decided; keep the flow open until the kernel TTL.
    Pending,
    /// The plugin has no opinion on this method (should not normally happen for
    /// an owner; folds to fail-closed if it is the only owner vote).
    NoOpinion,
}

/// Output of `tap_account_recovery`, discriminated by `result`
/// (`methods` | `initiated` | `verdict`) — the frozen 1.0 recovery result.
///
/// SYNC: An identical enum exists in `crates/plugin-sdk/src/types.rs`
/// (`RecoveryTapResult`). Plugins serialize this; the kernel deserializes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RecoveryTapResult {
    /// Response to `describe`: the methods this plugin offers for the account.
    Methods {
        /// The advertised methods (namespaced ids).
        methods: Vec<RecoveryMethod>,
    },
    /// Response to `initiate`: the challenge has begun.
    Initiated {
        /// `"initiated"` or `"unavailable"`.
        status: String,
        /// A hint SAFE to display to the user — **no secrets**.
        challenge_hint: String,
        /// Advisory only; the kernel enforces its own TTL.
        expires_in_secs: u64,
    },
    /// Response to `verify`: a bare verdict and nothing else.
    Verdict {
        /// The verdict for the (flow, account, method) the kernel dispatched.
        verdict: Verdict,
    },
}

/// The outcome of folding the `verify` votes across all implementers
/// (design §4.3). See [`fold_recovery_verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryVerifyOutcome {
    /// ≥1 owner `Verified`, no owner `Rejected` — the plugin side succeeds.
    /// (The kernel's own flow checks — nonce, single-use, expiry, rate — still
    /// apply on top; those are Story 4.6.)
    Granted,
    /// An owner `Rejected` — recovery denied (an owner rejection beats anything).
    Rejected,
    /// An owner is `Pending` (and none `Rejected`/`Verified`) — keep the flow
    /// open until the kernel TTL.
    Pending,
    /// Fail-closed default: no owner vote at all, only `NoOpinion`, all owner
    /// handlers trapped, or only non-owner votes (which never count).
    Denied,
}

/// Fold the `verify` results of `tap_account_recovery` into a single outcome,
/// **owner-scoped and fail-closed** (design §4.3, D-32).
///
/// `results` are the raw `Vec<TapResult>` from
/// `TapDispatcher::dispatch("tap_account_recovery", …)`; `method_id` is the
/// namespaced method the kernel dispatched `verify` for. A `TapResult` counts
/// only if the acting plugin **owns** the `method_id` namespace
/// (`method_id.starts_with("<plugin_name>:")`) and its output parses as a
/// [`RecoveryTapResult::Verdict`]. Then: any owner `Rejected` ⇒
/// [`RecoveryVerifyOutcome::Rejected`]; else any owner `Verified` ⇒
/// [`RecoveryVerifyOutcome::Granted`]; else any owner `Pending` ⇒
/// [`RecoveryVerifyOutcome::Pending`]; else [`RecoveryVerifyOutcome::Denied`]
/// (fail-closed).
///
/// Per the fold-audit amendment (see the [module docs](self)), an ignored
/// non-owner `Verified` and an owner's wrong-shape/unparseable output are logged
/// (never silently dropped); the fold semantics are unchanged.
pub fn fold_recovery_verify(results: &[TapResult], method_id: &str) -> RecoveryVerifyOutcome {
    let mut owner_verdicts: Vec<Verdict> = Vec::new();

    for r in results {
        let is_owner = method_id.starts_with(&format!("{}:", r.plugin_name));
        match serde_json::from_str::<RecoveryTapResult>(&r.output) {
            Ok(RecoveryTapResult::Verdict { verdict }) => {
                if is_owner {
                    owner_verdicts.push(verdict);
                } else if matches!(verdict, Verdict::Verified) {
                    // Fold-audit amendment (D-32): a non-owner forging `Verified`
                    // is an attempted escalation. Ignored by the fold, but MUST be
                    // surfaced — never silently dropped. Upgrades to a structured
                    // audit event when the Epic-4 audit module lands (Story 4.1).
                    warn!(
                        plugin = %r.plugin_name,
                        method_id = %method_id,
                        "recovery fold: ignored a forged `Verified` from a non-owner of the method_id namespace (attempted escalation)"
                    );
                }
                // A non-owner non-`Verified` verdict is simply not our concern.
            }
            Ok(_wrong_shape) => {
                if is_owner {
                    warn!(
                        plugin = %r.plugin_name,
                        method_id = %method_id,
                        "recovery fold: owner answered `verify` with a non-verdict result shape; no vote"
                    );
                }
            }
            Err(_unparseable) => {
                if is_owner {
                    warn!(
                        plugin = %r.plugin_name,
                        method_id = %method_id,
                        "recovery fold: owner returned an unparseable `verify` output; no vote"
                    );
                }
            }
        }
    }

    if owner_verdicts
        .iter()
        .any(|v| matches!(v, Verdict::Rejected))
    {
        RecoveryVerifyOutcome::Rejected
    } else if owner_verdicts
        .iter()
        .any(|v| matches!(v, Verdict::Verified))
    {
        RecoveryVerifyOutcome::Granted
    } else if owner_verdicts.iter().any(|v| matches!(v, Verdict::Pending)) {
        RecoveryVerifyOutcome::Pending
    } else {
        // Fail-closed: NoOpinion, no owner vote at all, or every owner trapped.
        RecoveryVerifyOutcome::Denied
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A `TapResult` built inline (no dispatch) for fold unit tests.
    fn result(plugin_name: &str, output: &str) -> TapResult {
        TapResult {
            plugin_name: plugin_name.to_string(),
            output: output.to_string(),
        }
    }

    fn verdict_json(v: &str) -> String {
        format!(r#"{{"result":"verdict","verdict":"{v}"}}"#)
    }

    #[test]
    fn input_serde_round_trips_all_three_ops() {
        let acct = RecoveryAccount {
            user_id: Uuid::nil(),
            email_present: true,
        };
        for input in [
            RecoveryTapInput::Describe {
                flow_id: "f".into(),
                account: acct.clone(),
                locale: Some("en".into()),
            },
            RecoveryTapInput::Initiate {
                flow_id: "f".into(),
                account: acct.clone(),
                method_id: "p:code".into(),
            },
            RecoveryTapInput::Verify {
                flow_id: "f".into(),
                account: acct.clone(),
                method_id: "p:code".into(),
                response: "123456".into(),
            },
        ] {
            let json = serde_json::to_string(&input).unwrap();
            let back: RecoveryTapInput = serde_json::from_str(&json).unwrap();
            assert_eq!(input, back);
        }
    }

    #[test]
    fn verify_account_without_email_present_deserializes() {
        // The design's leaner `verify` account `{ "user_id": … }` must round-trip
        // (email_present defaults to false).
        let json = r#"{"op":"verify","flow_id":"f","account":{"user_id":"00000000-0000-0000-0000-000000000000"},"method_id":"p:code","response":"x"}"#;
        let input: RecoveryTapInput = serde_json::from_str(json).unwrap();
        match input {
            RecoveryTapInput::Verify { account, .. } => assert!(!account.email_present),
            _ => panic!("expected Verify"),
        }
    }

    #[test]
    fn output_serde_tags_and_verdict_is_pascal_case() {
        let methods = RecoveryTapResult::Methods {
            methods: vec![RecoveryMethod {
                method_id: "p:code".into(),
                display_name: "Email recovery code".into(),
                available: true,
            }],
        };
        assert!(
            serde_json::to_string(&methods)
                .unwrap()
                .contains(r#""result":"methods""#)
        );

        let initiated = RecoveryTapResult::Initiated {
            status: "initiated".into(),
            challenge_hint: "sent".into(),
            expires_in_secs: 900,
        };
        assert!(
            serde_json::to_string(&initiated)
                .unwrap()
                .contains(r#""result":"initiated""#)
        );

        let verdict = RecoveryTapResult::Verdict {
            verdict: Verdict::Verified,
        };
        let s = serde_json::to_string(&verdict).unwrap();
        assert!(s.contains(r#""result":"verdict""#));
        assert!(
            s.contains(r#""verdict":"Verified""#),
            "Verdict is PascalCase: {s}"
        );
    }

    #[test]
    fn kernel_input_deserializes_into_the_sdk_mirror() {
        // SYNC guard: the kernel serialization must deserialize into the SDK copy.
        let input = RecoveryTapInput::Verify {
            flow_id: "f".into(),
            account: RecoveryAccount {
                user_id: Uuid::now_v7(),
                email_present: false,
            },
            method_id: "p:code".into(),
            response: "123456".into(),
        };
        let json = serde_json::to_string(&input).unwrap();
        let _sdk: trovato_sdk::types::RecoveryTapInput = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn sdk_result_deserializes_into_the_kernel_type() {
        // SYNC guard, the other direction: a plugin (SDK) result must deserialize
        // into the kernel copy the fold parses.
        let sdk = trovato_sdk::types::RecoveryTapResult::Verdict {
            verdict: trovato_sdk::types::Verdict::Rejected,
        };
        let json = serde_json::to_string(&sdk).unwrap();
        let kernel: RecoveryTapResult = serde_json::from_str(&json).unwrap();
        assert_eq!(
            kernel,
            RecoveryTapResult::Verdict {
                verdict: Verdict::Rejected
            }
        );
    }

    #[test]
    fn fold_owner_verified_grants() {
        let r = vec![result("p", &verdict_json("Verified"))];
        assert_eq!(
            fold_recovery_verify(&r, "p:code"),
            RecoveryVerifyOutcome::Granted
        );
    }

    #[test]
    fn fold_ignores_non_owner_forged_verified() {
        // The rogue owns nothing under `p:`; its forged Verified must be ignored,
        // leaving no owner vote ⇒ fail-closed Denied.
        let r = vec![result("rogue", &verdict_json("Verified"))];
        assert_eq!(
            fold_recovery_verify(&r, "p:code"),
            RecoveryVerifyOutcome::Denied
        );
    }

    #[test]
    fn fold_owner_rejected_beats_non_owner_forged_verified() {
        let r = vec![
            result("p", &verdict_json("Rejected")),
            result("rogue", &verdict_json("Verified")),
        ];
        assert_eq!(
            fold_recovery_verify(&r, "p:code"),
            RecoveryVerifyOutcome::Rejected
        );
    }

    #[test]
    fn fold_owner_rejected_beats_owner_verified() {
        // Two owner votes disagree — Rejected wins regardless of order.
        let r = vec![
            result("p", &verdict_json("Verified")),
            result("p", &verdict_json("Rejected")),
        ];
        assert_eq!(
            fold_recovery_verify(&r, "p:code"),
            RecoveryVerifyOutcome::Rejected
        );
    }

    #[test]
    fn fold_empty_is_fail_closed() {
        // No implementer / all handlers trapped (skipped by the dispatcher) ⇒ Denied.
        assert_eq!(
            fold_recovery_verify(&[], "p:code"),
            RecoveryVerifyOutcome::Denied
        );
    }

    #[test]
    fn fold_owner_pending_keeps_flow_open() {
        let r = vec![result("p", &verdict_json("Pending"))];
        assert_eq!(
            fold_recovery_verify(&r, "p:code"),
            RecoveryVerifyOutcome::Pending
        );
    }

    #[test]
    fn fold_owner_wrong_shape_casts_no_vote() {
        // An owner answering `verify` with a `methods` result is a protocol
        // violation ⇒ no vote ⇒ fail-closed.
        let r = vec![result("p", r#"{"result":"methods","methods":[]}"#)];
        assert_eq!(
            fold_recovery_verify(&r, "p:code"),
            RecoveryVerifyOutcome::Denied
        );
    }

    #[test]
    fn fold_owner_error_output_casts_no_vote() {
        // The `{"error":…}` a `String`-bound plugin would emit ⇒ no vote.
        let r = vec![result("p", r#"{"error":"deserialize: …"}"#)];
        assert_eq!(
            fold_recovery_verify(&r, "p:code"),
            RecoveryVerifyOutcome::Denied
        );
    }

    #[test]
    fn fold_namespace_owner_check_is_delimiter_exact() {
        // "p" does not own "p_evil:code" (the ':' delimiter prevents prefix
        // collision), so its Verified is a non-owner vote ⇒ Denied.
        let r = vec![result("p", &verdict_json("Verified"))];
        assert_eq!(
            fold_recovery_verify(&r, "p_evil:code"),
            RecoveryVerifyOutcome::Denied
        );
    }
}
