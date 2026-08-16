//! Plugin-served HTTP requests (**G-NO-PLUGIN-HTTP**, K1 fix 1).
//!
//! # The gap this closes
//!
//! Before `KERNEL_API_VERSION (0,99)` there was **no surface in Trovato through
//! which a plugin served a request**, and therefore no way for an authenticated
//! user to write a plugin-owned table. Every candidate closed for a different
//! reason, which is why it read as an absence rather than a decision:
//!
//! - `tap_menu` looked like routing and was not. The SDK's `MenuDefinition`
//!   carried a `callback` field; the kernel's did not, so it was dropped on
//!   deserialize. A plugin author reading the SDK believed they had registered
//!   a handler.
//! - The form taps were unreachable: `FormService` is constructed and exposed
//!   on `AppState`, and no route calls `build` or `process`.
//! - `tap_form_ajax` had a route and was closed three times over: `require_admin`,
//!   `RequestState::without_services` (so a dispatched tap had no DB handle),
//!   and a `form_state_cache` lookup nothing ever writes.
//! - `public_functions` + `invoke` is plugin-to-plugin only.
//! - Record-type admin is list and view only.
//!
//! # What this module does
//!
//! At startup [`build_plugin_api_router`] walks the menu registry and registers
//! an axum route for every entry with `handler_type = "api"` and a non-empty
//! `callback`. A matching request is authorized against the entry's
//! `permission`, then dispatched to the **owning plugin's** `tap_api` with a
//! live services handle and the authenticated user, so the tap can write.
//!
//! # Security posture
//!
//! - **Permission.** The menu entry's `permission` field is the gate, checked
//!   before dispatch. Empty means public — the same convention the rest of the
//!   registry uses. An anonymous caller who fails the check gets 403, never a
//!   dispatch, so a plugin cannot be reached by someone the site did not
//!   authorize.
//! - **CSRF / bearer (G-CSRF-NO-BEARER-BYPASS, recorded in `M3-DESIGN.md`).**
//!   A state-changing method requires a CSRF token **unless the request was
//!   authenticated by an `Authorization: Bearer` API token**. The reasoning is
//!   the threat model, not convenience: CSRF exists because a browser attaches
//!   cookies to a cross-site request by itself, so a cookie-authenticated write
//!   can be forged by any page the victim visits. A bearer token is never
//!   attached automatically by any browser — an attacker's page cannot make the
//!   victim's browser send it — so a bearer-authenticated request has no CSRF
//!   exposure to protect against. The exemption is keyed on the
//!   [`ApiTokenAuth`] marker the token middleware sets *only when the token
//!   actually authenticated the request*: when a session cookie identified the
//!   user, the bearer header was ignored and CSRF still applies. This is what
//!   lets a token client (the Argus iOS series) write without a
//!   session-establishing round-trip.
//! - **Response.** A plugin's `status`, `body` and `content_type` are served as
//!   given, with an out-of-range status served as 500 and the content type
//!   validated as a header value. The kernel does **not** sanitize a plugin's
//!   response body — a plugin that emits HTML is responsible for escaping it,
//!   the same contract view taps have — and `content_type` defaults to
//!   `application/json` for that reason.
//! - **Body size.** Capped at [`MAX_REQUEST_BODY`], because the body crosses the
//!   WASM boundary through the tap I/O buffer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::{
    Extension, Router,
    body::Bytes,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use tower_sessions::Session;

use crate::menu::MenuDefinition;
use crate::middleware::api_token::ApiTokenAuth;
use crate::state::AppState;
use crate::tap::RequestState;
use trovato_sdk::types::{ApiRequest, ApiResponse};

/// Largest request body handed to a plugin, in bytes.
///
/// The body crosses the WASM boundary through the tap I/O buffer, so an
/// unbounded body is a self-inflicted truncation bug. Sized well under the
/// 1 MiB payload cap the freeze records as TUNABLE.
pub const MAX_REQUEST_BODY: usize = 256 * 1024;

/// The `handler_type` value that marks a menu entry as plugin-served.
const API_HANDLER_TYPE: &str = "api";

