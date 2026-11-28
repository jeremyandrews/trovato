//! Edge cache purge over plain HTTP POST (B303).
//!
//! `tap_cache_purge` only enqueues: the tap is a notification
//! (`tap-cache-purge: func(...) -> result<_, string>`), and delivery and
//! retry belong to the plugin queue. `tap_queue_worker` POSTs the purge
//! payload as JSON to the configured `url`, with an optional bearer `secret`.
//! A transport failure or a non-2xx status is an error, so the queue retries
//! with backoff.
//!
//! Variables (`plugin.trovato_cdn_purge_http.*`, read through
//! `host::variables_get`):
//! - `url` (required): where the purge is POSTed.
//! - `secret` (optional): sent as `Authorization: Bearer <secret>`.

use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::prelude::*;

const PLUGIN: &str = "trovato_cdn_purge_http";
const QUEUE_NAME: &str = "purge";

/// Enqueue the purge for delivery.
#[plugin_tap_result]
fn tap_cache_purge(purge: Value) -> Result<Value, String> {
    match host::queue_push(QUEUE_NAME, &purge) {
        Ok(()) => Ok(json!({ "queued": true })),
        Err(code) => {
            host::log("error", PLUGIN, &format!("failed to queue a purge: {code}"));
            Err(format!("failed to queue purge: {code}"))
        }
    }
}

/// Deliver one purge.
///
/// An unset `url` is already reported, once per process, by the host itself
/// (`host::variables_get` warns the first time a plugin reads a variable that
/// was never set): logging it again here on every job would turn that single
/// warning into one per purge.
#[plugin_tap_result]
fn tap_queue_worker(job: Value) -> Result<Value, String> {
    let url = match host::variables_get("url", "") {
        Ok(url) if !url.is_empty() => url,
        _ => return Ok(json!({ "status": "dropped", "reason": "no url configured" })),
    };

    let secret = host::variables_get("secret", "")
        .ok()
        .filter(|s| !s.is_empty());
    let request = build_request(&url, secret.as_deref(), &job);

    interpret_response(&url, host::http_request(&request))
}

/// Build the outbound request (seam for testing without a live server).
fn build_request(url: &str, secret: Option<&str>, job: &Value) -> HttpRequest {
    let body = serde_json::to_string(job).unwrap_or_else(|_| "{}".to_string());
    let mut request = HttpRequest::post(url, body)
        .header("Content-Type", "application/json")
        .timeout(10_000);
    if let Some(secret) = secret {
        request = request.header("Authorization", format!("Bearer {secret}"));
    }
    request
}

/// Turn the host's answer into the tap's result (seam for testing without a
/// live server): a 2xx status is success, anything else (a transport failure
/// or a non-2xx status) is an error, so the queue retries with backoff.
fn interpret_response(url: &str, result: Result<HttpResponse, i32>) -> Result<Value, String> {
    match result {
        Ok(response) if (200..300).contains(&response.status) => {
            Ok(json!({ "status": "ok", "http_status": response.status }))
        }
        Ok(response) => {
            host::log(
                "error",
                PLUGIN,
                &format!("purge POST to {url} answered HTTP {}", response.status),
            );
            Err(format!("purge endpoint answered HTTP {}", response.status))
        }
        Err(code) => {
            host::log(
                "error",
                PLUGIN,
                &format!("purge POST to {url} failed: {code}"),
            );
            Err(format!("purge request failed with code {code}"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn worker_drops_when_no_url_is_configured() {
        // The native stub for variables_get always returns the default (""),
        // so this exercises the real "unset" path end to end.
        let result = __inner_tap_queue_worker(json!({"tags": ["site"], "all": false}));
        let value = result.expect("dropping is Ok, not a retry");
        assert_eq!(value["status"], "dropped");
    }

    #[test]
    fn purge_request_is_json_post_with_the_payload() {
        let job = json!({"tags": ["site"], "all": false});
        let request = build_request("http://example.test/purge", None, &job);
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "http://example.test/purge");
        assert_eq!(
            request.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
        assert!(!request.headers.contains_key("Authorization"));
        let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body, job);
    }

    #[test]
    fn a_configured_secret_is_sent_as_a_bearer_token() {
        let request = build_request("http://example.test/purge", Some("sekrit"), &json!({}));
        assert_eq!(
            request.headers.get("Authorization").map(String::as_str),
            Some("Bearer sekrit")
        );
    }

    #[test]
    fn a_2xx_response_is_success() {
        for status in [200, 201, 204, 299] {
            let response = Ok(HttpResponse {
                status,
                headers: HashMap::new(),
                body: String::new(),
            });
            let result = interpret_response("http://example.test", response);
            assert!(result.is_ok(), "status {status} must be success");
        }
    }

    #[test]
    fn a_non_2xx_response_is_an_error_so_the_queue_retries() {
        for status in [401, 404, 500, 503] {
            let response = Ok(HttpResponse {
                status,
                headers: HashMap::new(),
                body: "nope".to_string(),
            });
            let result = interpret_response("http://example.test", response);
            assert!(result.is_err(), "status {status} must be an error");
        }
    }

    #[test]
    fn a_transport_failure_is_an_error_so_the_queue_retries() {
        let result = interpret_response("http://example.test", Err(-1));
        assert!(result.is_err());
    }
}
