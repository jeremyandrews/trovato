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
//! **This `POST` is exempt from CSRF, here and on purpose**, unless it is the
//! confirmation page's own form. It is authenticated by the token in its
//! path, which is unguessable and authorizes exactly one thing, as the reset
//! and verification links are; and a mail client's one-click unsubscribe
//! (RFC 8058) posts `List-Unsubscribe=One-Click` with no session and no form
//! token. The confirmation page's form, `user_mail_unsubscribe`, is built
//! through the forms pipeline, and a post that names its build id
//! (`_form_build_id`) is checked through it (CSRF and `tap_form_validate`)
//! like every other form. Rate limited per client IP on the `forms` bucket by
//! the middleware, like any anonymous POST.
//!
//! Resubscribing is the opposite: a signed-in member's choice on their
//! profile page, a CSRF-protected form like every other account change.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::ValidationError;
use crate::models::user_mail_preference;
use crate::routes::helpers::render_server_error;
use crate::state::AppState;

/// The unsubscribe confirmation form, `user_mail_unsubscribe`, posting to
/// the link's own address.
fn build_unsubscribe_form(token: &str) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    Form::new("user_mail_unsubscribe")
        .action(format!("/user/mail/unsubscribe/{token}"))
        .attribute("class", "login-form")
        .element("op", FormElement::submit("Unsubscribe").weight(100))
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
///
/// The confirm stage carries the form, showing `errors`.
async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    stage: &str,
    token: &str,
    plugin: Option<&str>,
    errors: &[ValidationError],
) -> Response {
    let mut context = tera::Context::new();
    context.insert("stage", stage);
    if stage == "confirm" {
        let viewer = super::helpers::get_user_context(session, state).await;
        match super::admin_form::render_as(
            state,
            session,
            &viewer,
            build_unsubscribe_form(token),
            errors,
        )
        .await
        {
            Ok(html) => context.insert("unsubscribe_form", &html),
            Err(refusal) => return refusal,
        }
    }
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
                &[],
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
                &[],
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
/// design, except from the confirmation page's form; see the module
/// documentation.
async fn unsubscribe(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    // Read as a form whatever the content type says: a one-click post may be
    // multipart, and must still be taken.
    let posted = PostedForm(url::form_urlencoded::parse(&body).into_owned().collect());
    let mut form_state = None;
    let viewer = super::helpers::get_user_context(&session, &state).await;
    if posted.get("_form_build_id").is_some() {
        match super::admin_form::check_as(
            &state,
            &session,
            &viewer,
            &posted,
            "user_mail_unsubscribe",
            Vec::new(),
        )
        .await
        {
            Ok(checked) => form_state = Some(checked),
            Err(Err(refusal)) => return refusal,
            Err(Ok(errors)) => {
                let plugin = user_mail_preference::plugin_for_token(state.db(), &token)
                    .await
                    .ok()
                    .flatten();
                let Some(plugin) = plugin else {
                    return render(
                        &state,
                        &session,
                        StatusCode::NOT_FOUND,
                        "invalid",
                        &token,
                        None,
                        &[],
                    )
                    .await;
                };
                return render(
                    &state,
                    &session,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "confirm",
                    &token,
                    Some(&plugin),
                    &errors,
                )
                .await;
            }
        }
    }
    match user_mail_preference::opt_out(state.db(), &token).await {
        Ok(Some(plugin)) => {
            tracing::info!(plugin = %plugin, "a member opted out of a plugin's mail");
            if let Some(form_state) = &form_state {
                state
                    .forms()
                    .submitted(
                        form_state,
                        &posted,
                        None,
                        &super::item_form::tap_state(&state, &viewer),
                    )
                    .await;
            }
            render(
                &state,
                &session,
                StatusCode::OK,
                "done",
                &token,
                Some(&plugin),
                &[],
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
                &[],
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match crate::routes::helpers::require_login(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let form_state = match super::admin_form::check_as(
        &state,
        &session,
        &viewer,
        &posted,
        "user_mail_resubscribe",
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Err(refusal)) => return refusal,
        Err(Ok(errors)) => {
            return super::auth::render_profile(
                &state,
                &session,
                &user,
                super::auth::ProfilePage {
                    mail_errors: errors,
                    ..Default::default()
                },
            )
            .await;
        }
    };
    let plugin = posted.get("plugin").unwrap_or_default();
    if let Err(e) = user_mail_preference::resubscribe(state.db(), user.id, plugin).await {
        tracing::error!(error = %e, "resubscribe failed");
        return render_server_error("Failed to resubscribe.");
    }
    state
        .forms()
        .submitted(
            &form_state,
            &posted,
            None,
            &super::item_form::tap_state(&state, &viewer),
        )
        .await;
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
