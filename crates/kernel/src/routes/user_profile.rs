//! Account pages: a user's public profile, and "My account" (B156).
//!
//! `/user/{id}` and `/users/{name}` show an account to whoever holds `access
//! user profiles` (or `administer users`), and to its own owner: the name as
//! the page title and "Member for", the History a Drupal account page has.
//! `/user` is the signed-in user's own page; the edit form stays at
//! `/user/profile`, linked from it as a tab.
//!
//! A refusal never tells an account apart from no account: a viewer without
//! the permission is refused before anything is looked up, and an account
//! that is blocked answers `404`, as a missing one does, to everyone but an
//! administrator of users.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use tower_sessions::Session;

use crate::models::User;
use crate::models::user::ANONYMOUS_USER_ID;
use crate::state::AppState;
use crate::tap::UserContext;

use super::helpers::{
    get_user_context, inject_site_context, render_forbidden, render_not_found, render_server_error,
};

/// The permission that lets a visitor read other users' profiles.
pub const ACCESS_USER_PROFILES: &str = "access user profiles";

/// GET /user: the signed-in user's own account page; the login form for
/// nobody, as Drupal's `/user` is.
async fn my_account(State(state): State<AppState>, session: Session) -> Response {
    match super::helpers::session_user(&state, &session).await {
        Some(me) => {
            let viewer = get_user_context(&session, &state).await;
            render_account(&state, &session, &viewer, &me).await
        }
        None => Redirect::to("/user/login").into_response(),
    }
}

/// GET /user/{id}: an account by its id.
async fn profile_by_id(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    let viewer = get_user_context(&session, &state).await;
    let Ok(id) = uuid::Uuid::parse_str(&id) else {
        return render_not_found();
    };
    if let Err(refusal) = may_read_profiles(&viewer, id) {
        return refusal;
    }
    let target = state.users().find_by_id(id).await.ok().flatten();
    show(&state, &session, &viewer, target).await
}

/// GET /users/{name}: an account by its name, Drupal's `users/[user:name]`.
async fn profile_by_name(
    State(state): State<AppState>,
    session: Session,
    Path(name): Path<String>,
) -> Response {
    let viewer = get_user_context(&session, &state).await;
    let target = state.users().find_by_name(&name).await.ok().flatten();
    // The permission is asked before the answer can depend on the account: a
    // viewer without it is refused whether or not the name exists.
    let own = target.as_ref().is_some_and(|t| t.id == viewer.id);
    if !own && let Err(refusal) = may_read_profiles(&viewer, ANONYMOUS_USER_ID) {
        return refusal;
    }
    show(&state, &session, &viewer, target).await
}

/// Whether `viewer` may read profiles at all (their own always).
#[allow(clippy::result_large_err)]
fn may_read_profiles(viewer: &UserContext, target: uuid::Uuid) -> Result<(), Response> {
    if (viewer.authenticated && viewer.id == target)
        || viewer.is_admin()
        || viewer.has_permission(ACCESS_USER_PROFILES)
        || viewer.has_permission("administer users")
    {
        Ok(())
    } else {
        Err(render_forbidden(
            "You are not authorized to access this page.",
        ))
    }
}

/// The page for `target`, or 404 when there is none this viewer may see.
async fn show(
    state: &AppState,
    session: &Session,
    viewer: &UserContext,
    target: Option<User>,
) -> Response {
    let Some(target) = target.filter(|t| t.id != ANONYMOUS_USER_ID) else {
        return render_not_found();
    };
    let own = viewer.authenticated && viewer.id == target.id;
    let administers = viewer.is_admin() || viewer.has_permission("administer users");
    if !target.is_active() && !own && !administers {
        return render_not_found();
    }
    render_account(state, session, viewer, &target).await
}

/// Render the account page for `target` as `viewer` sees it.
async fn render_account(
    state: &AppState,
    session: &Session,
    viewer: &UserContext,
    target: &User,
) -> Response {
    let own = viewer.authenticated && viewer.id == target.id;
    let administers = viewer.is_admin() || viewer.has_permission("administer users");
    let edit_url = if own {
        Some("/user/profile".to_string())
    } else if administers {
        Some(format!("/admin/people/{}/edit", target.id))
    } else {
        None
    };

    let mut context = tera::Context::new();
    context.insert(
        "account",
        &serde_json::json!({
            "id": target.id,
            "name": target.name,
            "member_for": member_for(target.created, chrono::Utc::now()),
            "created": target.created.timestamp(),
            "blocked": !target.is_active(),
        }),
    );
    context.insert("own", &own);
    context.insert("edit_url", &edit_url);
    context.insert("view_url", &format!("/user/{}", target.id));
    inject_site_context(
        state,
        session,
        &mut context,
        &format!("/user/{}", target.id),
    )
    .await;

    match state.theme().tera().render("user/view.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the account page");
            render_server_error("The account page could not be displayed.")
        }
    }
}

/// How long an account has existed, as Drupal's "Member for" says it: the
/// two largest units, "3 years 2 months", "5 days 4 hours", or "1 minute".
pub fn member_for(
    created: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let seconds = (now - created).num_seconds().max(0);
    const UNITS: [(&str, &str, i64); 7] = [
        ("year", "years", 365 * 86_400),
        ("month", "months", 30 * 86_400),
        ("week", "weeks", 7 * 86_400),
        ("day", "days", 86_400),
        ("hour", "hours", 3_600),
        ("minute", "minutes", 60),
        ("second", "seconds", 1),
    ];
    let mut left = seconds;
    let mut parts = Vec::new();
    for (one, many, size) in UNITS {
        let count = left / size;
        if count > 0 {
            parts.push(format!("{count} {}", if count == 1 { one } else { many }));
            left -= count * size;
            if parts.len() == 2 {
                break;
            }
        } else if !parts.is_empty() {
            break;
        }
    }
    if parts.is_empty() {
        "0 seconds".to_string()
    } else {
        parts.join(" ")
    }
}

/// The account page routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user", get(my_account))
        .route("/user/{id}", get(profile_by_id))
        .route("/users/{name}", get(profile_by_name))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn member_for_names_the_two_largest_units() {
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 11, 23, 12, 0, 0)
            .unwrap();
        let at = |y, m, d, h| chrono::Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap();
        // No level is skipped: twenty years and some days is "20 years".
        assert_eq!(member_for(at(2006, 11, 23, 12), now), "20 years");
        assert_eq!(member_for(at(2026, 9, 10, 12), now), "2 months 2 weeks");
        assert_eq!(member_for(at(2026, 11, 18, 8), now), "5 days 4 hours");
        assert_eq!(member_for(now, now), "0 seconds");
        assert_eq!(
            member_for(now - chrono::Duration::seconds(61), now),
            "1 minute 1 second"
        );
    }
}
