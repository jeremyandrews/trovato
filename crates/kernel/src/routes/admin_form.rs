//! Building and checking a kernel admin         .and_then(|form| text.scope(|| state.theme().render_form_with_errors(&form, errors))) through `FormService` (B108).
//!
//! The admin screens added for access rules, text formats, appearance and
//! owned vocabularies all do the same two things: build a [`Form`] for the
//! signed-in user (CSRF token, `tap_form_alter`, saved state) and render it,
//! and on submission verify the token and that the saved state is this form's.
//! These two helpers are that, once.

use axum::response::Response;
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{Form, FormState, ProcessError, ValidationError};
use crate::models::User;
use crate::state::AppState;

use super::helpers::{render_error, render_server_error};

/// Build `form` for `user` and render it, showing `errors` beside it.
pub async fn render(
    state: &AppState,
    session: &Session,
    user: &User,
    form: Form,
    errors: &[ValidationError],
) -> Result<String, Response> {
    let viewer = super::helpers::user_context_for(state, user).await;
    // The form templates' own words (a select's "- Select -", "Add
    // another") in the viewer's admin language: every form here is an
    // admin form.
    let text = super::admin_nav::AdminText::for_page(state, &viewer, "/admin/").await;
    let mut form_state = FormState::new(form.form_id.clone(), "");
    form_state.user_id = Some(user.id);
    state
        .forms()
        .build(
            form,
            session,
            &super::item_form::tap_state(state, &viewer),
            form_state,
        )
        .await
        .and_then(|form| text.scope(|| state.theme().render_form_with_errors(&form, errors)))
        .map_err(|e| {
            tracing::error!(error = %e, "failed to build an admin form");
            render_server_error("The form could not be displayed.")
        })
}

/// Check a submission of the form `form_id`: its CSRF token, and that the
/// saved state it names was built for `user` and is that form. `extra` errors
/// (the caller's own validation) are returned with any a plugin adds through
/// `tap_form_validate`.
///
/// `Err(Ok(errors))` is a submission to show again with its errors;
/// `Err(Err(response))` is a refusal to send as is.
pub async fn check(
    state: &AppState,
    session: &Session,
    user: &User,
    posted: &PostedForm,
    form_id: &str,
    extra: Vec<ValidationError>,
) -> Result<(), Result<Vec<ValidationError>, Response>> {
    let viewer = super::helpers::user_context_for(state, user).await;
    let result = state
        .forms()
        .process(
            posted.get("_form_build_id").unwrap_or_default(),
            posted,
            session,
            &super::item_form::tap_state(state, &viewer),
            |form_state| {
                if form_state.form_id != form_id {
                    Err(vec![ValidationError::form(crate::form::FORM_EXPIRED)])
                } else if extra.is_empty() {
                    Ok(())
                } else {
                    Err(extra)
                }
            },
        )
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(ProcessError::Csrf) => Err(Err(render_error(
            "Invalid or expired form token. Please try again.",
        ))),
        Err(ProcessError::Invalid(errors)) => {
            if errors
                .iter()
                .any(|e| e.field.is_none() && e.message == crate::form::FORM_EXPIRED)
            {
                Err(Err(render_error(
                    "The form has expired. Reload the page and try again.",
                )))
            } else {
                Err(Ok(errors))
            }
        }
    }
}
