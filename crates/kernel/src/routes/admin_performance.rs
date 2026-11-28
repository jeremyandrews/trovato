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
use crate::edge_cache::DrainOutcome;
use crate::form::{FormElement, FormState, ProcessError, ValidationError};
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_error, render_server_error, require_any_permission,
};

/// The page's address.
pub const PATH: &str = "/admin/config/performance";

/// Either opens the page and runs the clear.
pub const PERMISSIONS: &[&str] = &["clear caches", "administer site"];

/// Session key for the message after a clear or a purge: one key, since only
/// one of the page's two forms is ever submitted at a time, and the base
/// admin template already renders either a plain string or `{kind: "error",
/// message}` from it (see `page--admin.html`).
const FLASH_KEY: &str = "performance_flash";

/// What the page says after a clear that worked.
pub const CLEARED: &str = "Caches cleared.";

/// At most this many tags a manual purge may name (B303).
const MAX_PURGE_TAGS: usize = 500;

pub fn router() -> Router<AppState> {
    Router::new().route(PATH, get(performance_page).post(handle_post))
}

/// The Clear all caches form, through the forms pipeline.
fn clear_form() -> crate::form::Form {
    crate::form::Form::new("cache_clear_form")
        .action(PATH)
        .element("op", FormElement::submit("Clear all caches").weight(0))
}

/// The Purge the CDN form (B303), through the forms pipeline.
fn purge_form() -> crate::form::Form {
    crate::form::Form::new("cdn_purge_form")
        .action(PATH)
        .element(
            "scope",
            FormElement::radio(vec![
                ("tags".to_string(), "These tags".to_string()),
                ("everything".to_string(), "Everything".to_string()),
            ])
            .title("What to purge")
            .default_value("tags")
            .weight(0),
        )
        .element(
            "tags",
            FormElement::textarea(6)
                .title("Tags")
                .description(
                    "One tag per line, for example item:0b6b3b8e-... or items or site. \
                     At most 500. Ignored when Everything is chosen.",
                )
                .weight(1),
        )
        .element("op", FormElement::submit("Purge the CDN").weight(2))
}

/// Whether `tag` is in the cache tag vocabulary's character set: the three
/// kinds are `site`, `items` and `item:<uuid>` (see `crate::edge_cache::tags`).
fn is_valid_purge_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 200
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'))
}

/// What a validated POST asked for.
enum PostOutcome {
    ClearCaches,
    Purge(PurgeRequest),
}

/// What the purge form asked to purge.
enum PurgeRequest {
    Everything,
    Tags(Vec<String>),
}

/// GET /admin/config/performance
async fn performance_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_any_permission(&state, &session, PERMISSIONS).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let text = super::admin_nav::AdminText::for_page(&state, &viewer, PATH).await;
    let mut form_state = FormState::new("cache_clear_form", "");
    form_state.user_id = Some(user.id);
    let form_html = match state
        .forms()
        .build(
            clear_form(),
            &session,
            &super::item_form::tap_state(&state, &viewer),
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

    let purge_form_html = match state
        .forms()
        .build(
            purge_form(),
            &session,
            &super::item_form::tap_state(&state, &viewer),
            {
                let mut s = FormState::new("cdn_purge_form", "");
                s.user_id = Some(user.id);
                s
            },
        )
        .await
        .and_then(|form| text.scope(|| state.theme().render_form(&form)))
    {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the CDN purge form");
            return render_server_error("The performance page could not be displayed.");
        }
    };

    let flash: Option<serde_json::Value> = session.remove(FLASH_KEY).await.ok().flatten();
    let kinds: Vec<&str> = CacheKind::ALL.iter().map(|k| k.as_str()).collect();

    let mut context = tera::Context::new();
    context.insert("clear_form", &form_html);
    context.insert("purge_form", &purge_form_html);
    context.insert("kinds", &kinds);
    context.insert("flash", &flash);
    context.insert("path", PATH);
    render_admin_template(&state, &viewer, "admin/config/performance.html", context).await
}

