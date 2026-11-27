//! The performance page: clear every cache from the admin (B161).
//!
//! `GET /admin/config/performance` says what the caches are and offers one
//! button, "Clear all caches"; `POST` runs the same clear as
//! `trovato cache clear` ([`crate::cache::clear_everywhere`] over every
//! [`CacheKind`]), so every running server drops its in-memory copies and the
//! next request reads the database. Before this the clear was on the command
//! line only, and an administrator without a shell had no way to see a change
//! made behind the servers' backs before the copies expired.
//!
//! Both take `clear caches` or `administer site`; the POST goes through the
//! forms pipeline, so it needs the page's single use token.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use tower_sessions::Session;

use crate::cache::generation::CacheKind;
use crate::content::submission::PostedForm;
use crate::form::{FormElement, FormState, ProcessError, ValidationError};
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_error, render_server_error, require_any_permission,
};

/// The page's address.
pub const PATH: &str = "/admin/config/performance";

/// The CDN purge form's own address.
const PURGE_PATH: &str = "/admin/config/performance/purge";

/// Either opens the page and runs the clear or the purge.
pub const PERMISSIONS: &[&str] = &["clear caches", "administer site"];

/// Session key for the page's one flash message, after a clear or a purge.
const FLASH_KEY: &str = "performance_flash";

/// What the page says after a clear that worked.
pub const CLEARED: &str = "Caches cleared.";

/// What the page says after a purge reached a plugin.
pub const PURGE_SENT: &str = "Purge sent to the CDN.";

/// What the page says when nothing could send the purge.
pub const PURGE_NO_PLUGIN: &str =
    "No CDN purge plugin is enabled, so nothing was sent. The purge will apply on its own once s-maxage runs out.";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(PATH, get(performance_page).post(clear_caches))
        .route(PURGE_PATH, axum::routing::post(purge_cdn))
}

/// The Clear all caches form, through the forms pipeline.
fn clear_form() -> crate::form::Form {
    crate::form::Form::new("cache_clear_form")
        .action(PATH)
        .element("op", FormElement::submit("Clear all caches").weight(0))
}

/// The Purge the CDN form: everything, or a hand-picked list of tags.
/// `scope_default` and `tags_default` are what the fields show: `"all"` and
/// empty on first load, whatever was posted when redisplaying after a
/// validation error.
fn purge_form(scope_default: &str, tags_default: &str) -> crate::form::Form {
    crate::form::Form::new("cdn_purge_form")
        .action(PURGE_PATH)
        .element(
            "scope",
            FormElement::radio(vec![
                ("all".to_string(), "Everything".to_string()),
                ("tags".to_string(), "These tags".to_string()),
            ])
            .title("What to purge")
            .default_value(if scope_default.is_empty() {
                "all"
            } else {
                scope_default
            })
            .required()
            .weight(0),
        )
        .element(
            "tags",
            FormElement::textarea(6)
                .title("Tags")
                .description(
                    "One tag per line, used only when \"These tags\" is chosen above \
                     (for example item:0199.. or site). At most 500.",
                )
                .default_value(tags_default.to_string())
                .weight(1),
        )
        .element("op", FormElement::submit("Purge the CDN").weight(10))
}

/// Parse the purge form's posted `scope` and `tags` into what to purge:
/// `Ok(None)` for everything, `Ok(Some(tags))` for a checked tag list.
fn read_purge(posted: &PostedForm) -> Result<Option<Vec<String>>, Vec<ValidationError>> {
    let scope = posted.get("scope").unwrap_or_default();
    if scope == "all" {
        return Ok(None);
    }
    if scope != "tags" {
        return Err(vec![ValidationError::field(
            "scope",
            "Choose what to purge.",
        )]);
    }
    let tags: Vec<String> = posted
        .get("tags")
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if tags.is_empty() {
        return Err(vec![ValidationError::field(
            "tags",
            "Enter at least one tag, or choose Everything.",
        )]);
    }
    if tags.len() > crate::cache::tags::MAX_MANUAL_TAGS {
        return Err(vec![ValidationError::field(
            "tags",
            format!(
                "At most {} tags at a time.",
                crate::cache::tags::MAX_MANUAL_TAGS
            ),
        )]);
    }
    if let Some(bad) = tags.iter().find(|t| !crate::cache::tags::is_valid_tag(t)) {
        return Err(vec![ValidationError::field(
            "tags",
            format!("\"{bad}\" is not a valid tag (letters, digits, : _ and - only)."),
        )]);
    }
    Ok(Some(tags))
}

/// GET /admin/config/performance
async fn performance_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_any_permission(&state, &session, PERMISSIONS).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    render_page(&state, &session, &user, flash, "all", "", &[]).await
}

