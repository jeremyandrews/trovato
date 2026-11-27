//! Cloudflare CDN purge delivery for Trovato (B303).
//!
//! `tap_cache_purge` enqueues the kernel's purge payload on this plugin's own
//! queue; `tap_queue_worker` calls Cloudflare's `purge_cache` API, chunking
//! tags at [`MAX_TAGS_PER_CHUNK`] per request (Cloudflare's own limit) and
//! sending `{"purge_everything": true}` for an "everything" purge. A
//! transport failure, a non-2xx response or a body without `"success":
//! true` traps, so the queue retries with its own exponential backoff.
//!
//! Variables: `zone_id` and `api_token` (sent as `Authorization: Bearer
//! <api_token>`).

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// The queue this plugin drains its own purges from.
const QUEUE_NAME: &str = "trovato_cdn_purge_cloudflare";

/// Cloudflare's own cap on tags per `purge_cache` call.
const MAX_TAGS_PER_CHUNK: usize = 30;

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
                "trovato_cdn_purge_cloudflare",
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

/// Deliver one purge payload to Cloudflare, one `purge_cache` call per chunk
/// (or one `purge_everything` call).
///
/// Unset `zone_id` or `api_token`: logs a warning and drops the job — a
/// configuration gap, not a transient failure.
#[plugin_tap]
pub fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    let (Some(zone_id), Some(api_token)) = (configured("zone_id"), configured("api_token"))
    else {
        host::log(
            "warn",
            "trovato_cdn_purge_cloudflare",
            "zone_id or api_token is not configured; dropping a pending purge",
        );
        return serde_json::json!({ "status": "dropped" });
    };

    let all = input.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let tags: Vec<String> = input
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let url = purge_url(&zone_id);
    for body in purge_bodies(&tags, all) {
        let request = purge_request(&url, &api_token, body.to_string());
        match host::http_request(&request) {
            Ok(response) if response_is_success(&response) => {}
            Ok(response) => panic!(
                "Cloudflare purge to {url} failed: status {} body {}",
                response.status, response.body
            ),
            Err(code) => panic!("Cloudflare purge to {url} failed: transport error {code}"),
        }
    }

    serde_json::json!({ "status": "ok" })
}

/// A configured variable, or `None` when it is unset or empty.
fn configured(name: &str) -> Option<String> {
    host::variables_get(name, "").ok().filter(|v| !v.is_empty())
}

/// The `purge_cache` endpoint for `zone_id`.
fn purge_url(zone_id: &str) -> String {
    format!("https://api.cloudflare.com/client/v4/zones/{zone_id}/purge_cache")
}

/// The request bodies to send: one `{"purge_everything": true}` for `all`,
/// otherwise one `{"tags": [...]}` per chunk of at most
/// [`MAX_TAGS_PER_CHUNK`] tags (empty when there are no tags and `all` is
/// false, so a caller sends nothing).
fn purge_bodies(tags: &[String], all: bool) -> Vec<serde_json::Value> {
    if all {
        return vec![serde_json::json!({ "purge_everything": true })];
    }
    tags.chunks(MAX_TAGS_PER_CHUNK)
        .map(|chunk| serde_json::json!({ "tags": chunk }))
        .collect()
}

/// Build the Cloudflare request: JSON POST to `url`, bearer-authenticated
/// with `api_token`. A pure function so the request shape is testable
/// without a live call.
fn purge_request(url: &str, api_token: &str, body: String) -> HttpRequest {
    HttpRequest::post(url, body)
        .header("Authorization", format!("Bearer {api_token}"))
        .header("Content-Type", "application/json")
        .timeout(10_000)
}

/// Cloudflare's own success signal: 2xx *and* `"success": true` in the body.
/// A 2xx with `"success": false` is how Cloudflare reports a rejected
/// request (a bad token, an over-cap chunk), and must retry the same as a
/// transport error.
fn response_is_success(response: &trovato_sdk::types::HttpResponse) -> bool {
    (200..300).contains(&response.status)
        && serde_json::from_str::<serde_json::Value>(&response.body)
            .ok()
            .and_then(|v| v.get("success").and_then(|s| s.as_bool()))
            .unwrap_or(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn purge_bodies_chunks_tags_at_the_cloudflare_cap() {
        let tags: Vec<String> = (0..31).map(|i| format!("item:{i}")).collect();
        let bodies = purge_bodies(&tags, false);
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["tags"].as_array().unwrap().len(), 30);
        assert_eq!(bodies[1]["tags"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn purge_bodies_all_is_one_purge_everything_call() {
        let tags = vec!["site".to_string(), "items".to_string()];
        let bodies = purge_bodies(&tags, true);
        assert_eq!(bodies, vec![serde_json::json!({"purge_everything": true})]);
    }

    #[test]
    fn purge_bodies_empty_tags_sends_nothing() {
        assert!(purge_bodies(&[], false).is_empty());
    }

    #[test]
    fn purge_url_names_the_zone() {
        assert_eq!(
            purge_url("abc123"),
            "https://api.cloudflare.com/client/v4/zones/abc123/purge_cache"
        );
    }

    #[test]
    fn purge_request_carries_bearer_auth_and_json_content_type() {
        let request = purge_request("https://example.test/purge_cache", "tok", "{}".to_string());
        assert_eq!(
            request.headers.get("Authorization").map(String::as_str),
            Some("Bearer tok")
        );
        assert_eq!(
            request.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(request.method, "POST");
    }

    #[test]
    fn response_is_success_requires_2xx_and_success_true() {
        let ok = trovato_sdk::types::HttpResponse {
            status: 200,
            headers: Default::default(),
            body: r#"{"success":true}"#.to_string(),
        };
        assert!(response_is_success(&ok));

        let rejected = trovato_sdk::types::HttpResponse {
            status: 200,
            headers: Default::default(),
            body: r#"{"success":false,"errors":[{"message":"bad token"}]}"#.to_string(),
        };
        assert!(!response_is_success(&rejected));

        let server_error = trovato_sdk::types::HttpResponse {
            status: 500,
            headers: Default::default(),
            body: r#"{"success":true}"#.to_string(),
        };
        assert!(!response_is_success(&server_error));
    }

    #[test]
    fn queue_info_declares_the_queue() {
        let info = __inner_tap_queue_info();
        assert_eq!(info[0]["name"], QUEUE_NAME);
    }

    #[test]
    fn worker_drops_the_job_when_unconfigured() {
        // Off-wasm, `host::variables_get` always answers its default (empty).
        let result =
            __inner_tap_queue_worker(serde_json::json!({"tags": ["site"], "all": false}));
        assert_eq!(result["status"], "dropped");
    }
}