/// Build a router serving every `handler_type = "api"` menu entry.
///
/// Called once at startup, after `tap_menu` has populated the registry — the
/// same shape [`crate::routes::gather_routes::build_gather_route_router`] uses
/// for gather route aliases.
///
/// Entries are skipped, with a warning, when they are unusable rather than
/// merely absent: no `callback` (the plugin declared an api route and named no
/// handler), a path that is not absolute, an unsupported method, or a path that
/// collides with one already registered — axum panics on a route conflict, and a
/// plugin must not be able to take the process down by declaring a duplicate.
pub fn build_plugin_api_router(menus: &[MenuDefinition]) -> Router<AppState> {
    let mut router = Router::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for menu in menus {
        if menu.handler_type != API_HANDLER_TYPE {
            continue;
        }
        if menu.callback.trim().is_empty() {
            tracing::warn!(
                plugin = %menu.plugin,
                path = %menu.path,
                "skipping api menu entry with no callback"
            );
            continue;
        }
        if !menu.path.starts_with('/') {
            tracing::warn!(
                plugin = %menu.plugin,
                path = %menu.path,
                "skipping api menu entry whose path is not absolute"
            );
            continue;
        }

        let method = menu.method.to_ascii_uppercase();
        let axum_path = to_axum_path(&menu.path);

        if !seen.insert((method.clone(), axum_path.clone())) {
            tracing::warn!(
                plugin = %menu.plugin,
                path = %menu.path,
                method = %method,
                "skipping duplicate api menu entry"
            );
            continue;
        }

        let config = Arc::new(menu.clone());
        let handler = {
            let config = Arc::clone(&config);
            move |state: State<AppState>,
                  session: Session,
                  bearer: Option<Extension<ApiTokenAuth>>,
                  headers: HeaderMap,
                  uri: OriginalUri,
                  method: Method,
                  params: Path<HashMap<String, String>>,
                  query: Query<HashMap<String, String>>,
                  body: Bytes| {
                let config = Arc::clone(&config);
                async move {
                    serve(
                        config, state, session, bearer, headers, uri, method, params, query, body,
                    )
                    .await
                }
            }
        };

        let route = match method.as_str() {
            "GET" => get(handler),
            "POST" => post(handler),
            "PUT" => put(handler),
            "DELETE" => delete(handler),
            other => {
                tracing::warn!(
                    plugin = %menu.plugin,
                    path = %menu.path,
                    method = %other,
                    "skipping api menu entry with an unsupported method"
                );
                continue;
            }
        };

        tracing::debug!(
            plugin = %menu.plugin,
            path = %axum_path,
            method = %method,
            callback = %menu.callback,
            "registered plugin api route"
        );
        router = router.route(&axum_path, route);
    }

    router
}

