//! Shared route helpers for public page rendering.

use tower_sessions::Session;
use uuid::Uuid;

use crate::models::SiteConfig;
use crate::state::AppState;

/// Session key for user ID.
const SESSION_USER_ID: &str = "user_id";

/// Inject site-wide context variables into a Tera context.
///
/// Adds: `site_name`, `site_slogan`, `menus`, `user_authenticated`
pub async fn inject_site_context(
    state: &AppState,
    session: &Session,
    context: &mut tera::Context,
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

    // User authentication status
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    context.insert("user_authenticated", &user_id.is_some());
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
