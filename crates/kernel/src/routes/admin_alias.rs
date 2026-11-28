//! Admin routes for URL alias management.
//!
//! Every form here is built and checked by the form pipeline
//! (`routes::admin_form`): the add and edit form, and the confirmation that
//! deletes an alias.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form as PostedBody, Router};
use serde::Serialize;
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{Form, FormElement, ValidationError};
use crate::models::{CreateUrlAlias, UpdateUrlAlias, UrlAlias, User};
use crate::state::AppState;

use super::admin_form;
use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    render_admin_template, render_error, render_not_found, render_server_error, require_permission,
};

/// The permission every alias screen takes.
const PERMISSION: &str = "administer site";

/// The alias list, under which every alias screen hangs.
const BASE: &str = "/admin/structure/aliases";

/// The add and edit form.
const ALIAS_FORM_ID: &str = "url_alias_form";

/// The form that confirms deleting an alias.
const DELETE_FORM_ID: &str = "url_alias_delete_form";

/// The alias listing's sortable columns: key and SQL.
const ALIAS_SORTS: &[(&str, &str)] = &[
    ("alias", "lower(alias)"),
    ("source", "lower(source)"),
    ("language", "language"),
    ("created", "created"),
];

// =============================================================================
// Form data
// =============================================================================

/// What the alias form's fields hold.
#[derive(Debug, Default)]
struct AliasValues {
    source: String,
    alias: String,
    language: String,
}

/// Alias display struct for templates.
#[derive(Debug, Serialize)]
struct AliasDisplay {
    id: uuid::Uuid,
    source: String,
    alias: String,
    language: String,
    created_display: String,
}

// =============================================================================
// Forms
// =============================================================================

/// A text field for a path or a code: no spell checking, no capitals.
fn plain_text() -> FormElement {
    FormElement::textfield()
        .attribute("spellcheck", "false")
        .attribute("autocapitalize", "none")
}

/// The add and edit form: `action` is where it posts, `values` what its
/// fields hold, `editing` whether it saves an existing alias, and `cancel`
/// the link beside its button.
fn alias_form(action: &str, values: &AliasValues, editing: bool, cancel: String) -> Form {
    Form::new(ALIAS_FORM_ID)
        .action(action)
        .element(
            "source",
            plain_text()
                .title("Source path")
                .description("The internal path (e.g., /item/uuid)")
                .default_value(values.source.clone())
                .required()
                .weight(0),
        )
        .element(
            "alias",
            plain_text()
                .title("Alias path")
                .description("The human-readable URL (e.g., /about-us)")
                .default_value(values.alias.clone())
                .required()
                .weight(1),
        )
        .element(
            "language",
            plain_text()
                .title("Language")
                .description("Language code for this alias (e.g., en, es, fr)")
                .default_value(if values.language.is_empty() {
                    "en".to_string()
                } else {
                    values.language.clone()
                })
                .weight(2),
        )
        .element(
            "op",
            FormElement::submit(if editing {
                "Save changes"
            } else {
                "Create alias"
            })
            .suffix(cancel)
            .weight(10),
        )
}

/// The confirmation form that deletes the alias `id`.
fn delete_form_for(id: uuid::Uuid, cancel: String) -> Form {
    Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{id}/delete"))
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .suffix(cancel)
                .weight(10),
        )
}

/// A Cancel link back to the alias list, in `user`'s admin language on `path`.
async fn cancel_link(state: &AppState, user: &User, path: &str) -> String {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, path).await;
    format!(
        r#"<a href="{BASE}" class="button button--secondary">{}</a>"#,
        text.html("Cancel", &[])
    )
}

/// What the alias form posted, with the errors that keep it from saving.
fn read_alias(posted: &PostedForm) -> (AliasValues, Vec<ValidationError>) {
    let values = AliasValues {
        source: posted.get("source").unwrap_or_default().to_string(),
        alias: posted.get("alias").unwrap_or_default().to_string(),
        language: posted.get("language").unwrap_or_default().to_string(),
    };
    let mut errors = Vec::new();
    if values.source.trim().is_empty() {
        errors.push(ValidationError::field("source", "Source path is required."));
    }
    if values.alias.trim().is_empty() {
        errors.push(ValidationError::field("alias", "Alias path is required."));
    }
    (values, errors)
}