/// Translate a menu path pattern to axum's syntax: `/x/:id` → `/x/{id}`.
///
/// The registry's own matcher (`menu::registry::match_pattern`) uses the `:name`
/// form, so a plugin author writes one syntax for both.
fn to_axum_path(path: &str) -> String {
    path.split('/')
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => format!("{{{name}}}"),
            None => segment.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether a method changes state, and so needs CSRF protection when the caller
/// is authenticated by an ambient session cookie.
fn is_state_changing(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Serve one plugin-owned request.
#[allow(clippy::too_many_arguments)] // One axum handler; each argument is a distinct extractor.
async fn serve(
    menu: Arc<MenuDefinition>,
    State(state): State<AppState>,
    session: Session,
    bearer: Option<Extension<ApiTokenAuth>>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    method: Method,
    Path(params): Path<HashMap<String, String>>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    // A disabled plugin serves nothing, the same 404 the gated kernel routes
    // give — a route registered at startup must not outlive its plugin.
    if !state.is_plugin_enabled(&menu.plugin) {
        return error_response(StatusCode::NOT_FOUND, "Not found");
    }

    let user = crate::routes::item::get_user_context(&session, &state).await;

    // Permission gate. Empty `permission` means public, matching the registry's
    // convention everywhere else.
    if !menu.permission.is_empty() && !user.has_permission(&menu.permission) && !user.is_admin() {
        let status = if user.authenticated {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        };
        return error_response(status, "Access denied");
    }

    // CSRF, with the bearer exemption argued in the module docs.
    if is_state_changing(&method)
        && bearer.is_none()
        && crate::routes::helpers::require_csrf_header(&session, &headers)
            .await
            .is_err()
    {
        return error_response(
            StatusCode::FORBIDDEN,
            "Invalid or missing CSRF token. Include an X-CSRF-Token header, \
             or authenticate with an Authorization: Bearer token.",
        );
    }

    if body.len() > MAX_REQUEST_BODY {
        return error_response(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
    }
    let Ok(body) = String::from_utf8(body.to_vec()) else {
        return error_response(StatusCode::BAD_REQUEST, "Request body is not valid UTF-8");
    };

    let mut request = ApiRequest::new(
        menu.callback.clone(),
        method.as_str(),
        uri.path(),
        user.id.to_string(),
        user.authenticated,
    );
    request.params = params;
    request.query = query;
    request.body = body;

    let Ok(payload) = serde_json::to_string(&request) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };

    // Dispatch with services attached — this is the whole point. A tap with no
    // DB handle could not write the table this exists to let it write.
    let tap_state = RequestState::new(user, state.tap_services().clone());
    let result = state
        .tap_dispatcher()
        .dispatch_to_plugin("tap_api", &payload, &menu.plugin, tap_state)
        .await;

    match result {
        Some(result) => match parse_plugin_response(&result.output) {
            Some(response) => response,
            None => {
                tracing::warn!(
                    plugin = %menu.plugin,
                    callback = %menu.callback,
                    "tap_api returned an unparseable response"
                );
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "Plugin returned an invalid response",
                )
            }
        },
        None => {
            tracing::warn!(
                plugin = %menu.plugin,
                callback = %menu.callback,
                "tap_api dispatch failed"
            );
            error_response(StatusCode::BAD_GATEWAY, "Plugin handler failed")
        }
    }
}

/// Turn a plugin's `tap_api` output into an HTTP response.
///
/// `#[plugin_tap]` serializes a tap's return with `serde_json::to_string`, so an
/// `ApiResponse` arrives as a JSON object — but a `String`-returning tap would
/// arrive as a JSON *string*, so the same decode the view path needs applies
/// here (G-VIEW-OUTPUT-JSON-ENCODED). Both are accepted.
fn parse_plugin_response(raw: &str) -> Option<Response> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    // A JSON string envelope wrapping the real object.
    let value = match value {
        serde_json::Value::String(ref inner) => {
            serde_json::from_str::<serde_json::Value>(inner).unwrap_or(value)
        }
        other => other,
    };
    let parsed: ApiResponse = serde_json::from_value(value).ok()?;

    // A status outside the valid range is a plugin saying something
    // meaningless; serve 500 rather than silently clamping it to a code the
    // plugin never chose.
    let status = StatusCode::from_u16(parsed.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let content_type = HeaderValue::from_str(&parsed.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/json"));

    let mut response = (status, parsed.body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    Some(response)
}

/// A kernel-authored JSON error, distinct in shape from a plugin's own body.
fn error_response(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn menu(path: &str, method: &str, callback: &str, handler_type: &str) -> MenuDefinition {
        MenuDefinition {
            path: path.to_string(),
            title: "t".into(),
            plugin: "p".into(),
            permission: String::new(),
            callback: callback.to_string(),
            parent: None,
            weight: 0,
            visible: false,
            method: method.to_string(),
            handler_type: handler_type.to_string(),
            local_task: false,
        }
    }

    #[test]
    fn menu_paths_translate_to_axum_syntax() {
        assert_eq!(to_axum_path("/argus/stories"), "/argus/stories");
        assert_eq!(
            to_axum_path("/argus/story/:id/react"),
            "/argus/story/{id}/react"
        );
        assert_eq!(to_axum_path("/a/:x/b/:y"), "/a/{x}/b/{y}");
    }

    #[test]
    fn only_state_changing_methods_need_a_token() {
        assert!(!is_state_changing(&Method::GET));
        assert!(!is_state_changing(&Method::HEAD));
        assert!(!is_state_changing(&Method::OPTIONS));
        assert!(is_state_changing(&Method::POST));
        assert!(is_state_changing(&Method::PUT));
        assert!(is_state_changing(&Method::DELETE));
    }

    #[test]
    fn the_router_registers_only_usable_api_entries() {
        // Building must not panic on any of these, and a duplicate must not
        // reach axum's route table (it panics on a conflict, and a plugin must
        // not be able to take the process down).
        let menus = vec![
            menu("/ok", "POST", "handler", "api"),
            menu("/ok", "POST", "handler", "api"), // duplicate
            menu("/page", "GET", "handler", "page"), // not an api entry
            menu("/nocallback", "POST", "", "api"),
            menu("relative", "POST", "handler", "api"),
            menu("/patch", "PATCH", "handler", "api"), // unsupported method
            menu("/ok", "GET", "handler", "api"),      // same path, other method
        ];
        let _router = build_plugin_api_router(&menus);
    }

    #[test]
    fn a_plugin_response_is_parsed_from_either_envelope() {
        let object = r#"{"status":201,"body":"{\"ok\":true}","content_type":"application/json"}"#;
        let response = parse_plugin_response(object).expect("object envelope");
        assert_eq!(response.status(), StatusCode::CREATED);

        // What a `String`-returning tap would produce.
        let wrapped = serde_json::to_string(object).unwrap();
        let response = parse_plugin_response(&wrapped).expect("string envelope");
        assert_eq!(response.status(), StatusCode::CREATED);

        assert!(parse_plugin_response("not json").is_none());
        assert!(parse_plugin_response(r#"{"nope":1}"#).is_none());
    }

    #[test]
    fn an_out_of_range_status_does_not_panic() {
        let raw = r#"{"status":9999,"body":"","content_type":"application/json"}"#;
        let response = parse_plugin_response(raw).expect("parses");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn a_missing_content_type_defaults_to_json() {
        let raw = r#"{"status":200,"body":"{}"}"#;
        let response = parse_plugin_response(raw).expect("parses");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }
}