/// Build and render the whole page: the clear-cache form, the CDN-purge
/// form (pre-filled with `scope_default`/`tags_default` and showing
/// `purge_errors`, both empty on a plain `GET`), and the "what is cleared"
/// list.
#[allow(clippy::too_many_arguments)]
async fn render_page(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    flash: Option<String>,
    scope_default: &str,
    tags_default: &str,
    purge_errors: &[ValidationError],
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, PATH).await;

    let mut form_state = FormState::new("cache_clear_form", "");
    form_state.user_id = Some(user.id);
    let form_html = match state
        .forms()
        .build(
            clear_form(),
            session,
            &super::item_form::tap_state(state, &viewer),
            form_state,
        )
        .await
        .and_then(|form| text.scope(|| state.theme().render_form(&form)))
    {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the cache clear form");
            return render_server_error("The performance page could not be displayed.");
        }
    };

    let mut purge_form_state = FormState::new("cdn_purge_form", "");
    purge_form_state.user_id = Some(user.id);
    let purge_form_html = match state
        .forms()
        .build(
            purge_form(scope_default, tags_default),
            session,
            &super::item_form::tap_state(state, &viewer),
            purge_form_state,
        )
        .await
        .and_then(|form| text.scope(|| state.theme().render_form_with_errors(&form, purge_errors)))
    {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the CDN purge form");
            return render_server_error("The performance page could not be displayed.");
        }
    };

    let kinds: Vec<&str> = CacheKind::ALL.iter().map(|k| k.as_str()).collect();

    let mut context = tera::Context::new();
    context.insert("clear_form", &form_html);
    context.insert("purge_form", &purge_form_html);
    context.insert("kinds", &kinds);
    context.insert("flash", &flash);
    context.insert("path", PATH);
    render_admin_template(state, &viewer, "admin/config/performance.html", context).await
}

/// POST /admin/config/performance: clear every cache on every server, then
/// back to the page.
async fn clear_caches(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_any_permission(&state, &session, PERMISSIONS).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    match state
        .forms()
        .process(
            posted.get("_form_build_id").unwrap_or_default(),
            &posted,
            &session,
            &super::item_form::tap_state(&state, &viewer),
            |form_state| {
                if form_state.form_id == "cache_clear_form" {
                    Ok(())
                } else {
                    Err(vec![crate::form::ValidationError::form(
                        crate::form::FORM_EXPIRED,
                    )])
                }
            },
        )
        .await
    {
        Ok(_) => {}
        Err(ProcessError::Csrf) => {
            return render_error("Invalid or expired form token. Please try again.");
        }
        Err(ProcessError::Invalid(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
    }

    tracing::info!(user_id = %user.id, "all caches cleared from the admin");
    // The command line's one path: bump every kind (each server, this one
    // included, drops its copies when it reads the new counter) and wait for
    // the bump to settle, so the page the browser is sent back to is already
    // read from the database.
    let message = match crate::cache::clear_everywhere(state.redis(), &CacheKind::ALL).await {
        Ok(()) => CLEARED.to_string(),
        Err(e) => {
            tracing::error!(error = %e, "cache clear from the admin failed");
            // This server's own copies can still go.
            state.clear_local_caches().await;
            "Only this server's caches were cleared: the cache server (Redis) could not be \
             reached, so other servers keep their copies until they expire."
                .to_string()
        }
    };
    let _ = session.insert(FLASH_KEY, message).await;
    Redirect::to(PATH).into_response()
}

/// POST /admin/config/performance/purge: record the purge (everything, or
/// the hand-picked tags), drain it at once, and say what happened.
async fn purge_cdn(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_any_permission(&state, &session, PERMISSIONS).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let scope = posted.get("scope").unwrap_or_default().to_string();
    let tags_text = posted.get("tags").unwrap_or_default().to_string();

    let parsed = read_purge(&posted);
    let extra_errors = parsed.clone().err().unwrap_or_default();
    match super::admin_form::check(&state, &session, &user, &posted, "cdn_purge_form", extra_errors).await
    {
        Ok(()) => {}
        Err(Ok(errors)) => {
            let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
            return render_page(&state, &session, &user, flash, &scope, &tags_text, &errors).await;
        }
        Err(Err(response)) => return response,
    }

    // `check` above only let this through when `read_purge` found no
    // problems, so this is always `Ok`.
    let target = parsed.unwrap_or_default();
    let everything = target.is_none();
    match &target {
        None => crate::cache::purge::purge_all(),
        Some(tags) => {
            let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
            crate::cache::purge::purge(&refs);
        }
    }

    let dispatched = state.cron().drain_cache_purge_now().await;
    tracing::info!(
        user_id = %user.id,
        everything = everything,
        dispatched = dispatched,
        "CDN purge requested from the admin"
    );
    let message = if dispatched {
        PURGE_SENT.to_string()
    } else {
        PURGE_NO_PLUGIN.to_string()
    };
    let _ = session.insert(FLASH_KEY, message).await;
    Redirect::to(PATH).into_response()
}
