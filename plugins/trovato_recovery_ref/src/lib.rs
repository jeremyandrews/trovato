//! FR-7c reference plugin — the canonical `tap_account_recovery` implementation.
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain, this is a **real, enableable** reference (not a test-only fixture).
//! It exists to:
//!
//! 1. **Validate the frozen `tap_account_recovery` schema before PF-5** (the
//!    `tap-csp-alter` / Story-2.4 discipline — never freeze an unexercised
//!    payload). Story 4.5's integration test drives this plugin through the real
//!    kernel `TapDispatcher`, round-tripping `RecoveryTapInput` →
//!    `RecoveryTapResult`.
//! 2. **Document the method-author pattern** every recovery plugin follows: bind
//!    the op-discriminated [`RecoveryTapInput`] (never a literal `String`),
//!    answer `describe` with your namespaced methods, `initiate` with a
//!    secret-free challenge hint, and `verify` with a bare [`Verdict`] — and only
//!    ever speak for the `method_id` you own (`trovato_recovery_ref:code`).
//!
//! It owns exactly one method, `trovato_recovery_ref:code` (an email-code-shaped
//! path). The kernel fold counts this plugin's `Verified` only because the
//! `method_id` namespace (`trovato_recovery_ref:`) matches the plugin name.
//!
//! The correct-code check here is a **fixture convenience**: the real code store
//! (generation, hashing, single-use, TTL) is Story 4.6's built-in path. This
//! reference only proves the contract round-trips and the fold is enforceable.

use trovato_sdk::plugin_tap;
use trovato_sdk::types::{RecoveryMethod, RecoveryTapInput, RecoveryTapResult, Verdict};

/// The single method this reference owns. Namespaced `<plugin_name>:<method>`.
const METHOD_ID: &str = "trovato_recovery_ref:code";

/// The response this fixture treats as the valid code. Fixture-only — the real
/// code store is Story 4.6.
const CORRECT_CODE: &str = "123456";

/// Reference `tap_account_recovery`: describe one method, initiate a challenge,
/// and return a verdict on `verify` — only for the method this plugin owns.
#[plugin_tap]
pub fn tap_account_recovery(input: RecoveryTapInput) -> RecoveryTapResult {
    match input {
        RecoveryTapInput::Describe { .. } => RecoveryTapResult::Methods {
            methods: vec![RecoveryMethod {
                method_id: METHOD_ID.to_string(),
                display_name: "Email recovery code".to_string(),
                available: true,
            }],
        },
        RecoveryTapInput::Initiate { method_id, .. } => {
            if method_id == METHOD_ID {
                RecoveryTapResult::Initiated {
                    status: "initiated".to_string(),
                    challenge_hint: "A 6-digit code was sent to your registered email".to_string(),
                    expires_in_secs: 900,
                }
            } else {
                // Not our method — report unavailable rather than pretend to start.
                RecoveryTapResult::Initiated {
                    status: "unavailable".to_string(),
                    challenge_hint: String::new(),
                    expires_in_secs: 0,
                }
            }
        }
        RecoveryTapInput::Verify {
            method_id,
            response,
            ..
        } => {
            // Only speak for the method we own; for anything else, NoOpinion (the
            // kernel fold ignores our vote on a foreign method_id regardless, but
            // a well-behaved plugin does not pretend to judge others' methods).
            let verdict = if method_id != METHOD_ID {
                Verdict::NoOpinion
            } else if response == CORRECT_CODE {
                Verdict::Verified
            } else {
                Verdict::Rejected
            };
            RecoveryTapResult::Verdict { verdict }
        }
    }
}
