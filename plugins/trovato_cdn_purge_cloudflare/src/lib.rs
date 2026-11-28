//! Edge cache purge via Cloudflare's `purge_cache` API (B303).
//!
//! `tap_cache_purge` only enqueues: the tap is a notification
//! (`tap-cache-purge: func(...) -> result<_, string>`), and delivery and
//! retry belong to the plugin queue. `tap_queue_worker` POSTs to
//! `https://api.cloudflare.com/client/v4/zones/<zone_id>/purge_cache`, in
//! chunks of at most [`MAX_TAGS_PER_REQUEST`] tags, or `purge_everything` for
//! an `all` purge. A response without `"success": true`, or a transport
//! failure, is an error, so the queue retries with backoff.
//!
//! Variables (`plugin.trovato_cdn_purge_cloudflare.*`, read through
//! `host::variables_get`):
//! - `zone_id` (required): the Cloudflare zone to purge.
//! - `api_token` (required): sent as `Authorization: Bearer <api_token>`.

use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::prelude::*;

const PLUGIN: &str = "trovato_cdn_purge_cloudflare";
const QUEUE_NAME: &str = "purge";

/// Cloudflare's `purge_cache` endpoint accepts at most 30 tags per request.
const MAX_TAGS_PER_REQUEST: usize = 30;

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

/// Deliver one purge, in as many requests as the tag count needs.
#[plugin_tap_result]
fn tap_queue_worker(job: Value) -> Result<Value, String> {
    let zone_id = match host::variables_get("zone_id", "") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            host::log("warn", PLUGIN, "no zone_id configured; dropping the purge");
            return Ok(json!({ "status": "dropped", "reason": "no zone_id configured" }));
        }
    };
    let api_token = match host::variables_get("api_token", "") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            host::log(
                "warn",
                PLUGIN,
                "no api_token configured; dropping the purge",
            );
            return Ok(json!({ "status": "dropped", "reason": "no api_token configured" }));
        }
    };

    let all = job.get("all").and_then(Value::as_bool).unwrap_or(false);
    let tags: Vec<String> = job
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let bodies = build_bodies(&tags, all);
    let requests_sent = bodies.len();
    for body in &bodies {
        let request = build_request(&zone_id, &api_token, body);
        interpret_response(host::http_request(&request))?;
    }

    Ok(json!({ "status": "ok", "requests_sent": requests_sent }))
}

/// The request bodies a purge needs: one `purge_everything` body when `all`
/// is true, or one `tags` body per chunk of at most [`MAX_TAGS_PER_REQUEST`]
/// tags otherwise.
fn build_bodies(tags: &[String], all: bool) -> Vec<Value> {
    if all {
        return vec![json!({ "purge_everything": true })];
    }
    if tags.is_empty() {
        return Vec::new();
    }
    tags.chunks(MAX_TAGS_PER_REQUEST)
        .map(|chunk| json!({ "tags": chunk }))
        .collect()
}

/// Build the outbound request (seam for testing without a live API).
fn build_request(zone_id: &str, api_token: &str, body: &Value) -> trovato_sdk::types::HttpRequest {
    let url = format!("https://api.cloudflare.com/client/v4/zones/{zone_id}/purge_cache");
    let body = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
    trovato_sdk::types::HttpRequest::post(url, body)
        .header("Authorization", format!("Bearer {api_token}"))
        .header("Content-Type", "application/json")
        .timeout(10_000)
}

/// Turn the host's answer into the tap's result (seam for testing without a
/// live API): Cloudflare's own `"success"` field decides, not the HTTP status
/// alone, because Cloudflare answers 200 with `"success": false` on some
/// rejections.
fn interpret_response(result: Result<trovato_sdk::types::HttpResponse, i32>) -> Result<(), String> {
    match result {
        Ok(response) => {
            let success = serde_json::from_str::<Value>(&response.body)
                .ok()
                .and_then(|v| v.get("success").and_then(Value::as_bool))
                .unwrap_or(false);
            if success {
                Ok(())
            } else {
                host::log(
                    "error",
                    PLUGIN,
                    &format!(
                        "Cloudflare purge was not successful: HTTP {} {}",
                        response.status, response.body
                    ),
                );
                Err(format!(
                    "Cloudflare purge was not successful: HTTP {}",
                    response.status
                ))
            }
        }
        Err(code) => {
            host::log("error", PLUGIN, &format!("purge request failed: {code}"));
            Err(format!("purge request failed with code {code}"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use trovato_sdk::types::HttpResponse;

    #[test]
    fn worker_drops_when_nothing_is_configured() {
        // The native stub for variables_get always returns the default (""),
        // so this exercises the real "unset" path end to end.
        let result = __inner_tap_queue_worker(json!({"tags": ["site"], "all": false}));
        let value = result.expect("dropping is Ok, not a retry");
        assert_eq!(value["status"], "dropped");
    }

    #[test]
    fn an_all_purge_is_one_purge_everything_body() {
        let bodies = build_bodies(&[], true);
        assert_eq!(bodies, vec![json!({"purge_everything": true})]);
    }

    #[test]
    fn tags_under_the_cap_are_one_body() {
        let tags: Vec<String> = (0..10).map(|n| format!("tag{n}")).collect();
        let bodies = build_bodies(&tags, false);
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["tags"].as_array().unwrap().len(), 10);
    }

    #[test]
    fn tags_over_the_cap_are_chunked_at_thirty() {
        let tags: Vec<String> = (0..65).map(|n| format!("tag{n}")).collect();
        let bodies = build_bodies(&tags, false);
        assert_eq!(bodies.len(), 3, "65 tags in chunks of 30 is 3 requests");
        assert_eq!(bodies[0]["tags"].as_array().unwrap().len(), 30);
        assert_eq!(bodies[1]["tags"].as_array().unwrap().len(), 30);
        assert_eq!(bodies[2]["tags"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn no_tags_and_not_all_sends_nothing() {
        assert!(build_bodies(&[], false).is_empty());
    }

    #[test]
    fn the_request_carries_the_bearer_token_and_the_zone() {
        let request = build_request("zone123", "token-abc", &json!({"tags": ["a"]}));
        assert_eq!(
            request.url,
            "https://api.cloudflare.com/client/v4/zones/zone123/purge_cache"
        );
        assert_eq!(
            request.headers.get("Authorization").map(String::as_str),
            Some("Bearer token-abc")
        );
        let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body, json!({"tags": ["a"]}));
    }

    #[test]
    fn success_true_is_ok() {
        let response = Ok(HttpResponse {
            status: 200,
            headers: HashMap::new(),
            body: r#"{"success":true,"errors":[],"messages":[]}"#.to_string(),
        });
        assert!(interpret_response(response).is_ok());
    }

    #[test]
    fn success_false_is_an_error_even_with_a_200_status() {
        let response = Ok(HttpResponse {
            status: 200,
            headers: HashMap::new(),
            body: r#"{"success":false,"errors":[{"code":1000,"message":"bad zone"}]}"#.to_string(),
        });
        assert!(interpret_response(response).is_err());
    }

    #[test]
    fn an_unparseable_body_is_an_error() {
        let response = Ok(HttpResponse {
            status: 200,
            headers: HashMap::new(),
            body: "<html>not json</html>".to_string(),
        });
        assert!(interpret_response(response).is_err());
    }

    #[test]
    fn a_transport_failure_is_an_error() {
        assert!(interpret_response(Err(-1)).is_err());
    }
}
