//! P11c end-to-end test fixture: the **background-AI principal** (D-40 / D-41).
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain — the same path a plugin author uses — so the P11c integration suite
//! exercises the background-principal authorization through genuine SDK-compiled
//! WASM rather than an in-test decision stub.
//!
//! Its single tap, `tap_cron`, calls the `ai-request` host function and returns
//! the outcome as JSON: `{"ai_code": <i32>}` on the host's error path, or
//! `{"ok": <content>}` if a provider actually answered. The integration test
//! dispatches this plugin under a **background** `RequestState` and asserts the
//! code: with the `ai_background` capability the call clears authorization and
//! reaches provider resolution (no provider configured in the test ⇒
//! `ERR_AI_NO_PROVIDER`); under a web/anonymous `RequestState` the same call is
//! denied at the permission gate (`ERR_AI_PERMISSION_DENIED`), proving the
//! web-denial is intact even for a capability-holding plugin.

use serde_json::json;
use trovato_sdk::plugin_tap;
use trovato_sdk::types::{AiMessage, AiOperationType, AiRequest, AiRequestOptions};

/// Background-AI driver, dispatched by the P11c suite via `TapDispatcher`.
///
/// Issues a minimal Chat `ai-request` and reports the host result verbatim so the
/// test can assert the exact authorization outcome.
#[plugin_tap]
fn tap_cron(_input: serde_json::Value) -> serde_json::Value {
    let request = AiRequest {
        operation: AiOperationType::Chat,
        provider_id: None,
        model: None,
        messages: vec![AiMessage {
            role: "user".to_string(),
            content: "ping".to_string(),
        }],
        input: None,
        options: AiRequestOptions::default(),
    };

    match trovato_sdk::host::ai_request(&request) {
        Ok(response) => json!({ "ok": response.content }),
        Err(code) => json!({ "ai_code": code }),
    }
}
