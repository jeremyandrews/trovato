//! Admin routes for URL alias management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::{CreateUrlAlias, UpdateUrlAlias, UrlAlias};
use crate::state::AppState;

use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_csrf,
    require_permission,
};

/// The alias listing's sortable columns: key and SQL.
const ALIAS_SORTS: &[(&str, &str)] = &[
    ("alias", super::admin_list::natural!("lower(alias)")),
    ("source", super::admin_list::natural!("lower(source)")),
    ("language", "language"),
    ("created", "created"),
];

// =============================================================================
// Form data
// =============================================================================

#[derive(Debug, Deserialize)]
struct UrlAliasFormData {
    #[serde(rename = "_token")]
    token: String,
    source: String,
    alias: String,
    language: Option<String>,
}

/// Alias display struct for templates.
#[derive(Debug, Serialize)]
struct AliasDisplay {
    id: uuid::Uuid,
    source: String,
    alias: String,
    language: String,
    /// Unix seconds; the template writes it with `admin_time`.
    created: i64,
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
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
            created: a.created,
        })
        .collect();

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert(
        "list",
        &query.view(total, aliases_display.len(), Some(&sort)),
    );
    context.insert("aliases", &aliases_display);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/structure/aliases");

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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("action", "/admin/structure/aliases/add");
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/structure/aliases/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/alias-form.html",
        context,
    )
    .await
}

/// Add URL alias submit.
///
/// POST /admin/structure/aliases/add
async fn add_alias_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<UrlAliasFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let input = CreateUrlAlias {
        source: form.source,
        alias: form.alias,
        language: form.language,
        stage_id: Some(crate::models::stage::LIVE_STAGE_ID),
    };

    match state.aliases().create(input).await {
        Ok(_) => Redirect::to("/admin/structure/aliases").into_response(),
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let alias = match UrlAlias::find_by_id(state.db(), alias_id).await {
        Ok(Some(alias)) => alias,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load url alias");
            return render_server_error("Failed to load URL alias.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert(
        "action",
        &format!("/admin/structure/aliases/{alias_id}/edit"),
    );
    context.insert("editing", &true);
    context.insert(
        "values",
        &serde_json::json!({
            "source": alias.source,
            "alias": alias.alias,
            "language": alias.language,
        }),
    );
    context.insert("path", &format!("/admin/structure/aliases/{alias_id}/edit"));

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/alias-form.html",
        context,
    )
    .await
}

/// Edit URL alias submit.
///
/// POST /admin/structure/aliases/{id}/edit
async fn edit_alias_submit(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
    Form(form): Form<UrlAliasFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let input = UpdateUrlAlias {
        source: Some(form.source),
        alias: Some(form.alias),
        language: form.language,
        stage_id: None,
    };

    match state.aliases().update(alias_id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/structure/aliases").into_response(),
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update url alias");
            render_server_error("Failed to update URL alias.")
        }
    }
}

/// Delete URL alias.
///
/// POST /admin/structure/aliases/{id}/delete
async fn delete_alias(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state.aliases().delete(alias_id).await {
        Ok(true) => {
            tracing::info!(alias_id = %alias_id, "url alias deleted");
            Redirect::to("/admin/structure/aliases").into_response()
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
        .route("/admin/structure/aliases/add", get(add_alias_form))
        .route("/admin/structure/aliases/add", post(add_alias_submit))
        .route("/admin/structure/aliases/{id}/edit", get(edit_alias_form))
        .route(
            "/admin/structure/aliases/{id}/edit",
            post(edit_alias_submit),
        )
        .route("/admin/structure/aliases/{id}/delete", post(delete_alias))
}
