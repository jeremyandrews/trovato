//! The reader-state write API (`tap_api`), un-deviating M3 deviation 5.
//!
//! M3 shipped `argus_reactions`, `argus_read_state` and `argus_subscriptions`
//! with schemas, indexes, storage functions and unit tests, and **two of the
//! three had no writer** — there was no surface in the frozen kernel through
//! which an authenticated user could write a plugin-owned table
//! (`G-NO-PLUGIN-HTTP`, `M3-DESIGN.md` Decision 5). An upvote had nowhere to
//! go.
//!
//! `KERNEL_API_VERSION (0,99)` routes a `tap_menu` entry with
//! `handler_type = "api"` to `tap_api`, with the authenticated user and a live
//! services handle attached, so this module is the writer those tables were
//! waiting for.
//!
//! Every route here is gated on [`crate::PERM_REACT`] by its menu entry, which
//! the kernel checks **before** dispatch, so nothing in this file runs for a
//! caller who does not hold it. The re-check on `authenticated` is belt and
//! braces: a plugin should not depend on the kernel's gate for its own
//! correctness.

use argus_core::reader::Reaction;
use serde_json::json;
use trovato_sdk::types::{ApiRequest, ApiResponse};

use crate::reader_ports;

/// Callback name: toggle a reaction on a story.
pub const CB_REACT: &str = "argus_react";
/// Callback name: read the caller's reactions on a story.
pub const CB_REACTIONS: &str = "argus_reactions";
/// Callback name: record that the caller has read a story.
pub const CB_MARK_READ: &str = "argus_mark_read";
/// Callback name: subscribe or unsubscribe from a topic.
pub const CB_SUBSCRIBE: &str = "argus_subscribe";

/// Dispatch one reader-state request.
///
/// Returns `None` when the callback is not one of this module's, so
/// `tap_api` can fall through to any other handler the plugin grows.
pub fn dispatch(request: &ApiRequest) -> Option<ApiResponse> {
    let callback = request.callback.as_str();
    if ![CB_REACT, CB_REACTIONS, CB_MARK_READ, CB_SUBSCRIBE].contains(&callback) {
        return None;
    }

    if !request.authenticated {
        return Some(ApiResponse::error(401, "authentication required"));
    }

    Some(match callback {
        CB_REACT => react(request),
        CB_REACTIONS => reactions(request),
        CB_MARK_READ => mark_read(request),
        CB_SUBSCRIBE => subscribe(request),
        // Unreachable: the list above is the same one the guard checks.
        _ => ApiResponse::error(404, "unknown callback"),
    })
}

/// The `:id` path parameter, or a 400.
fn path_id(request: &ApiRequest) -> Result<&str, ApiResponse> {
    match request.params.get("id") {
        Some(id) if !id.is_empty() => Ok(id),
        _ => Err(ApiResponse::error(400, "missing id")),
    }
}

/// `POST /argus/story/:id/react` — toggle one reaction, return the resulting set.
fn react(request: &ApiRequest) -> ApiResponse {
    let story = match path_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let body: serde_json::Value = match request.json() {
        Ok(v) => v,
        Err(e) => return ApiResponse::error(400, &format!("body is not JSON: {e}")),
    };
    let Some(raw) = body.get("reaction").and_then(|v| v.as_str()) else {
        return ApiResponse::error(400, "missing reaction");
    };
    let Ok(reaction) = Reaction::parse(raw) else {
        return ApiResponse::error(400, &format!("unknown reaction: {raw}"));
    };

    let now = match crate::host_ports::host_now() {
        Ok(now) => now,
        Err(_) => return ApiResponse::error(500, "clock unavailable"),
    };

    match reader_ports::apply_reaction(&request.user_id, story, reaction, now) {
        Ok(held) => ok(&json!({
            "story_id": story,
            "reactions": held.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        })),
        // A storage failure is transient; a 503 tells a client to retry rather
        // than to give up on a write that may yet succeed.
        Err(e) => ApiResponse::error(503, &format!("could not record the reaction: {e}")),
    }
}

