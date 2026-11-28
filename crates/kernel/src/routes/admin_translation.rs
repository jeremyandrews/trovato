//! Admin routes for content translation management.
//!
//! Provides a side-by-side translation UI for translating content items
//! into different languages.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::TranslationRefused;
use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::i18n::UiMessage;
use crate::models::{Item, User};
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    render_admin_template, render_error, render_not_found, render_server_error, require_permission,
};

/// Session key for flash messages on the translation screens.
const TRANSLATION_FLASH_KEY: &str = "translation_admin_flash";

/// The form that writes one language's translation of an item.
const TRANSLATION_FORM_ID: &str = "content_translation_form";

/// The form that removes one language's translation of an item.
const TRANSLATION_DELETE_FORM_ID: &str = "content_translation_delete_form";

/// The posted names that are not translated field values: the form
/// machinery (every `_` name) and the submit button.
const SUBMIT_NAME: &str = "op";

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
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
}

/// A stored field value as a form field shows it: a string as it is,
/// nothing as empty, anything else as its JSON.
fn as_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The translation form for `item` in `lang`, showing `values` (the title and
/// each translatable field by name).
///
/// The translatable fields are the content type's own text fields, so the
/// form is built from the type rather than from a fixed list. Only `Text` and
/// `TextLong` appear: a boolean, a date, a file reference or a record
/// reference is the same value in every language, and offering a second copy
/// of one per language invites them to drift apart with nothing to reconcile
/// them. A type that has gone away leaves the title, which is always
/// translatable because it is a column.
///
/// Each field's hint is its original text. That is a value the form templates
/// cannot put into their words, so `original` translates the hint here.
fn translation_form(
    item: &Item,
    lang: &str,
    content_type: Option<&trovato_sdk::types::ContentTypeDefinition>,
    values: &std::collections::HashMap<String, String>,
    original: &dyn Fn(UiMessage) -> String,
) -> crate::form::Form {
    use trovato_sdk::types::FieldType;

    let value = |name: &str| values.get(name).cloned().unwrap_or_default();
    let mut form = crate::form::Form::new(TRANSLATION_FORM_ID)
        .action(format!("/admin/content/{}/translate/{lang}", item.id))
        .element(
            "title",
            FormElement::textfield()
                .title("Title")
                .description(original(
                    UiMessage::new("Original: {text}").arg("text", item.title.clone()),
                ))
                .default_value(value("title"))
                .required()
                .weight(0),
        );
    let fields = content_type.map(|ct| ct.fields.as_slice()).unwrap_or(&[]);
    for (weight, field) in (1..).zip(fields) {
        let element = match field.field_type {
            FieldType::Text { .. } => FormElement::textfield(),
            FieldType::TextLong => FormElement::textarea(6),
            _ => continue,
        };
        let source = item
            .fields
            .get(&field.field_name)
            .map(as_text)
            .unwrap_or_default();
        let hint = if source.is_empty() {
            original(UiMessage::new("The original is empty."))
        } else {
            original(UiMessage::new("Original: {text}").arg("text", source))
        };
        form = form.element(
            field.field_name.clone(),
            element
                .title(field.label.clone())
                .description(hint)
                .default_value(value(&field.field_name))
                .weight(weight),
        );
    }
    form.element(
        SUBMIT_NAME,
        FormElement::submit("Save translation").weight(1000),
    )
}

/// The form that removes the `lang` translation of item `id`. It asks first
/// on the confirmation page (routes::admin_confirm), with the question and
/// button its hidden fields carry, already translated.
fn delete_form_for(id: Uuid, lang: &str, question: String, label: String) -> crate::form::Form {
    crate::form::Form::new(TRANSLATION_DELETE_FORM_ID)
        .action(format!(
            "/admin/content/{id}/translate/{lang}/delete?confirm"
        ))
        .element(
            "_confirm",
            FormElement::hidden().default_value(question).weight(0),
        )
        .element(
            "_confirm_label",
            FormElement::hidden().default_value(label).weight(1),
        )
        .element(
            SUBMIT_NAME,
            FormElement::submit("Remove this translation")
                .attribute("class", "button--danger")
                .weight(10),
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
    let user = match require_permission(&state, &session, "translate content").await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };

    // `translate content` says this user translates; it does not say *what*.
    // The seam decides that, and drops the fields they may not see before the
    // item reaches the template context.
    let user_ctx = super::helpers::admin_user_context(&state, &user).await;
    let Some(item) = state
        .items()
        .load_for_view_filtered(id, &user_ctx, "view")
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };

    let languages = &state.known_languages();
    let default_lang = &state.default_language();

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

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-translate-list.html",
        context,
    )
    .await
}

