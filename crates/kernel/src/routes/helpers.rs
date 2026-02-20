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

/// Require an authenticated user, or redirect to login.
///
/// Returns the [`User`] if one is logged in. Returns a redirect response if the
/// session contains no valid user id.
pub async fn require_login(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    if let Some(id) = user_id
        && let Ok(Some(user)) = User::find_by_id(state.db(), id).await
    {
        return Ok(user);
    }

    Err(Redirect::to("/user/login").into_response())
}

/// Require an authenticated **admin** user, or redirect/reject.
///
/// Returns the admin [`User`] on success. Redirects to `/user/login` if the
/// session has no valid user. Returns 403 if the user exists but is not an admin.
pub async fn require_admin(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    if let Some(id) = user_id
        && let Ok(Some(user)) = User::find_by_id(state.db(), id).await
    {
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

/// HTML-escape a string for safe output.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
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
}
