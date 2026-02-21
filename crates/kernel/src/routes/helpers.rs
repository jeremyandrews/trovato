//! Shared route helpers for page rendering.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use tower_sessions::Session;
use uuid::Uuid;

use serde::Deserialize;

use crate::models::{SiteConfig, User};
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

/// Generic form struct for POST endpoints that only need a CSRF token.
///
/// Used by delete, approve, unpublish, and similar action-only endpoints.
#[derive(Debug, Deserialize)]
pub struct CsrfOnlyForm {
    #[serde(rename = "_token")]
    pub token: String,
}

/// Require an authenticated, active user, or redirect to login.
///
/// Returns the [`User`] if one is logged in and active (`status=1`).
/// Destroys the session and redirects to login if the user is blocked.
pub async fn require_login(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    if let Some(id) = user_id
        && let Ok(Some(user)) = User::find_by_id(state.db(), id).await
    {
        if !user.is_active() {
            let _ = session.delete().await;
            return Err(Redirect::to("/user/login").into_response());
        }
        return Ok(user);
    }

    Err(Redirect::to("/user/login").into_response())
}

/// Require an authenticated, active **admin** user, or redirect/reject.
///
/// Returns the admin [`User`] on success. Redirects to `/user/login` if the
/// session has no valid user or the user is blocked. Returns 403 if the user
/// exists and is active but is not an admin.
pub async fn require_admin(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    if let Some(id) = user_id
        && let Ok(Some(user)) = User::find_by_id(state.db(), id).await
    {
        if !user.is_active() {
            let _ = session.delete().await;
            return Err(Redirect::to("/user/login").into_response());
        }
        if user.is_admin {
            return Ok(user);
        }
        return Err((StatusCode::FORBIDDEN, Html("Access denied")).into_response());
    }

    Err(Redirect::to("/user/login").into_response())
}

/// Inject site-wide context variables into a Tera context.
///
/// Adds: `site_name`, `site_slogan`, `menus`, `user_authenticated`, `sidebar_tiles`
///
/// The `path` parameter is the current request path, used for sidebar tile
/// visibility filtering.
pub async fn inject_site_context(
    state: &AppState,
    session: &Session,
    context: &mut tera::Context,
    path: &str,
) {
    // Load site name and slogan in a single query
    let all_config = SiteConfig::all(state.db()).await.unwrap_or_default();
    let site_name = all_config
        .get("site_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Trovato")
        .to_string();
    let site_slogan = all_config
        .get("site_slogan")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    context.insert("site_name", &site_name);
    context.insert("site_slogan", &site_slogan);

    // Public menus sorted by weight
    let mut menus: Vec<_> = state
        .menu_registry()
        .root_menus()
        .into_iter()
        .filter(|m| m.permission.is_empty())
        .cloned()
        .collect();
    menus.sort_by_key(|m| m.weight);
    context.insert("menus", &menus);

    // User authentication status and roles for tile visibility
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    context.insert("user_authenticated", &user_id.is_some());

    // Generate CSRF token for authenticated users (used by logout form in page.html)
    if user_id.is_some() {
        let csrf_token = crate::form::csrf::generate_csrf_token(session)
            .await
            .unwrap_or_default();
        context.insert("csrf_token", &csrf_token);
    }

    let mut user_roles = vec!["anonymous user".to_string()];
    if let Some(id) = user_id {
        user_roles.push("authenticated user".to_string());
        if let Ok(Some(user)) = User::find_by_id(state.db(), id).await
            && user.is_admin
        {
            user_roles.push("administrator".to_string());
        }
    }

    // Load sidebar tiles filtered by request path and user roles
    let sidebar_tiles_html = state
        .tiles()
        .render_region("sidebar", "live", path, &user_roles)
        .await
        .unwrap_or_default();
    context.insert("sidebar_tiles", &sidebar_tiles_html);
}