/// Edit a translation for a specific language.
///
/// GET /admin/content/{id}/translate/{lang}
async fn translation_edit(
    State(state): State<AppState>,
    session: Session,
    Path((id, lang)): Path<(Uuid, String)>,
) -> Response {
    let user = match require_permission(&state, &session, "translate content").await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_translation_edit(&state, &session, &user, id, &lang, None, &[]).await
}

/// The translation screen for item `id` in `lang`: the form, showing the
/// stored translation or, when a submission is shown again with `errors`,
/// the `posted` values; and below it the form that removes the translation.
async fn render_translation_edit(
    state: &AppState,
    session: &Session,
    user: &User,
    id: Uuid,
    lang: &str,
    posted: Option<&PostedForm>,
    errors: &[ValidationError],
) -> Response {
    // Same bar as the list, and the same field filtering: the side-by-side form
    // puts every field of the original on the page.
    let user_ctx = super::helpers::admin_user_context(state, user).await;
    let Some(item) = state
        .items()
        .load_for_view_filtered(id, &user_ctx, "view")
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };

    // Check language is valid
    if !state.known_languages().iter().any(|l| l == lang) {
        return render_not_found();
    }

    // Load existing translation if any
    let translation = state
        .items()
        .load_translation(id, lang)
        .await
        .ok()
        .flatten();

    // The translatable fields come from the item's content type, so the form
    // renders whatever that type declares rather than a fixed pair of boxes.
    let content_type = state.content_types().get(&item.item_type);

    // What the fields show: what was posted, when it is shown again, or else
    // the stored translation.
    let values: std::collections::HashMap<String, String> = match (posted, &translation) {
        (Some(posted), _) => posted
            .0
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .cloned()
            .collect(),
        (None, Some(t)) => {
            let mut values: std::collections::HashMap<String, String> = t
                .fields
                .as_object()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|(k, v)| (k.clone(), as_text(v)))
                        .collect()
                })
                .unwrap_or_default();
            values.insert("title".to_string(), t.title.clone());
            values
        }
        (None, None) => std::collections::HashMap::new(),
    };

    let path = format!("/admin/content/{id}/translate/{lang}");
    let text = super::admin_nav::AdminText::for_page(state, &user_ctx, &path).await;
    let locale = state.locale().map(|l| &**l);
    let translated = |message: UiMessage| message.text(locale, &text.lang);
    let form = translation_form(&item, lang, content_type.as_ref(), &values, &translated);
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let delete_form_html = if translation.is_some() {
        let form = delete_form_for(
            id,
            lang,
            translated(
                UiMessage::new("Remove the {language} translation? This cannot be undone.")
                    .arg("language", lang),
            ),
            translated(UiMessage::new("Remove the {language} translation").arg("language", lang)),
        );
        match admin_form::render(state, session, user, form, &[]).await {
            Ok(html) => Some(html),
            Err(response) => return response,
        }
    } else {
        None
    };

    let flash: Option<String> = session.get(TRANSLATION_FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(TRANSLATION_FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear flash message");
    }

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("language", lang);
    context.insert("default_language", &state.default_language());
    context.insert("translation", &translation);
    context.insert("item_id", &id.to_string());
    context.insert("form", &form_html);
    context.insert("delete_form", &delete_form_html);
    context.insert("flash", &flash);
    // `page--admin.html` reads `path` to mark the active admin menu entry.
    context.insert("path", &path);

    render_admin_template(
        state,
        &user_ctx,
        "admin/content-translate-edit.html",
        context,
    )
    .await
}