/// POST /admin/config/performance: either "Clear all caches" or "Purge the
/// CDN" (B303), whichever form was submitted, then back to the page.
async fn handle_post(
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
    let outcome = state
        .forms()
        .process(
            posted.get("_form_build_id").unwrap_or_default(),
            &posted,
            &session,
            &super::item_form::tap_state(&state, &viewer),
            |form_state| match form_state.form_id.as_str() {
                "cache_clear_form" => Ok(PostOutcome::ClearCaches),
                "cdn_purge_form" => validate_purge_request(&posted),
                _ => Err(vec![ValidationError::form(crate::form::FORM_EXPIRED)]),
            },
        )
        .await;

    let flash = match outcome {
        Ok((_, PostOutcome::ClearCaches)) => {
            tracing::info!(user_id = %user.id, "all caches cleared from the admin");
            // The command line's one path: bump every kind (each server, this
            // one included, drops its copies when it reads the new counter)
            // and wait for the bump to settle, so the page the browser is
            // sent back to is already read from the database.
            match crate::cache::clear_everywhere(state.redis(), &CacheKind::ALL).await {
                Ok(()) => serde_json::json!(CLEARED),
                Err(e) => {
                    tracing::error!(error = %e, "cache clear from the admin failed");
                    // This server's own copies can still go.
                    state.clear_local_caches().await;
                    serde_json::json!(
                        "Only this server's caches were cleared: the cache server (Redis) could \
                         not be reached, so other servers keep their copies until they expire."
                    )
                }
            }
        }
        Ok((_, PostOutcome::Purge(request))) => {
            match &request {
                PurgeRequest::Everything => crate::edge_cache::purge_everything(),
                PurgeRequest::Tags(tags) => crate::edge_cache::purge(tags.clone()),
            }
            tracing::info!(user_id = %user.id, "a CDN purge was requested from the admin");
            serde_json::json!(purge_flash_message(
                crate::edge_cache::drain_now(&state).await
            ))
        }
        Err(ProcessError::Csrf) => {
            return render_error("Invalid or expired form token. Please try again.");
        }
        Err(ProcessError::Invalid(errors)) => match errors.into_iter().find(|e| e.field.is_some())
        {
            Some(error) => serde_json::json!({ "kind": "error", "message": error.message }),
            None => return render_error("The form has expired. Reload the page and try again."),
        },
    };
    let _ = session.insert(FLASH_KEY, flash).await;
    Redirect::to(PATH).into_response()
}

/// Validate the purge form's fields into a [`PostOutcome::Purge`]: "These
/// tags" needs at least one, each matching the tag vocabulary's character set
/// and at most [`MAX_PURGE_TAGS`] of them; "Everything" needs nothing.
fn validate_purge_request(posted: &PostedForm) -> Result<PostOutcome, Vec<ValidationError>> {
    if posted.get("scope").unwrap_or("tags") == "everything" {
        return Ok(PostOutcome::Purge(PurgeRequest::Everything));
    }
    let tags: Vec<String> = posted
        .get("tags")
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if tags.is_empty() {
        return Err(vec![ValidationError::field(
            "tags",
            "Enter at least one tag, or choose Everything.",
        )]);
    }
    if tags.len() > MAX_PURGE_TAGS {
        return Err(vec![ValidationError::field(
            "tags",
            format!("Name at most {MAX_PURGE_TAGS} tags."),
        )]);
    }
    if let Some(bad) = tags.iter().find(|t| !is_valid_purge_tag(t)) {
        return Err(vec![ValidationError::field(
            "tags",
            format!("'{bad}' is not a valid tag: use letters, digits, ':', '-', '_' or '.'."),
        )]);
    }
    Ok(PostOutcome::Purge(PurgeRequest::Tags(tags)))
}

/// What the page says after a manual purge.
fn purge_flash_message(outcome: DrainOutcome) -> String {
    match outcome {
        DrainOutcome::Dispatched { all: true, .. } => "Purge sent: everything.".to_string(),
        DrainOutcome::Dispatched { tags, all: false } => {
            format!("Purge sent: {}.", tags.join(", "))
        }
        DrainOutcome::NoPlugin => {
            "No CDN purge plugin is enabled; nothing was sent.".to_string()
        }
        DrainOutcome::Nothing => "Nothing to purge.".to_string(),
    }
}
