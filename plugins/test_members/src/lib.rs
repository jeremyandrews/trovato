//! B73 e2e fixture: a plugin that works with the site's **members** by id.
//!
//! One public `api` route per host function it exercises, each answering the
//! host function's result, or its error code, as JSON, so a kernel test can
//! drive the real wasm over HTTP and read back exactly what the plugin saw.
//! The routes are public on purpose: the gate under test is the plugin's
//! manifest capability, not a permission.

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::{ApiRequest, ApiResponse, MenuRoute};

/// Register the routes.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        // Public facts about users by id: `?ids=a,b,c`.
        MenuRoute::api("GET", "/test-members/users", "load_users").title("Load users"),
        // The one account named `?name=`, or null.
        MenuRoute::api("GET", "/test-members/find-by-name", "find_by_name")
            .title("Find a user by name"),
        // Up to 10 accounts whose name starts with `?prefix=`.
        MenuRoute::api(
            "GET",
            "/test-members/find-by-name-prefix",
            "find_by_name_prefix",
        )
        .title("Find users by name prefix"),
        // Mail one member: `?user_id=&subject=&body=`, or `&body_len=N` for a
        // body of N bytes too long to put in a URL. A GET with a side effect,
        // which only a test fixture may have: it keeps the test free of a
        // session and a form token, neither of which is under test.
        MenuRoute::api("GET", "/test-members/mail-user", "mail_user").title("Mail a member"),
    ]
}

/// Dispatch a request to its callback.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "load_users" => load_users(&request),
        "find_by_name" => find_by_name(&request),
        "find_by_name_prefix" => find_by_name_prefix(&request),
        "mail_user" => mail_user(&request),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}

/// Look up the users named in `?ids=` (comma separated) and answer
/// `{users}` or `{code}`.
fn load_users(request: &ApiRequest) -> ApiResponse {
    let ids = request.query.get("ids").cloned().unwrap_or_default();
    let ids: Vec<&str> = ids.split(',').filter(|id| !id.is_empty()).collect();
    let answer = match host::load_users(&ids) {
        Ok(users) => serde_json::json!({ "users": users }),
        Err(code) => serde_json::json!({ "code": code }),
    };
    ApiResponse::json(&answer).unwrap_or_else(|_| ApiResponse::error(500, "serialize failed"))
}

/// Look up the user named `?name=` and answer `{user}` (`null` for no match)
/// or `{code}`.
fn find_by_name(request: &ApiRequest) -> ApiResponse {
    let name = request.query.get("name").cloned().unwrap_or_default();
    let answer = match host::find_user_by_name(&name) {
        Ok(user) => serde_json::json!({ "user": user }),
        Err(code) => serde_json::json!({ "code": code }),
    };
    ApiResponse::json(&answer).unwrap_or_else(|_| ApiResponse::error(500, "serialize failed"))
}

/// Look up users whose name starts with `?prefix=` and answer `{users}` or
/// `{code}`.
fn find_by_name_prefix(request: &ApiRequest) -> ApiResponse {
    let prefix = request.query.get("prefix").cloned().unwrap_or_default();
    let answer = match host::find_users_by_name_prefix(&prefix) {
        Ok(users) => serde_json::json!({ "users": users }),
        Err(code) => serde_json::json!({ "code": code }),
    };
    ApiResponse::json(&answer).unwrap_or_else(|_| ApiResponse::error(500, "serialize failed"))
}

/// Send `?subject=` and `?body=` (or `?body_len=` bytes of `b`) to the member
/// `?user_id=`, and answer `{sent: true}` or `{sent: false, code}`.
///
/// The plugin names a user, never an address, and gets back only whether it was
/// sent or the code it was refused with.
fn mail_user(request: &ApiRequest) -> ApiResponse {
    let field = |name: &str| request.query.get(name).cloned().unwrap_or_default();
    let body = match field("body_len").parse::<usize>() {
        Ok(len) => "b".repeat(len),
        Err(_) => field("body"),
    };
    let answer = match host::mail_send_to_user(&field("user_id"), &field("subject"), &body) {
        Ok(()) => serde_json::json!({ "sent": true }),
        Err(code) => serde_json::json!({ "sent": false, "code": code }),
    };
    ApiResponse::json(&answer).unwrap_or_else(|_| ApiResponse::error(500, "serialize failed"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_api_route_has_a_callback_the_dispatcher_knows() {
        for route in __inner_tap_menu() {
            let request = ApiRequest::new(&route.callback, "GET", &route.path, "", false);
            assert_ne!(__inner_tap_api(request).status, 404, "{}", route.callback);
        }
    }

    #[test]
    fn an_unknown_callback_is_a_404_not_a_trap() {
        let request = ApiRequest::new("nope", "GET", "/test-members/x", "", false);
        assert_eq!(__inner_tap_api(request).status, 404);
    }
}
