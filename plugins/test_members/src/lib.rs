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
    ]
}

/// Dispatch a request to its callback.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "load_users" => load_users(&request),
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
