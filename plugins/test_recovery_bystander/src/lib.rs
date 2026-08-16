//! FR-7c end-to-end test fixture: a **rogue** recovery plugin.
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain, it exists to prove the owner-scoped, fail-closed kernel fold
//! (design §4.3, D-32) through genuine dispatch rather than a stub. Two rogue
//! behaviours:
//!
//! - **Forge `Verified` on any `verify`, for any `method_id`.** This is the
//!   escalation attempt: a rogue trying to approve recovery on a method it does
//!   not own. The kernel fold MUST ignore it because this plugin does not own the
//!   `method_id` namespace — a `Verified` counts only from the namespace owner.
//!   Story 4.5's integration test asserts exactly that: the forged `Verified` on
//!   `trovato_recovery_ref:code` is ignored, and an owner `Rejected` beats it.
//!
//! - **Trap on the `__trap__` response sentinel.** When it *does* own the
//!   dispatched `method_id` (`test_recovery_bystander:*`) and the response is
//!   `"__trap__"`, it panics — a real WASM trap the dispatcher logs and skips, so
//!   the handler casts no vote and the fold falls through to its fail-closed
//!   default (a trapped owner cannot grant).
//!
//! It ships an `.info.toml` declaring only `tap_account_recovery` and the
//! `logging` capability the `#[plugin_tap]` macro requires; `default_enabled =
//! false` so it never loads outside the test.

use trovato_sdk::plugin_tap;
use trovato_sdk::types::{RecoveryTapInput, RecoveryTapResult, Verdict};

/// The response sentinel that makes this fixture trap (to exercise the
/// trapped-handler-casts-no-vote path through real dispatch).
const TRAP_SENTINEL: &str = "__trap__";

/// Rogue `tap_account_recovery`: offers nothing on `describe`, reports
/// unavailable on `initiate`, and **forges `Verified` on every `verify`** — the
/// escalation the owner-scoped fold must neutralise. Traps on `__trap__`.
#[plugin_tap]
pub fn tap_account_recovery(input: RecoveryTapInput) -> RecoveryTapResult {
    match input {
        RecoveryTapInput::Describe { .. } => RecoveryTapResult::Methods {
            methods: Vec::new(),
        },
        RecoveryTapInput::Initiate { .. } => RecoveryTapResult::Initiated {
            status: "unavailable".to_string(),
            challenge_hint: String::new(),
            expires_in_secs: 0,
        },
        RecoveryTapInput::Verify { response, .. } => {
            if response == TRAP_SENTINEL {
                // A real WASM trap — the dispatcher logs and skips it, so this
                // handler casts no vote.
                panic!("test_recovery_bystander: intentional trap (fixture)");
            }
            // Forge approval on ANY method_id — the fold must ignore it because
            // this plugin owns no part of the dispatched namespace.
            RecoveryTapResult::Verdict {
                verdict: Verdict::Verified,
            }
        }
    }
}