/// Render an admin template with common context (enabled_plugins).
///
/// This is the shared implementation used by all admin route modules
/// (admin, gather_admin, plugin_admin). The `enabled_plugins` list is
/// sorted for deterministic template output.
pub async fn render_admin_template(
    state: &AppState,
    template: &str,
    mut context: tera::Context,
) -> Response {
    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    context.insert("enabled_plugins", &enabled);
    // Ensure "errors" is always available for form templates that use form::errors().
    if context.get("errors").is_none() {
        let empty: Vec<String> = Vec::new();
        context.insert("errors", &empty);
    }
    match state.theme().tera().render(template, &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template = %template, "failed to render template");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(format!(
                    r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body><h1>Template Error</h1><pre>{}</pre></body></html>"#,
                    html_escape(&e.to_string())
                )),
            )
                .into_response()
        }
    }
}

/// Verify a CSRF token, returning an error response on failure.
///
/// Call sites use this as:
/// ```ignore
/// if let Err(resp) = require_csrf(&session, &form.token).await {
///     return resp;
/// }
/// ```
pub async fn require_csrf(session: &Session, token: &str) -> Result<(), Response> {
    let valid = crate::form::csrf::verify_csrf_token(session, token)
        .await
        .unwrap_or(false);
    if !valid {
        Err(render_error(
            "Invalid or expired form token. Please try again.",
        ))
    } else {
        Ok(())
    }
}

/// Verify a CSRF token from the `X-CSRF-Token` header, returning a JSON error on failure.
///
/// For use with JSON API endpoints that use cookie-based session auth.
/// The client must include the token in a custom header rather than a form field.
pub async fn require_csrf_header(
    session: &Session,
    headers: &axum::http::HeaderMap,
) -> Result<(), (StatusCode, axum::Json<serde_json::Value>)> {
    let token = headers
        .get("X-CSRF-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let valid = crate::form::csrf::verify_csrf_token(session, token)
        .await
        .unwrap_or(false);
    if !valid {
        Err((
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "Invalid or missing CSRF token. Include X-CSRF-Token header."
            })),
        ))
    } else {
        Ok(())
    }
}

/// Validate that a machine name starts with a lowercase letter and contains
/// only lowercase letters, digits, and underscores.
pub fn is_valid_machine_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let mut chars = name.chars();

    // First character must be lowercase letter
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }

    // Rest must be lowercase letters, digits, or underscores
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Render a simple error page with the given message.
///
/// Returns a `400 Bad Request` response with escaped HTML content.
pub fn render_error(message: &str) -> Response {
    let html = format!(
        r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body>
<div style="max-width: 600px; margin: 100px auto; text-align: center;">
<h1>Error</h1>
<p>{}</p>
<p><a href="javascript:history.back()">Go back</a></p>
</div>
</body></html>"#,
        html_escape(message)
    );

    (StatusCode::BAD_REQUEST, Html(html)).into_response()
}

/// Render a simple error page for server-side failures.
///
/// Returns a `500 Internal Server Error` response with escaped HTML content.
pub fn render_server_error(message: &str) -> Response {
    let html = format!(
        r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body>
<div style="max-width: 600px; margin: 100px auto; text-align: center;">
<h1>Error</h1>
<p>{}</p>
<p><a href="javascript:history.back()">Go back</a></p>
</div>
</body></html>"#,
        html_escape(message)
    );

    (StatusCode::INTERNAL_SERVER_ERROR, Html(html)).into_response()
}

/// Render a simple 404 page.
pub fn render_not_found() -> Response {
    let html = r#"<!DOCTYPE html>
<html><head><title>Not Found</title></head>
<body>
<div style="max-width: 600px; margin: 100px auto; text-align: center;">
<h1>Not Found</h1>
<p>The requested page could not be found.</p>
<p><a href="/admin">Return to admin</a></p>
</div>
</body></html>"#;

    (StatusCode::NOT_FOUND, Html(html)).into_response()
}

