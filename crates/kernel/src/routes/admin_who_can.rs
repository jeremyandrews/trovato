//! "Who can do this?": for an admin page, the permission it needs and the
//! roles that hold it.
//!
//! Each admin page links here with its own path
//! (`/admin/people/who-can?path=/admin/structure/menus`). The answer is
//! worked out again from the path, with the registry's gate for that page
//! (`admin_nav::placement`), so nothing in the address can change it. Only
//! someone who may change who can do what (the Permissions page's
//! permission) sees the link or the page.
//!
//! A role's row names its members (B252), up to 20, each linked to their
//! account, then a link to the rest ("and N more") on the People listing
//! filtered to that role — but only to a viewer who may open the People
//! listing themselves ([`may_see_names`]); anyone else keeps the member
//! count the page always showed. Every role's names and count come from one
//! query, not one per role.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::models::role::well_known;
use crate::state::AppState;
use crate::tap::UserContext;

use super::helpers::{render_admin_template, require_permission};

/// The permission the page and its link need: the Permissions page's own.
pub const PERMISSION: &str = "administer users";

/// This page's address.
pub const PATH: &str = "/admin/people/who-can";

/// Most member names a role's row lists before "and N more".
const MAX_NAMES: i64 = 20;

/// Whether `viewer` may open the People listing: the page whose own gate
/// decides whether member names, rather than only a count, are safe to show.
/// Read from the registry rather than assumed equal to this page's own gate,
/// so the two stay independent if either's permission ever changes.
fn may_see_names(viewer: &UserContext) -> bool {
    viewer.is_admin()
        || super::admin_nav::LINKS
            .iter()
            .find(|l| l.href == "/admin/people")
            .is_some_and(|l| l.gate.permissions().iter().any(|p| viewer.can(p)))
}

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
    /// Up to [`MAX_NAMES`] members, alphabetically; empty unless the viewer
    /// may see names ([`may_see_names`]).
    names: Vec<MemberName>,
    /// How many members beyond `names`; `0` when all of them are listed.
    more: i64,
}

/// One role member, as the page links to their account.
#[derive(Debug, Serialize)]
struct MemberName {
    id: uuid::Uuid,
    name: String,
}

/// Up to [`MAX_NAMES`] members and the total membership of every role in
/// `role_ids`, in one query: a role with no members is simply absent from
/// the result (a user without matching rows returns none).
async fn member_names(
    state: &AppState,
    role_ids: &[uuid::Uuid],
) -> std::collections::HashMap<uuid::Uuid, (Vec<MemberName>, i64)> {
    if role_ids.is_empty() {
        return std::collections::HashMap::new();
    }
    let rows: Vec<(uuid::Uuid, uuid::Uuid, String, i64)> = sqlx::query_as(
        r#"
        SELECT role_id, user_id, user_name, total FROM (
            SELECT ur.role_id, u.id AS user_id, u.name AS user_name,
                   COUNT(*) OVER (PARTITION BY ur.role_id) AS total,
                   ROW_NUMBER() OVER (
                       PARTITION BY ur.role_id ORDER BY lower(u.name), u.id
                   ) AS rn
            FROM user_roles ur
            JOIN users u ON u.id = ur.user_id
            WHERE ur.role_id = ANY($1)
        ) ranked
        WHERE rn <= $2
        ORDER BY role_id, rn
        "#,
    )
    .bind(role_ids)
    .bind(MAX_NAMES)
    .fetch_all(state.db())
    .await
    .unwrap_or_default();

    let mut by_role: std::collections::HashMap<uuid::Uuid, (Vec<MemberName>, i64)> =
        std::collections::HashMap::new();
    for (role_id, user_id, user_name, total) in rows {
        let entry = by_role
            .entry(role_id)
            .or_insert_with(|| (Vec::new(), total));
        entry.0.push(MemberName {
            id: user_id,
            name: user_name,
        });
    }
    by_role
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
    let show_names = may_see_names(&viewer);

    let mut context = tera::Context::new();
    context.insert("path", PATH);
    context.insert("show_names", &show_names);
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
                rows.push(RoleRow {
                    id,
                    name,
                    holds: vec![permission],
                    everyone,
                    members: 0,
                    names: Vec::new(),
                    more: 0,
                });
            }

            // Every role's members and names, in one query: `everyone` rows
            // (anonymous, authenticated) hold no specific membership, so stay
            // at the zero they were built with.
            let role_ids: Vec<uuid::Uuid> = rows
                .iter()
                .filter(|r| r.everyone.is_none())
                .map(|r| r.id)
                .collect();
            let mut found = member_names(&state, &role_ids).await;
            for row in &mut rows {
                if row.everyone.is_some() {
                    continue;
                }
                let (names, total) = found.remove(&row.id).unwrap_or_default();
                row.members = total;
                if show_names {
                    row.more = (total - MAX_NAMES).max(0);
                    row.names = names;
                }
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

    /// A site administrator, and anyone holding the People listing's own
    /// permission, may see names; everyone else, including a viewer who
    /// holds some other admin permission, keeps only the count.
    #[test]
    fn names_are_shown_only_to_a_viewer_who_may_open_the_people_listing() {
        let id = uuid::Uuid::now_v7();
        assert!(may_see_names(&UserContext::administrator(id, Vec::new())));
        assert!(may_see_names(&UserContext::authenticated(
            id,
            vec!["administer users".to_string()]
        )));
        assert!(!may_see_names(&UserContext::authenticated(
            id,
            vec!["administer site".to_string()]
        )));
        assert!(!may_see_names(&UserContext::authenticated(id, Vec::new())));
    }
}
