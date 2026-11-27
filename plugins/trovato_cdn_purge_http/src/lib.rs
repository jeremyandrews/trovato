//! Plain HTTP POST CDN purge delivery for Trovato (B303).
//!
//! `tap_cache_purge` enqueues the kernel's purge payload (`{"tags": [...],
//! "all": bool}`) on this plugin's own queue; `tap_queue_worker` POSTs it as
//! JSON to the configured `url`, the queue's own retry and backoff carrying a
//! transport failure or a non-2xx response.
//!
//! Variables: `url` (required) and `secret` (optional, sent as
//! `Authorization: Bearer <secret>`).

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// The queue this plugin drains its own purges from.
const QUEUE_NAME: &str = "trovato_cdn_purge_http";

/// Enqueue the purge payload for delivery. A notification: the kernel does
/// not read what this returns, only whether the call trapped.
#[plugin_tap]
pub fn tap_cache_purge(purge_json: String) -> serde_json::Value {
    let payload: serde_json::Value =
        serde_json::from_str(&purge_json).unwrap_or_else(|_| serde_json::json!({}));
    match host::queue_push(QUEUE_NAME, &payload) {
        Ok(()) => serde_json::json!({ "status": "ok" }),
        Err(code) => {
            host::log(
                "error",
                "trovato_cdn_purge_http",
                &format!("failed to enqueue purge: {code}"),
            );
            serde_json::json!({ "status": "error", "code": code })
        }
    }
}

/// Declare the queue this plugin owns.
#[plugin_tap]
pub fn tap_queue_info() -> serde_json::Value {
    serde_json::json!([{ "name": QUEUE_NAME, "concurrency": 1 }])
}

/// Deliver one purge payload by POSTing it to the configured URL.
///
/// Unset `url`: logs a warning and drops the job (returns normally — this is
/// a configuration gap, not a transient failure, and retrying it changes
/// nothing). A transport failure or a non-2xx response traps, so the queue
/// retries with its own exponential backoff.
#[plugin_tap]
pub fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    let Some(url) = configured_url() else {
        host::log(
            "warn",
            "trovato_cdn_purge_http",
            "url is not configured; dropping a pending purge",
        );
        return serde_json::json!({ "status": "dropped" });
    };

    let body = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
    let request = purge_request(&url, host::variables_get("secret", "").ok().as_deref(), body);

    match host::http_request(&request) {
        Ok(response) if (200..300).contains(&response.status) => {
            serde_json::json!({ "status": "ok" })
        }
        Ok(response) => panic!(
            "CDN purge POST to {url} failed with status {}",
            response.status
        ),
        Err(code) => panic!("CDN purge POST to {url} failed: transport error {code}"),
    }
}

/// The configured `url`, or `None` when it is unset or empty.
fn configured_url() -> Option<String> {
    host::variables_get("url", "")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Build the purge request: JSON POST to `url`, with `Authorization: Bearer
/// <secret>` when `secret` is set and non-empty. A pure function so the
/// request shape is testable without a live HTTP call.
fn purge_request(url: &str, secret: Option<&str>, body: String) -> HttpRequest {
    let request = HttpRequest::post(url, body)
        .header("Content-Type", "application/json")
        .timeout(10_000);
    match secret {
        Some(secret) if !secret.is_empty() => {
            request.header("Authorization", format!("Bearer {secret}"))
        }
        _ => request,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn purge_request_carries_no_authorization_without_a_secret() {
        let request = purge_request("http://example.test/purge", None, "{}".to_string());
        assert_eq!(request.url, "http://example.test/purge");
        assert_eq!(request.method, "POST");
        assert!(!request.headers.contains_key("Authorization"));
        assert_eq!(
            request.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn purge_request_carries_a_bearer_token_with_a_secret() {
        let request = purge_request("http://example.test/purge", Some("shh"), "{}".to_string());
        assert_eq!(
            request.headers.get("Authorization").map(String::as_str),
            Some("Bearer shh")
        );
    }

    #[test]
    fn purge_request_ignores_an_empty_secret() {
        let request = purge_request("http://example.test/purge", Some(""), "{}".to_string());
        assert!(!request.headers.contains_key("Authorization"));
    }

    #[test]
    fn tap_cache_purge_enqueues_on_the_named_queue() {
        let result = __inner_tap_cache_purge(r#"{"tags":["site"],"all":false}"#.to_string());
        assert_eq!(result["status"], "ok");
    }

    #[test]
    fn tap_cache_purge_handles_malformed_json_without_trapping() {
        let result = __inner_tap_cache_purge("not json".to_string());
        assert_eq!(result["status"], "ok");
    }

    #[test]
    fn queue_info_declares_the_queue() {
        let info = __inner_tap_queue_info();
        assert_eq!(info[0]["name"], QUEUE_NAME);
    }

    /// Off-wasm, `host::variables_get` always answers its default (empty), so
    /// the worker always takes the "unset url" drop path here — which is
    /// exactly the behaviour this proves: no `url` means no crash, no panic,
    /// no real request, just a dropped job.
    #[test]
    fn worker_drops_the_job_when_url_is_unconfigured() {
        let result =
            __inner_tap_queue_worker(serde_json::json!({"tags": ["site"], "all": false}));
        assert_eq!(result["status"], "dropped");
    }
}