/// The item a translator writes a translation of, or removes one from: an
/// item that exists and that this translator may view. Any other is a 404,
/// the refusal a missing item produces.
async fn translatable_item(
    state: &AppState,
    user_ctx: &crate::tap::UserContext,
    id: Uuid,
) -> Result<Item, Response> {
    let Some(item) = state.items().load(id).await.ok().flatten() else {
        return Err(render_not_found());
    };
    if !state
        .items()
        .check_access(&item, "view", user_ctx)
        .await
        .unwrap_or(false)
    {
        return Err(render_not_found());
    }
    Ok(item)
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "translate content").await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };

    // The user, so the save can refuse a translated value for a field this
    // user may not edit: a translation writes field values like any other save.
    let user_ctx = super::helpers::admin_user_context(&state, &user).await;

    // An item this translator may not view is not one they may write a
    // translation onto, and the refusal is the one a missing item produces.
    let item = match translatable_item(&state, &user_ctx, id).await {
        Ok(item) => item,
        Err(refusal) => return refusal,
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
        return render_error(
            "This item is already written in that language. Translate it into a different one.",
        );
    }

    // The form token and saved state, checked after the permission and the
    // item so an unauthorized caller learns nothing from the difference.
    let title = posted.get("title").unwrap_or_default().trim().to_string();
    let mut errors = Vec::new();
    if title.is_empty() {
        errors.push(ValidationError::field(
            "title",
            "A translation needs a title.",
        ));
    }
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        TRANSLATION_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_translation_edit(
                &state,
                &session,
                &user,
                id,
                &lang,
                Some(&posted),
                &errors,
            )
            .await;
        }
        Err(Err(response)) => return response,
    };

    // Same exclusion rule the content form uses: `_`-prefixed keys are form
    // machinery, `title` is carried on its own column, and the submit button
    // is no field.
    let mut fields = serde_json::Map::new();
    for (key, value) in &posted.0 {
        if !key.starts_with('_') && key != "title" && key != SUBMIT_NAME {
            fields.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
    }
    let fields = serde_json::Value::Object(fields);

    // Through the service's translation write, the one `item-api` uses: it
    // decides the write again, drops the item from the cache and fires
    // `tap_item_update`, so a translation is an edit wherever it comes from.
    match state
        .items()
        .translate(id, &lang, &title, &fields, &user_ctx)
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(item_id = %id, language = %lang, "translation saved");
            let msg = format!("The {lang} translation has been saved.");
            if let Err(e) = session.insert(TRANSLATION_FLASH_KEY, &msg).await {
                tracing::warn!(error = %e, "failed to set flash message");
            }
            Redirect::to(&format!("/admin/content/{id}/translate/{lang}")).into_response()
        }
        Err(e) if e.downcast_ref::<TranslationRefused>().is_some() => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, item_id = %id, language = %lang, "failed to save translation");
            render_server_error("Failed to save the translation.")
        }
    }
}

/// Remove a translation.
///
/// POST /admin/content/{id}/translate/{lang}/delete, from the translation
/// screen's remove form once the confirmation page has asked.
async fn translation_delete(
    State(state): State<AppState>,
    session: Session,
    Path((id, lang)): Path<(Uuid, String)>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "translate content").await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };

    // The item is loaded for one reason: to ask the seam about it. Removing a
    // translation is a write to an item, and an item this translator may not
    // view is not theirs to write to.
    let user_ctx = super::helpers::admin_user_context(&state, &user).await;
    if let Err(refusal) = translatable_item(&state, &user_ctx, id).await {
        return refusal;
    }

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        TRANSLATION_DELETE_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
        Err(Err(response)) => return response,
    };

    match state
        .items()
        .withdraw_translation(id, &lang, &user_ctx)
        .await
    {
        Ok(removed) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
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
        Err(e) if e.downcast_ref::<TranslationRefused>().is_some() => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, item_id = %id, language = %lang, "failed to delete translation");
            render_server_error("Failed to remove the translation.")
        }
    }
}
