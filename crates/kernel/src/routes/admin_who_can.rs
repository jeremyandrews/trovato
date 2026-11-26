//! "Who can do this?": for an admin page, the permission it needs and the
//! roles that hold it.
//!
//! Each admin page links here with its own path
//! (`/admin/people/who-can?path=/admin/structure/menus`). The answer is
//! worked out again from the path, with the registry's gate for that page
//! (`admin_nav::placement`), so nothing in the address can change it. Only
//! someone who may change who can do what (the Permissions page's
//! permission) sees the link or the page.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::models::role::well_known;
use crate::state::AppState;

use super::helpers::{render_admin_template, require_permission};

/// The permission the page and its link need: the Permissions page's own.
pub const PERMISSION: &str = "administer users";

/// This page's address.
pub const PATH: &str = "/admin/people/who-can";

pub fn router() -> Router<AppState> {
    Router::new().route(PATH, get(who_can))
}

#[derive(Debug, Deserialize)]
struct WhoCanQuery {
    #[serde(default)]
    path: String,
}

/// One role that holds a permission the page needs.
#[derive(Debug, Serialize)]
struct RoleRow {
    id: uuid::Uuid,
    name: String,
    /// Which of the page's permissions the role holds.
    holds: Vec<String>,
    /// Everyone signed in, or everyone at all.
    everyone: Option<&'static str>,
    members: i64,
}

/// One page in the overview: its name, address, group and permissions.
#[derive(Debug, Serialize)]
struct PageRow {
    label: String,
    href: String,
    group: Option<String>,
    /// Empty: site administrators only.
    permissions: Vec<String>,
}

/// Every page the registry knows, with what opens it: the kernel's, then
/// the plugins'.
fn every_page(state: &AppState) -> Vec<PageRow> {
    let nav = state.plugin_admin_nav();
    let mut rows: Vec<PageRow> = super::admin_nav::LINKS
        .iter()
        .map(|l| PageRow {
            label: l.label.to_string(),
            href: l.href.to_string(),
            group: l.group.map(|g| g.info().title.to_string()),
            permissions: l
                .gate
                .permissions()
                .into_iter()
                .map(str::to_string)
                .collect(),
        })
        .collect();
    rows.extend(nav.links().iter().map(|l| {
        PageRow {
            label: l.label.clone(),
            href: l.href.clone(),
            group: Some(
                super::admin_nav::group_for_section(&l.section)
                    .info()
                    .title
                    .to_string(),
            ),
            permissions: vec![l.permission.clone()],
        }
    }));
    rows
}

/// The path, if it is an admin page's: it must be `/admin` or below, with
/// no query, fragment, empty or dot segments.
fn admin_path(raw: &str) -> Option<String> {
    let path = raw.trim();
    let under_admin = path == "/admin" || path.starts_with("/admin/");
    let clean = !path.contains(['?', '#', '\\'])
        && path
            .split('/')
            .skip(1)
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    (under_admin && clean && path.len() <= 300).then(|| path.to_string())
}

/// GET /admin/people/who-can?path=
async fn who_can(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<WhoCanQuery>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let nav = state.plugin_admin_nav();
    let place = admin_path(&query.path)
        .map(|path| {
            (
                super::admin_nav::placement(&path, nav.links(), nav.cards()),
                path,
            )
        })
        .filter(|(place, _)| place.known);

    let mut context = tera::Context::new();
    context.insert("path", PATH);
    if let Some((place, path)) = place {
        let mut rows = Vec::new();
        if !place.permissions.is_empty() {
            let held: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
                "SELECT r.id, r.name, rp.permission FROM roles r \
                 JOIN role_permissions rp ON rp.role_id = r.id \
                 WHERE rp.permission = ANY($1) ORDER BY lower(r.name), rp.permission",
            )
            .bind(&place.permissions)
            .fetch_all(state.db())
            .await
            .unwrap_or_default();
            for (id, name, permission) in held {
                if let Some(row) = rows.iter_mut().find(|r: &&mut RoleRow| r.id == id) {
                    row.holds.push(permission);
                    continue;
                }
                let everyone = if id == well_known::ANONYMOUS_ROLE_ID {
                    Some("Everyone, signed in or not")
                } else if id == well_known::AUTHENTICATED_ROLE_ID {
                    Some("Everyone who is signed in")
                } else {
                    None
                };
                let members = if everyone.is_some() {
                    0
                } else {
                    crate::models::Role::member_count(state.db(), id)
                        .await
                        .unwrap_or(0)
                };
                rows.push(RoleRow {
                    id,
                    name,
                    holds: vec![permission],
                    everyone,
                    members,
                });
            }
        }
        context.insert("target_path", &path);
        context.insert("entry", &place.entry);
        context.insert("permissions", &place.permissions);
        context.insert("roles", &rows);
    } else {
        context.insert("target_path", &None::<String>);
        context.insert("asked", &!query.path.trim().is_empty());
        context.insert("pages", &every_page(&state));
    }
    render_admin_template(&state, &viewer, "admin/who-can.html", context).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_clean_admin_path_is_accepted() {
        assert_eq!(admin_path("/admin").as_deref(), Some("/admin"));
        assert_eq!(
            admin_path(" /admin/structure/menus ").as_deref(),
            Some("/admin/structure/menus")
        );
        for bad in [
            "",
            "/",
            "/administrator",
            "/user/1",
            "/admin/../user",
            "/admin//x",
            "/admin/x?y=1",
            "/admin/x#y",
            "https://example.com/admin",
            "/admin/./x",
            "/admin\\x",
        ] {
            assert_eq!(admin_path(bad), None, "{bad}");
        }
    }
}