/// Render the add form, or the edit form of the alias `editing`.
async fn render_alias_form(
    state: &AppState,
    session: &Session,
    user: &User,
    editing: Option<uuid::Uuid>,
    values: &AliasValues,
    errors: &[ValidationError],
) -> Response {
    let action = match editing {
        Some(id) => format!("{BASE}/{id}/edit"),
        None => format!("{BASE}/add"),
    };
    let cancel = cancel_link(state, user, &action).await;
    let form_html = match admin_form::render(
        state,
        session,
        user,
        alias_form(&action, values, editing.is_some(), cancel),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("editing", &editing.is_some());
    context.insert("alias", &values.alias);
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/alias-form.html", context).await
}

/// The alias `id`, or the page that says why it is not there.
async fn load_alias(state: &AppState, id: uuid::Uuid) -> Result<UrlAlias, Response> {
    match UrlAlias::find_by_id(state.db(), id).await {
        Ok(Some(alias)) => Ok(alias),
        Ok(None) => Err(render_not_found()),
        Err(e) => {
            tracing::error!(error = %e, "failed to load url alias");
            Err(render_server_error("Failed to load URL alias."))
        }
    }
}

// =============================================================================
// Handlers
// =============================================================================

/// List all URL aliases.
///
/// GET /admin/structure/aliases
async fn list_aliases(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, PERMISSION).await {
        return redirect;
    }

    let query = ListQuery::from_params(&params);
    let sort = query.sort(ALIAS_SORTS, ("created", true));
    let like = query.like();

    let (aliases, total) = match UrlAlias::admin_page(
        state.db(),
        like.as_deref(),
        &sort.order_by(),
        PER_PAGE,
        query.offset(),
    )
    .await
    {
        Ok(page) => page,
        Err(e) => {
            tracing::error!(error = %e, "failed to list url aliases");
            return render_server_error("Failed to load URL aliases.");
        }
    };

    // Convert to display structs with formatted dates
    let aliases_display: Vec<AliasDisplay> = aliases
        .into_iter()
        .map(|a| AliasDisplay {
            id: a.id,
            source: a.source,
            alias: a.alias,
            language: a.language,
            created_display: chrono::DateTime::from_timestamp(a.created, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "Unknown".to_string()),
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert(
        "list",
        &query.view(total, aliases_display.len(), Some(&sort)),
    );
    context.insert("aliases", &aliases_display);
    context.insert("path", BASE);

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/aliases.html",
        context,
    )
    .await
}

/// Add URL alias form.
///
/// GET /admin/structure/aliases/add
async fn add_alias_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_alias_form(&state, &session, &user, None, &AliasValues::default(), &[]).await
}

/// Add URL alias submit.
///
/// POST /admin/structure/aliases/add
async fn add_alias_submit(
    State(state): State<AppState>,
    session: Session,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (values, errors) = read_alias(&posted);
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, ALIAS_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_alias_form(&state, &session, &user, None, &values, &errors).await;
            }
            Err(Err(response)) => return response,
        };

    let input = CreateUrlAlias {
        source: values.source,
        alias: values.alias,
        language: posted.get("language").map(str::to_string),
        stage_id: Some(crate::models::stage::LIVE_STAGE_ID),
    };

    match state.aliases().create(input).await {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to(BASE).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create url alias");
            render_server_error("Failed to create URL alias. The alias may already exist.")
        }
    }
}

/// Edit URL alias form.
///
/// GET /admin/structure/aliases/{id}/edit
async fn edit_alias_form(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let alias = match load_alias(&state, alias_id).await {
        Ok(alias) => alias,
        Err(response) => return response,
    };
    let values = AliasValues {
        source: alias.source,
        alias: alias.alias,
        language: alias.language,
    };
    render_alias_form(&state, &session, &user, Some(alias_id), &values, &[]).await
}

/// Edit URL alias submit.
///
/// POST /admin/structure/aliases/{id}/edit
async fn edit_alias_submit(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (values, errors) = read_alias(&posted);
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, ALIAS_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_alias_form(
                    &state,
                    &session,
                    &user,
                    Some(alias_id),
                    &values,
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };

    let input = UpdateUrlAlias {
        source: Some(values.source),
        alias: Some(values.alias),
        language: posted.get("language").map(str::to_string),
        stage_id: None,
    };

    match state.aliases().update(alias_id, input).await {
        Ok(Some(_)) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to(BASE).into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update url alias");
            render_server_error("Failed to update URL alias.")
        }
    }
}

/// Confirm deleting a URL alias.
///
/// GET /admin/structure/aliases/{id}/delete
async fn delete_alias_form(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let alias = match load_alias(&state, alias_id).await {
        Ok(alias) => alias,
        Err(response) => return response,
    };
    let path = format!("{BASE}/{alias_id}/delete");
    let cancel = cancel_link(&state, &user, &path).await;
    let form_html = match admin_form::render(
        &state,
        &session,
        &user,
        delete_form_for(alias_id, cancel),
        &[],
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("alias", &alias.alias);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(&state, &viewer, "admin/alias-delete.html", context).await
}

/// Delete URL alias.
///
/// POST /admin/structure/aliases/{id}/delete
async fn delete_alias(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, DELETE_FORM_ID, Vec::new()).await
        {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return render_error("The form has expired. Reload the page and try again.");
            }
            Err(Err(response)) => return response,
        };

    match state.aliases().delete(alias_id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(alias_id = %alias_id, "url alias deleted");
            Redirect::to(BASE).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete url alias");
            render_server_error("Failed to delete URL alias.")
        }
    }
}

// =============================================================================
// Router
// =============================================================================

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/structure/aliases", get(list_aliases))
        .route(
            "/admin/structure/aliases/add",
            get(add_alias_form).post(add_alias_submit),
        )
        .route(
            "/admin/structure/aliases/{id}/edit",
            get(edit_alias_form).post(edit_alias_submit),
        )
        .route(
            "/admin/structure/aliases/{id}/delete",
            get(delete_alias_form).post(delete_alias),
        )
}
