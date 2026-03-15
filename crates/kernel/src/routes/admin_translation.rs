//! Admin routes for content translation management.
//!
//! Provides a side-by-side translation UI for translating content items
//! into different languages.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use tower_sessions::Session;
use uuid::Uuid;

use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_not_found, render_server_error, require_permission,
};

/// Create the content translation admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/content/{id}/translate", get(translation_list))
        .route(
            "/admin/content/{id}/translate/{lang}",
            get(translation_edit),
        )
}

/// List available translations for an item.
///
/// GET /admin/content/{id}/translate
async fn translation_list(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Response {
    let Ok(_user) = require_permission(&state, &session, "translate content").await else {
        return super::helpers::render_error("Permission denied");
    };

    let Some(item) = state.items().load(id).await.ok().flatten() else {
        return render_not_found();
    };

    let languages = state.known_languages();
    let default_lang = state.default_language();

    // List existing translations via ItemService
    let translations = match state.items().list_translations(id).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, item_id = %id, "failed to list translations");
            return render_server_error("Failed to load translations");
        }
    };

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("languages", languages);
    context.insert("default_language", default_lang);
    context.insert("translations", &translations);
    context.insert("item_id", &id.to_string());

    render_admin_template(&state, "admin/content-translate-list.html", context).await
}

/// Edit a translation for a specific language.
///
/// GET /admin/content/{id}/translate/{lang}
async fn translation_edit(
    State(state): State<AppState>,
    session: Session,
    Path((id, lang)): Path<(Uuid, String)>,
) -> Response {
    let Ok(_user) = require_permission(&state, &session, "translate content").await else {
        return super::helpers::render_error("Permission denied");
    };

    let Some(item) = state.items().load(id).await.ok().flatten() else {
        return render_not_found();
    };

    // Check language is valid
    if !state.known_languages().iter().any(|l| l == &lang) {
        return render_not_found();
    }

    // Load existing translation if any
    let translation = state
        .items()
        .load_translation(id, &lang)
        .await
        .ok()
        .flatten();

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("language", &lang);
    context.insert("default_language", state.default_language());
    context.insert("translation", &translation);
    context.insert("item_id", &id.to_string());

    render_admin_template(&state, "admin/content-translate-edit.html", context).await
}