/// Build local task tab data from the menu registry, merged with hardcoded tabs.
///
/// Looks up plugin-registered local tasks for the given `parent_path` and
/// merges them with `base_tabs`. Plugin tabs are appended after base tabs,
/// sorted by weight. Plugin task paths have `:id` placeholders substituted
/// with `id_value` (if provided) and are marked active when they match
/// `current_path`.
pub fn build_local_tasks(
    state: &crate::state::AppState,
    parent_path: &str,
    current_path: &str,
    id_value: Option<&str>,
    base_tabs: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut tabs = base_tabs;

    // Append plugin-registered local tasks from menu registry
    for task in state.menu_registry().local_tasks(parent_path) {
        let concrete_path = if let Some(id) = id_value {
            task.path.replace(":id", id)
        } else {
            task.path.clone()
        };
        tabs.push(serde_json::json!({
            "title": task.title,
            "path": concrete_path,
            "active": concrete_path == current_path,
        }));
    }

    serde_json::Value::Array(tabs)
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Basic email format validation.
///
/// Checks that the address has the structure `local@domain.tld` where:
/// - Local part is non-empty
/// - Domain contains at least one dot
/// - Domain labels are non-empty
/// - Total length is within RFC 5321 limits (254 chars)
///
/// This is deliberately lenient — full RFC 5322 compliance is not
/// attempted. The goal is to reject obviously invalid addresses while
/// accepting the vast majority of real-world addresses.
pub fn is_valid_email(email: &str) -> bool {
    if email.len() > 254 {
        return false;
    }

    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };

    // Local part must be non-empty and <= 64 chars
    if local.is_empty() || local.len() > 64 {
        return false;
    }

    // Domain must contain at least one dot with non-empty labels
    if !domain.contains('.') {
        return false;
    }

    // All domain labels must be non-empty
    domain.split('.').all(|label| !label.is_empty())
}

/// Basic timezone format validation.
///
/// Accepts IANA timezone identifiers (e.g. `America/New_York`, `UTC`,
/// `Europe/London`) by checking that the value contains only ASCII
/// alphanumeric characters, `/`, `_`, `+`, and `-`.
pub fn is_valid_timezone(tz: &str) -> bool {
    !tz.is_empty()
        && tz.len() <= 64
        && tz
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '+' || c == '-')
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_html_escape_special_chars() {
        assert_eq!(
            html_escape("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
    }

    #[test]
    fn test_html_escape_ampersand() {
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn test_html_escape_quotes() {
        assert_eq!(html_escape(r#"say "hello""#), "say &quot;hello&quot;");
    }

    #[test]
    fn test_html_escape_plain_text() {
        assert_eq!(html_escape("hello world"), "hello world");
    }

    #[test]
    fn test_html_escape_empty() {
        assert_eq!(html_escape(""), "");
    }

    #[test]
    fn test_is_valid_email_valid() {
        assert!(is_valid_email("user@example.com"));
        assert!(is_valid_email("user@sub.domain.co.uk"));
        assert!(is_valid_email("a@b.c"));
        assert!(is_valid_email("user+tag@example.com"));
    }

    #[test]
    fn test_is_valid_email_invalid() {
        assert!(!is_valid_email(""));
        assert!(!is_valid_email("user"));
        assert!(!is_valid_email("@."));
        assert!(!is_valid_email(".@"));
        assert!(!is_valid_email("user@"));
        assert!(!is_valid_email("@domain.com"));
        assert!(!is_valid_email("user@domain"));
        assert!(!is_valid_email("user@.com"));
        assert!(!is_valid_email("user@domain."));
        assert!(!is_valid_email("user@domain..com"));
    }

    #[test]
    fn test_is_valid_timezone_valid() {
        assert!(is_valid_timezone("UTC"));
        assert!(is_valid_timezone("America/New_York"));
        assert!(is_valid_timezone("Europe/London"));
        assert!(is_valid_timezone("Etc/GMT+5"));
        assert!(is_valid_timezone("US/Eastern"));
    }

    #[test]
    fn test_is_valid_timezone_invalid() {
        assert!(!is_valid_timezone(""));
        assert!(!is_valid_timezone("America/New York"));
        assert!(!is_valid_timezone("<script>"));
        assert!(!is_valid_timezone("a".repeat(65).as_str()));
    }
}
