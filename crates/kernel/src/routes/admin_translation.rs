//! Admin routes for content translation management.
//!
//! Provides a side-by-side translation UI for translating content items
//! into different languages.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::form::csrf::generate_csrf_token;
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_csrf,
    require_permission,
};

/// Session key for flash messages on the translation screens.
const TRANSLATION_FLASH_KEY: &str = "translation_admin_flash";

/// Create the content translation admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/content/{id}/translate", get(translation_list))
        .route(
            "/admin/content/{id}/translate/{lang}",
            get(translation_edit).post(translation_save),
        )
        .route(
            "/admin/content/{id}/translate/{lang}/delete",
            post(translation_delete),
        )
}

/// Translation form data.
///
/// `fields` is the item's translatable field values, flattened the way
/// `ContentFormData` flattens a content type's own fields: the names are not
/// known at compile time because they come from the content type.
#[derive(Debug, Deserialize)]
struct TranslationFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    #[allow(dead_code)]
    form_build_id: String,
    title: String,
    #[serde(flatten)]
    fields: std::collections::HashMap<String, serde_json::Value>,
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

    let flash: Option<String> = session.get(TRANSLATION_FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(TRANSLATION_FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear flash message");
    }

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("languages", languages);
    context.insert("default_language", default_lang);
    context.insert("translations", &translations);
    context.insert("item_id", &id.to_string());
    context.insert("flash", &flash);
    // `page--admin.html` reads `path` to mark the active admin menu entry.
    context.insert("path", &format!("/admin/content/{id}/translate"));

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

    // The translatable fields come from the item's content type, so the form
    // renders whatever that type declares rather than a fixed pair of boxes.
    let content_type = state.content_types().get(&item.item_type);

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let flash: Option<String> = session.get(TRANSLATION_FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(TRANSLATION_FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear flash message");
    }

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("language", &lang);
    context.insert("default_language", state.default_language());
    context.insert("translation", &translation);
    context.insert("item_id", &id.to_string());
    context.insert("content_type", &content_type);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("flash", &flash);
    // `page--admin.html` reads `path` to mark the active admin menu entry.
    context.insert("path", &format!("/admin/content/{id}/translate/{lang}"));

    render_admin_template(&state, "admin/content-translate-edit.html", context).await
}

/// Save a translation for one language.
///
/// POST /admin/content/{id}/translate/{lang}
///
/// The kernel's first write path for `item_translation`. The table has been
/// read since it was added — the request overlay, the gather join, the sitemap
/// and the translated menu labels all consult it — and until now nothing but
/// SQL could put a row in it, so every one of those readers was reading
/// something no part of the product could produce.
async fn translation_save(
    State(state): State<AppState>,
    session: Session,
    Path((id, lang)): Path<(Uuid, String)>,
    Form(form): Form<TranslationFormData>,
) -> Response {
    let Ok(_user) = require_permission(&state, &session, "translate content").await else {
        return super::helpers::render_error("Permission denied");
    };

    // Same `_token` protection as the rest of the admin, checked after the
    // permission so an unauthorized caller learns nothing from the difference.
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(item) = state.items().load(id).await.ok().flatten() else {
        return render_not_found();
    };

    // A language the site does not know is a 404, exactly as it is on the GET:
    // the two have to agree, or the form renders at a URL that refuses to save.
    if !state.known_languages().iter().any(|l| l == &lang) {
        return render_not_found();
    }

    // Translating an item into the language it is already written in would
    // produce a row the overlay then applies on top of the original, which is
    // at best a no-op and at worst a second, diverging copy of the same text.
    if lang == item.language {
        return super::helpers::render_error(
            "This item is already written in that language. Translate it into a different one.",
        );
    }

    let title = form.title.trim();
    if title.is_empty() {
        return super::helpers::render_error("A translation needs a title.");
    }

    // Same exclusion rule the content form uses: `_`-prefixed keys are form
    // machinery, and `title` is carried on its own column.
    let mut fields = serde_json::Map::new();
    for (key, value) in &form.fields {
        if !key.starts_with('_') && key != "title" {
            fields.insert(key.clone(), value.clone());
        }
    }
    let fields = serde_json::Value::Object(fields);

    match state
        .items()
        .save_translation(id, &lang, title, &fields)
        .await
    {
        Ok(_) => {
            tracing::info!(item_id = %id, language = %lang, "translation saved");
            let msg = format!("The {lang} translation has been saved.");
            if let Err(e) = session.insert(TRANSLATION_FLASH_KEY, &msg).await {
                tracing::warn!(error = %e, "failed to set flash message");
            }
            Redirect::to(&format!("/admin/content/{id}/translate/{lang}")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, item_id = %id, language = %lang, "failed to save translation");
            render_server_error("Failed to save the translation.")
        }
    }
}

/// Remove a translation.
///
/// POST /admin/content/{id}/translate/{lang}/delete
async fn translation_delete(
    State(state): State<AppState>,
    session: Session,
    Path((id, lang)): Path<(Uuid, String)>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let Ok(_user) = require_permission(&state, &session, "translate content").await else {
        return super::helpers::render_error("Permission denied");
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state.items().delete_translation(id, &lang).await {
        Ok(removed) => {
            let msg = if removed {
                format!("The {lang} translation has been removed.")
            } else {
                format!("There was no {lang} translation to remove.")
            };
            tracing::info!(item_id = %id, language = %lang, removed, "translation delete");
            if let Err(e) = session.insert(TRANSLATION_FLASH_KEY, &msg).await {
                tracing::warn!(error = %e, "failed to set flash message");
            }
            Redirect::to(&format!("/admin/content/{id}/translate")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, item_id = %id, language = %lang, "failed to delete translation");
            render_server_error("Failed to remove the translation.")
        }
    }
}