/// `GET /argus/story/:id/reactions` — what the caller currently holds.
fn reactions(request: &ApiRequest) -> ApiResponse {
    let story = match path_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };

    match reader_ports::load_reactions(&request.user_id, story) {
        Ok(held) => ok(&json!({
            "story_id": story,
            "reactions": held.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        })),
        Err(e) => ApiResponse::error(503, &format!("could not read reactions: {e}")),
    }
}

/// `POST /argus/story/:id/read` — record a read.
///
/// `tap_item_view` already records this when a reader loads the story *page*.
/// This route is for a client that renders the story itself — the iOS series —
/// and never hits the page. Both go through the same idempotent statement, so
/// the two paths cannot disagree.
fn mark_read(request: &ApiRequest) -> ApiResponse {
    let story = match path_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let now = match crate::host_ports::host_now() {
        Ok(now) => now,
        Err(_) => return ApiResponse::error(500, "clock unavailable"),
    };

    match reader_ports::record_view(&request.user_id, story, now) {
        Ok(()) => ok(&json!({ "story_id": story, "read": true })),
        Err(e) => ApiResponse::error(503, &format!("could not record the read: {e}")),
    }
}

/// `PUT /argus/topic/:id/subscribe` — subscribe or unsubscribe.
fn subscribe(request: &ApiRequest) -> ApiResponse {
    let topic = match path_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };

    // Absent means subscribe: a bare PUT to a subscribe endpoint is a request
    // to subscribe, and unsubscribing is the case worth spelling out.
    let subscribed = request
        .json::<serde_json::Value>()
        .ok()
        .and_then(|body| body.get("subscribed").and_then(serde_json::Value::as_bool))
        .unwrap_or(true);

    let now = match crate::host_ports::host_now() {
        Ok(now) => now,
        Err(_) => return ApiResponse::error(500, "clock unavailable"),
    };

    match reader_ports::set_subscription(&request.user_id, topic, subscribed, now) {
        Ok(()) => ok(&json!({ "topic_id": topic, "subscribed": subscribed })),
        Err(e) => ApiResponse::error(503, &format!("could not change the subscription: {e}")),
    }
}

/// A 200 carrying `value`, or a 500 if it somehow will not serialize.
fn ok(value: &serde_json::Value) -> ApiResponse {
    ApiResponse::json(value).unwrap_or_else(|_| ApiResponse::error(500, "serialize failed"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn request(callback: &str, authenticated: bool) -> ApiRequest {
        ApiRequest::new(
            callback,
            "POST",
            "/argus/story/x/react",
            "019ffc00-0000-7000-8000-000000000001",
            authenticated,
        )
    }

    #[test]
    fn a_foreign_callback_falls_through() {
        assert!(dispatch(&request("something_else", true)).is_none());
    }

    #[test]
    fn an_anonymous_caller_is_refused_by_the_plugin_too() {
        for callback in [CB_REACT, CB_REACTIONS, CB_MARK_READ, CB_SUBSCRIBE] {
            let response = dispatch(&request(callback, false)).expect("handled");
            assert_eq!(response.status, 401, "{callback}");
        }
    }

    #[test]
    fn a_missing_id_is_a_400_not_a_write() {
        let response = dispatch(&request(CB_REACT, true)).expect("handled");
        assert_eq!(response.status, 400);
    }

    #[test]
    fn an_unknown_reaction_is_refused() {
        let mut req = request(CB_REACT, true);
        req.params.insert("id".into(), "story-1".into());
        req.body = json!({ "reaction": "shrug" }).to_string();
        let response = dispatch(&req).expect("handled");
        assert_eq!(response.status, 400);
        assert!(response.body.contains("shrug"));
    }

    #[test]
    fn a_body_that_is_not_json_is_refused() {
        let mut req = request(CB_REACT, true);
        req.params.insert("id".into(), "story-1".into());
        req.body = "not json".into();
        assert_eq!(dispatch(&req).expect("handled").status, 400);
    }
}
