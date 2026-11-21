//! Unsubscribing from a plugin's member mail, and resubscribing (BL-94).
//!
//! Every message `mail.send-to-user` sends carries a link to
//! `/user/mail/unsubscribe/{token}`, in its footer and in its
//! `List-Unsubscribe` header.
//!
//! - `GET` renders a confirmation page with a button and changes nothing:
//!   link scanners and previewers fetch a link before anyone clicks it.
//! - `POST` records the opt out and renders a done page. Idempotent.
//!
//! **This `POST` is exempt from CSRF, here and on purpose**: it calls no
//! `require_csrf`. It is authenticated by the token in its path, which is
//! unguessable and authorizes exactly one thing, as the reset and
//! verification links are; and a mail client's one-click unsubscribe
//! (RFC 8058) posts `List-Unsubscribe=One-Click` with no session and no form
//! token. Rate limited per client IP on the `forms` bucket by the middleware,
//! like any anonymous POST.
//!
//! Resubscribing is the opposite: a signed-in member's choice on their
//! profile page, a CSRF-protected form like every other account change.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::models::user_mail_preference;
use crate::routes::helpers::{render_server_error, require_csrf};
use crate::state::AppState;

/// The resubscribe form.
#[derive(Debug, Deserialize)]
struct ResubscribeForm {
    #[serde(rename = "_token")]
    token: String,
    plugin: String,
}

/// What a member sees a plugin called: its manifest description, or its
/// machine name when the plugin is not loaded.
pub fn plugin_label(state: &AppState, plugin: &str) -> String {
    state
        .plugin_runtime()
        .get_plugin(plugin)
        .map(|p| p.info.description.clone())
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| plugin.to_string())
}

/// Render the unsubscribe page in one of its three states.
async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    stage: &str,
    token: &str,
    plugin: Option<&str>,
) -> Response {
    let mut context = tera::Context::new();
    context.insert("stage", stage);
    context.insert("token", token);
    if let Some(plugin) = plugin {
        context.insert("plugin_label", &plugin_label(state, plugin));
    }
    super::helpers::inject_site_context(state, session, &mut context, "/user/mail/unsubscribe")
        .await;
    match state
        .theme()
        .tera()
        .render("user/mail_unsubscribe.html", &context)
    {
        // The token is in the URL: no referrer may carry it off-site.
        Ok(html) => (
            status,
            [(header::REFERRER_POLICY, "no-referrer")],
            Html(html),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the unsubscribe page");
            render_server_error("Failed to render the unsubscribe page.")
        }
    }
}

/// GET /user/mail/unsubscribe/{token}: confirm, change nothing.
async fn confirm(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
) -> Response {
    match user_mail_preference::plugin_for_token(state.db(), &token).await {
        Ok(Some(plugin)) => {
            render(
                &state,
                &session,
                StatusCode::OK,
                "confirm",
                &token,
                Some(&plugin),
            )
            .await
        }
        Ok(None) => {
            render(
                &state,
                &session,
                StatusCode::NOT_FOUND,
                "invalid",
                &token,
                None,
            )
            .await
        }
        Err(e) => {
            tracing::error!(error = %e, "unsubscribe lookup failed");
            render_server_error("Failed to read the unsubscribe link.")
        }
    }
}

/// POST /user/mail/unsubscribe/{token}: record the opt out. No CSRF token by
/// design; see the module documentation.
async fn unsubscribe(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
) -> Response {
    match user_mail_preference::opt_out(state.db(), &token).await {
        Ok(Some(plugin)) => {
            tracing::info!(plugin = %plugin, "a member opted out of a plugin's mail");
            render(
                &state,
                &session,
                StatusCode::OK,
                "done",
                &token,
                Some(&plugin),
            )
            .await
        }
        Ok(None) => {
            render(
                &state,
                &session,
                StatusCode::NOT_FOUND,
                "invalid",
                &token,
                None,
            )
            .await
        }
        Err(e) => {
            tracing::error!(error = %e, "recording an opt out failed");
            render_server_error("Failed to record the unsubscribe.")
        }
    }
}

/// POST /user/mail/resubscribe: a signed-in member takes back an opt out.
async fn resubscribe(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ResubscribeForm>,
) -> Response {
    let user_id = match crate::routes::helpers::require_login(&state, &session).await {
        Ok(user) => user.id,
        Err(redirect) => return redirect,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    if let Err(e) = user_mail_preference::resubscribe(state.db(), user_id, &form.plugin).await {
        tracing::error!(error = %e, "resubscribe failed");
        return render_server_error("Failed to resubscribe.");
    }
    Redirect::to("/user/profile").into_response()
}

/// The unsubscribe and resubscribe routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/user/mail/unsubscribe/{token}",
            get(confirm).post(unsubscribe),
        )
        .route("/user/mail/resubscribe", post(resubscribe))
}
