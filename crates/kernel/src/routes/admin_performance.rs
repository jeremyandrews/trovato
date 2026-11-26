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
use crate::form::{FormElement, FormState, ProcessError};
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_error, render_server_error, require_any_permission,
};

/// The page's address.
pub const PATH: &str = "/admin/config/performance";

/// Either opens the page and runs the clear.
pub const PERMISSIONS: &[&str] = &["clear caches", "administer site"];

/// Session key for the message after a clear.
const FLASH_KEY: &str = "performance_flash";

/// What the page says after a clear that worked.
pub const CLEARED: &str = "Caches cleared.";

pub fn router() -> Router<AppState> {
    Router::new().route(PATH, get(performance_page).post(clear_caches))
}

/// The Clear all caches form, through the forms pipeline.
fn clear_form() -> crate::form::Form {
    crate::form::Form::new("cache_clear_form")
        .action(PATH)
        .element("op", FormElement::submit("Clear all caches").weight(0))
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

    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    let kinds: Vec<&str> = CacheKind::ALL.iter().map(|k| k.as_str()).collect();

    let mut context = tera::Context::new();
    context.insert("clear_form", &form_html);
    context.insert("kinds", &kinds);
    context.insert("flash", &flash);
    context.insert("path", PATH);
    render_admin_template(&state, &viewer, "admin/config/performance.html", context).await
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
