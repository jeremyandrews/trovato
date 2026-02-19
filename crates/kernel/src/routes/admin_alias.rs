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

use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_admin,
    require_csrf,
};

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
    created_display: String,
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: i64 = 50;
    let offset = (page - 1) * per_page;

    let aliases = match UrlAlias::list_all(state.db(), per_page, offset).await {
        Ok(aliases) => aliases,
        Err(e) => {
            tracing::error!(error = %e, "failed to list url aliases");
            return render_server_error("Failed to load URL aliases.");
        }
    };

    let total = UrlAlias::count_all(state.db()).await.unwrap_or(0);
    let total_pages = (total as f64 / per_page as f64).ceil() as i64;

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

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("aliases", &aliases_display);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("total_pages", &total_pages);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/structure/aliases");

    render_admin_template(&state, "admin/aliases.html", context).await
}

/// Add URL alias form.
///
/// GET /admin/structure/aliases/add
async fn add_alias_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("action", "/admin/structure/aliases/add");
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/structure/aliases/add");

    render_admin_template(&state, "admin/alias-form.html", context).await
}

/// Add URL alias submit.
///
/// POST /admin/structure/aliases/add
async fn add_alias_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<UrlAliasFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Normalize paths
    let source = if form.source.starts_with('/') {
        form.source.clone()
    } else {
        format!("/{}", form.source)
    };

    let alias = if form.alias.starts_with('/') {
        form.alias.clone()
    } else {
        format!("/{}", form.alias)
    };

    let input = CreateUrlAlias {
        source,
        alias,
        language: form.language,
        stage_id: Some("live".to_string()),
    };

    match UrlAlias::create(state.db(), input).await {
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
    if let Err(redirect) = require_admin(&state, &session).await {
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

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert(
        "action",
        &format!("/admin/structure/aliases/{}/edit", alias_id),
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
    context.insert(
        "path",
        &format!("/admin/structure/aliases/{}/edit", alias_id),
    );

    render_admin_template(&state, "admin/alias-form.html", context).await
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Normalize paths
    let source = if form.source.starts_with('/') {
        form.source.clone()
    } else {
        format!("/{}", form.source)
    };

    let alias = if form.alias.starts_with('/') {
        form.alias.clone()
    } else {
        format!("/{}", form.alias)
    };

    let input = UpdateUrlAlias {
        source: Some(source),
        alias: Some(alias),
        language: form.language,
        stage_id: None,
    };

    match UrlAlias::update(state.db(), alias_id, input).await {
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match UrlAlias::delete(state.db(), alias_id).await {
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
