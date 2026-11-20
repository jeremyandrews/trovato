//! The viewer as templates see it.

use serde::Serialize;

use crate::tap::UserContext;

/// Who is looking at the page, as every template receives it under `viewer`.
///
/// Exactly three facts, and the only viewer data a template gets: enough to
/// link to the viewer's own pages (`/user/{{ viewer.id }}/...`) and to draw a
/// per-viewer control, and nothing a template could leak. No email, no display
/// name, no permissions, no session or token: a theme is not a place to make an
/// access decision, and what it never receives it cannot print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Viewer {
    /// The user's id as a string; `None` (`null` in a template) when anonymous.
    pub id: Option<String>,
    /// Whether the viewer is signed in.
    pub authenticated: bool,
    /// The names of the viewer's roles, sorted.
    pub roles: Vec<String>,
}

impl Viewer {
    /// The template viewer for a request's [`UserContext`].
    ///
    /// The one builder: every template context gets its `viewer` from here.
    pub fn of(user: &UserContext) -> Self {
        let mut roles = user.roles().to_vec();
        roles.sort();
        Self {
            id: user.authenticated.then(|| user.id.to_string()),
            authenticated: user.authenticated,
            roles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn an_anonymous_viewer_has_no_id() {
        let viewer = Viewer::of(&UserContext::anonymous());
        assert_eq!(
            serde_json::to_value(&viewer).ok(),
            Some(
                serde_json::json!({"id": null, "authenticated": false, "roles": ["anonymous user"]})
            )
        );
    }

    #[test]
    fn a_signed_in_viewer_has_their_id_and_sorted_roles() {
        let id = Uuid::now_v7();
        let user = UserContext::authenticated(id, vec!["access content".to_string()])
            .with_roles(vec!["zeta".to_string(), "authenticated user".to_string()]);
        let viewer = Viewer::of(&user);
        assert_eq!(viewer.id, Some(id.to_string()));
        assert!(viewer.authenticated);
        assert_eq!(viewer.roles, ["authenticated user", "zeta"]);
    }
}
