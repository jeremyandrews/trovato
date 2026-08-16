//! K1 e2e fixture: a plugin that **serves HTTP requests** (G-NO-PLUGIN-HTTP).
//!
//! Before `KERNEL_API_VERSION (0,99)` no such plugin could exist: `tap_menu`'s
//! `callback` was dropped on deserialize, the form taps were never dispatched,
//! and `tap_form_ajax` was admin-only and service-less. This fixture is the
//! smallest thing that proves the surface is real — it registers two `api`
//! routes, gates them on a permission, and **writes a plugin-owned table** from
//! an authenticated reader's request, which is the thing that was impossible.
//!
//! Deliberately minimal: no business logic, one table, three callbacks.

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::{ApiRequest, ApiResponse, MenuRoute};

/// Permission required to write a note. Anonymous callers do not hold it.
const PERM_WRITE: &str = "write test notes";

/// Declare the permission the menu entries gate on.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        PERM_WRITE,
        "Write a note through the test plugin API",
    )]
}

/// Register the plugin-served routes.
///
/// `handler_type = "api"` plus a `callback` is what makes the kernel route a
/// request here. The `permission` field is the gate; an entry with an empty
/// permission is public, which the read route uses on purpose so the test can
/// prove the two cases differ.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("POST", "/tpa/note/:slug", "write_note")
            .title("Write a note")
            .permission(PERM_WRITE),
        // Public on purpose: the kernel must gate on what the entry says, not
        // on whether an entry exists.
        MenuRoute::api("GET", "/tpa/notes", "list_notes").title("List notes"),
        // A page entry, to prove the kernel routes only `api` entries here.
        MenuRoute::page("/tpa/page", "Not an API"),
    ]
}

/// Serve one request.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "write_note" => write_note(&request),
        "list_notes" => list_notes(&request),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}

/// Write a note owned by the authenticated caller — the write that had no
/// surface before this existed.
fn write_note(request: &ApiRequest) -> ApiResponse {
    if !request.authenticated {
        // Belt and braces: the kernel's permission gate already refused an
        // anonymous caller, and a plugin should still not trust that.
        return ApiResponse::error(401, "authentication required");
    }

    let Some(slug) = request.params.get("slug") else {
        return ApiResponse::error(400, "missing slug");
    };

    let body: serde_json::Value = match request.json() {
        Ok(v) => v,
        Err(e) => return ApiResponse::error(400, &format!("body is not JSON: {e}")),
    };
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // Idempotent on (user, slug), so a re-delivered request updates rather than
    // duplicating.
    let written = host::execute_raw(
        "INSERT INTO tpa_notes (user_id, slug, text, method) VALUES ($1::uuid, $2, $3, $4) \
         ON CONFLICT (user_id, slug) DO UPDATE SET text = EXCLUDED.text",
        &[
            serde_json::json!(request.user_id),
            serde_json::json!(slug),
            serde_json::json!(text),
            serde_json::json!(request.method),
        ],
    );

    match written {
        Ok(_) => ApiResponse::json(&serde_json::json!({
            "written": true,
            "slug": slug,
            "user_id": request.user_id,
            // Echoed so the test can prove the request context crossed intact.
            "query": request.query,
        }))
        .unwrap_or_else(|_| ApiResponse::error(500, "serialize failed")),
        Err(code) => ApiResponse::error(500, &format!("write failed with code {code}")),
    }
}

/// Read the caller's notes back.
fn list_notes(request: &ApiRequest) -> ApiResponse {
    let rows = host::query_raw(
        "SELECT slug, text, method FROM tpa_notes WHERE user_id = $1::uuid ORDER BY slug",
        &[serde_json::json!(request.user_id)],
    );

    match rows {
        Ok(json) => ApiResponse::with_status(200, json),
        Err(code) => ApiResponse::error(500, &format!("read failed with code {code}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_declares_two_api_routes_with_callbacks() {
        let menus = __inner_tap_menu();
        let api: Vec<&MenuRoute> = menus.iter().filter(|m| m.handler_type == "api").collect();
        assert_eq!(api.len(), 2);
        assert!(api.iter().all(|m| !m.callback.is_empty()));
        assert_eq!(
            api.iter()
                .find(|m| m.callback == "write_note")
                .unwrap()
                .permission,
            PERM_WRITE
        );
    }

    #[test]
    fn an_unknown_callback_is_a_404_not_a_trap() {
        let request = ApiRequest::new("nope", "GET", "/tpa/x", "", false);
        assert_eq!(__inner_tap_api(request).status, 404);
    }

    #[test]
    fn an_anonymous_write_is_refused_by_the_plugin_too() {
        let request = ApiRequest::new("write_note", "POST", "/tpa/note/x", "", false);
        assert_eq!(__inner_tap_api(request).status, 401);
    }

    #[test]
    fn a_write_with_no_slug_is_a_400() {
        let request = ApiRequest::new(
            "write_note",
            "POST",
            "/tpa/note/",
            "00000000-0000-0000-0000-000000000001",
            true,
        );
        assert_eq!(__inner_tap_api(request).status, 400);
    }
}
