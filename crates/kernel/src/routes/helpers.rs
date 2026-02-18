//! Shared route helpers for page rendering.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use tower_sessions::Session;
use uuid::Uuid;

use crate::models::{SiteConfig, User};
use crate::state::AppState;

/// Session key for user ID.
const SESSION_USER_ID: &str = "user_id";

/// Require an authenticated user, or redirect to login.
///
/// Returns the [`User`] if one is logged in. Returns a redirect response if the
/// session contains no valid user id.
pub async fn require_login(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    if let Some(id) = user_id {
        if let Ok(Some(user)) = User::find_by_id(state.db(), id).await {
            return Ok(user);
        }
    }

    Err(Redirect::to("/user/login").into_response())
}

/// Require an authenticated **admin** user, or redirect/reject.
///
/// Returns the admin [`User`] on success. Redirects to `/user/login` if the
/// session has no valid user. Returns 403 if the user exists but is not an admin.
pub async fn require_admin(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    if let Some(id) = user_id {
        if let Ok(Some(user)) = User::find_by_id(state.db(), id).await {
            if user.is_admin {
                return Ok(user);
            }
            return Err((StatusCode::FORBIDDEN, Html("Access denied")).into_response());
        }
    }

    Err(Redirect::to("/user/login").into_response())
}

/// Inject site-wide context variables into a Tera context.
///
/// Adds: `site_name`, `site_slogan`, `menus`, `user_authenticated`
pub async fn inject_site_context(state: &AppState, session: &Session, context: &mut tera::Context) {
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

    // User authentication status
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    context.insert("user_authenticated", &user_id.is_some());
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

/// HTML-escape a string for safe output.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
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
