//! Languages (B161): `/admin/config/languages`.
//!
//! List the site's languages in order, add one (from a list of common
//! languages or with a code of the site's own, a private use `x-...` code
//! included), edit its name, direction, order and whether it is the default,
//! delete one nothing is written in, and choose whether the default language
//! is served and linked under its own prefix (`/en/...`).
//!
//! Before B161 the languages came from configuration only and were read once at
//! startup. Every change here is saved, read back into this server at once
//! ([`AppState::reload_languages`]) and announced to the others
//! ([`CacheKind::Languages`], with [`CacheKind::Menus`] since menu links carry
//! the prefixes), so `/{code}` serves the front page the moment the language
//! is added. Every route takes `administer languages`; every form goes
//! through the forms pipeline.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;

use crate::cache::generation::{self, CacheKind};
use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::middleware::language::PREFIX_DEFAULT_KEY;
use crate::models::{CreateLanguage, Language, SiteConfig};
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    render_admin_template, render_not_found, render_server_error, require_permission,
};

/// The permission every route here takes (Drupal's name for it).
pub const PERMISSION: &str = "administer languages";

const BASE: &str = "/admin/config/languages";
const SETTINGS_FORM_ID: &str = "language_settings_form";
const LANGUAGE_FORM_ID: &str = "language_form";
const DELETE_FORM_ID: &str = "language_delete_form";
const FLASH_KEY: &str = "languages_flash";

/// The value of the add form's language choice that asks for a code of the
/// site's own.
const CUSTOM: &str = "custom";

/// Common languages the add form offers: code, name in English, direction.
const PREDEFINED: &[(&str, &str, &str)] = &[
    ("ar", "Arabic", "rtl"),
    ("ca", "Catalan", "ltr"),
    ("cs", "Czech", "ltr"),
    ("da", "Danish", "ltr"),
    ("de", "German", "ltr"),
    ("el", "Greek", "ltr"),
    ("en", "English", "ltr"),
    ("es", "Spanish", "ltr"),
    ("fa", "Persian", "rtl"),
    ("fi", "Finnish", "ltr"),
    ("fr", "French", "ltr"),
    ("he", "Hebrew", "rtl"),
    ("hi", "Hindi", "ltr"),
    ("hu", "Hungarian", "ltr"),
    ("id", "Indonesian", "ltr"),
    ("it", "Italian", "ltr"),
    ("ja", "Japanese", "ltr"),
    ("ko", "Korean", "ltr"),
    ("nb", "Norwegian Bokmål", "ltr"),
    ("nl", "Dutch", "ltr"),
    ("pl", "Polish", "ltr"),
    ("pt-br", "Portuguese, Brazil", "ltr"),
    ("pt-pt", "Portuguese, Portugal", "ltr"),
    ("ro", "Romanian", "ltr"),
    ("ru", "Russian", "ltr"),
    ("sv", "Swedish", "ltr"),
    ("tr", "Turkish", "ltr"),
    ("uk", "Ukrainian", "ltr"),
    ("ur", "Urdu", "rtl"),
    ("vi", "Vietnamese", "ltr"),
    ("zh-hans", "Chinese, Simplified", "ltr"),
    ("zh-hant", "Chinese, Traditional", "ltr"),
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(list_languages).post(save_settings))
        .route(&format!("{BASE}/add"), get(add_form).post(add_submit))
        .route(
            &format!("{BASE}/{{code}}/edit"),
            get(edit_form).post(edit_submit),
        )
        .route(
            &format!("{BASE}/{{code}}/delete"),
            get(delete_form).post(delete_submit),
        )
}

/// Read the languages into this server now and tell the others.
async fn languages_changed(state: &AppState) {
    if let Err(e) = state.reload_languages().await {
        tracing::error!(error = %e, "failed to reload languages after a change");
    }
    generation::bump_all(&[CacheKind::Languages, CacheKind::Menus]);
}

// =============================================================================
// List and settings
// =============================================================================

fn settings_form(prefix_default: bool, default_code: &str) -> crate::form::Form {
    crate::form::Form::new(SETTINGS_FORM_ID)
        .action(BASE)
        .element(
            "prefix_default",
            FormElement::checkbox()
                .title(format!(
                    "Serve the default language under its prefix too (/{default_code}/...)"
                ))
                .description(
                    "On: pages in the default language are also served under its prefix, and \
                     the language switcher and menus link there. Off: an address with the \
                     default language's prefix is sent, permanently, to the same address \
                     without it.",
                )
                .default_value(prefix_default)
                .weight(0),
        )
        .element("op", FormElement::submit("Save configuration").weight(10))
}

#[derive(Debug, Serialize)]
struct LanguageRow {
    id: String,
    label: String,
    direction: String,
    weight: i32,
    is_default: bool,
    prefix: String,
}

async fn render_list(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    errors: &[ValidationError],
) -> Response {
    let languages = match Language::list_all(state.db()).await {
        Ok(languages) => languages,
        Err(e) => {
            tracing::error!(error = %e, "failed to list languages");
            return render_server_error("The languages could not be loaded.");
        }
    };
    let set = state.languages();
    let rows: Vec<LanguageRow> = languages
        .into_iter()
        .map(|l| LanguageRow {
            prefix: set.prefix(&l.id),
            id: l.id,
            label: l.label,
            direction: l.direction,
            weight: l.weight,
            is_default: l.is_default,
        })
        .collect();
    let form_html = match admin_form::render(
        state,
        session,
        user,
        settings_form(set.prefix_default(), set.default_language()),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    let viewer = super::helpers::user_context_for(state, user).await;

    let mut context = tera::Context::new();
    context.insert("languages", &rows);
    context.insert("settings_form", &form_html);
    context.insert("flash", &flash);
    context.insert("path", BASE);
    render_admin_template(state, &viewer, "admin/languages.html", context).await
}

/// GET /admin/config/languages
async fn list_languages(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_list(&state, &session, &user, &[]).await
}

/// POST /admin/config/languages: the default language prefix setting.
async fn save_settings(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, SETTINGS_FORM_ID, vec![]).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => return render_list(&state, &session, &user, &errors).await,
            Err(Err(response)) => return response,
        };
    let on = posted.contains("prefix_default");
    if let Err(e) = SiteConfig::set(state.db(), PREFIX_DEFAULT_KEY, serde_json::json!(on)).await {
        tracing::error!(error = %e, "failed to save the default language prefix setting");
        return render_server_error("The setting could not be saved.");
    }
    tracing::info!(log_type = "config", user_id = %user.id, prefix_default = on, "language settings saved");
    languages_changed(&state).await;
    admin_form::submitted(&state, &user, &form_state, &posted).await;
    let _ = session
        .insert(FLASH_KEY, "The configuration options have been saved.")
        .await;
    Redirect::to(BASE).into_response()
}

// =============================================================================
// Add and edit
// =============================================================================

/// The add form: a common language, or a code of the site's own.
fn add_language_form(values: &[String; 5]) -> crate::form::Form {
    let mut options: Vec<(String, String)> = PREDEFINED
        .iter()
        .map(|(code, name, _)| ((*code).to_string(), format!("{name} ({code})")))
        .collect();
    options.push((CUSTOM.to_string(), "Custom language...".to_string()));
    crate::form::Form::new(LANGUAGE_FORM_ID)
        .action(format!("{BASE}/add"))
        .element(
            "predefined",
            FormElement::select(options)
                .title("Language")
                .description(
                    "Choose a common language, or Custom language to give the code and name below.",
                )
                .default_value(if values[0].is_empty() {
                    "es"
                } else {
                    values[0].as_str()
                })
                .required()
                .weight(0),
        )
        .element(
            "code",
            FormElement::textfield()
                .title("Language code (custom language only)")
                .description(
                    "A BCP 47 code: two or three lowercase letters and optional subtags (pt-br, \
                     zh-hans), or x- and a private use subtag (x-pirate). It is the address \
                     prefix: /code/...",
                )
                .default_value(values[1].clone())
                .max_length(12)
                .weight(1),
        )
        .element(
            "label",
            FormElement::textfield()
                .title("Name (custom language only)")
                .default_value(values[2].clone())
                .max_length(255)
                .weight(2),
        )
        .element(
            "direction",
            direction_element(if values[3].is_empty() {
                "ltr"
            } else {
                &values[3]
            })
            .weight(3),
        )
        .element("weight", weight_element(&values[4]).weight(4))
        .element("op", FormElement::submit("Add language").weight(10))
}

fn direction_element(value: &str) -> FormElement {
    FormElement::radio(vec![
        ("ltr".to_string(), "Left to right".to_string()),
        ("rtl".to_string(), "Right to left".to_string()),
    ])
    .title("Direction")
    .description("Ignored for a common language, which has its own.")
    .default_value(value)
}

fn weight_element(value: &str) -> FormElement {
    FormElement::textfield()
        .title("Weight")
        .description("Languages are listed, and offered in the language switcher, lightest first.")
        .default_value(if value.is_empty() { "0" } else { value }.to_string())
        .max_length(6)
}

/// The edit form for the language `code`.
fn edit_language_form(code: &str, values: &[String; 4]) -> crate::form::Form {
    crate::form::Form::new(LANGUAGE_FORM_ID)
        .action(format!("{BASE}/{code}/edit"))
        .element(
            "label",
            FormElement::textfield()
                .title("Name")
                .default_value(values[0].clone())
                .max_length(255)
                .required()
                .weight(0),
        )
        .element(
            "direction",
            FormElement::radio(vec![
                ("ltr".to_string(), "Left to right".to_string()),
                ("rtl".to_string(), "Right to left".to_string()),
            ])
            .title("Direction")
            .default_value(values[1].as_str())
            .required()
            .weight(1),
        )
        .element("weight", weight_element(&values[2]).weight(2))
        .element(
            "is_default",
            FormElement::checkbox()
                .title("Default language")
                .description(
                    "Content is written in the default language unless it says otherwise, and \
                     its pages need no prefix.",
                )
                .default_value(values[3] == "1")
                .weight(3),
        )
        .element("op", FormElement::submit("Save language").weight(10))
}

fn parse_weight(raw: &str, errors: &mut Vec<ValidationError>) -> i32 {
    match raw.trim() {
        "" => 0,
        w => w.parse::<i32>().unwrap_or_else(|_| {
            errors.push(ValidationError::field(
                "weight",
                "Weight must be a whole number.",
            ));
            0
        }),
    }
}

async fn render_form(
    state: &AppState,
    user: &crate::models::User,
    session: &Session,
    title: &str,
    action: &str,
    form: crate::form::Form,
    errors: &[ValidationError],
) -> Response {
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("title", title);
    context.insert("form", &form_html);
    context.insert("path", action);
    render_admin_template(state, &viewer, "admin/language-form.html", context).await
}

/// GET /admin/config/languages/add
async fn add_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let action = format!("{BASE}/add");
    let form = add_language_form(&Default::default());
    render_form(&state, &user, &session, "Add language", &action, form, &[]).await
}

/// POST /admin/config/languages/add
async fn add_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let field = |name: &str| posted.get(name).unwrap_or_default().trim().to_string();
    let values = [
        field("predefined"),
        field("code"),
        field("label"),
        field("direction"),
        field("weight"),
    ];
    let mut errors = Vec::new();
    let weight = parse_weight(&values[4], &mut errors);
    let (code, label, direction) = if values[0] == CUSTOM {
        if values[1].is_empty() {
            errors.push(ValidationError::field("code", "Enter the language code."));
        }
        if values[2].is_empty() {
            errors.push(ValidationError::field("label", "Enter the language name."));
        }
        let direction = if values[3] == "rtl" { "rtl" } else { "ltr" };
        (
            values[1].to_lowercase(),
            values[2].clone(),
            direction.to_string(),
        )
    } else if let Some((code, name, direction)) =
        PREDEFINED.iter().find(|(code, _, _)| *code == values[0])
    {
        (
            (*code).to_string(),
            (*name).to_string(),
            (*direction).to_string(),
        )
    } else {
        errors.push(ValidationError::field("predefined", "Choose a language."));
        (String::new(), String::new(), String::new())
    };
    if errors.is_empty() {
        match Language::find_by_id(state.db(), &code).await {
            Ok(Some(_)) => errors.push(ValidationError::field(
                if values[0] == CUSTOM {
                    "code"
                } else {
                    "predefined"
                },
                "The site already has this language.",
            )),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to look up a language");
                return render_server_error("The language could not be added.");
            }
        }
    }
    let action = format!("{BASE}/add");
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, LANGUAGE_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                let form = add_language_form(&values);
                return render_form(
                    &state,
                    &user,
                    &session,
                    "Add language",
                    &action,
                    form,
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };
    let input = CreateLanguage {
        id: code.clone(),
        label: label.clone(),
        weight: Some(weight),
        is_default: Some(false),
        direction: Some(direction),
    };
    if let Err(e) = Language::upsert(state.db(), input).await {
        // The model's own checks (the code's shape, the name's length).
        let errors = vec![ValidationError::field(
            if values[0] == CUSTOM {
                "code"
            } else {
                "predefined"
            },
            format!("The language could not be added: {e}"),
        )];
        let form = add_language_form(&values);
        return render_form(
            &state,
            &user,
            &session,
            "Add language",
            &action,
            form,
            &errors,
        )
        .await;
    }
    tracing::info!(log_type = "config", user_id = %user.id, language = %code, "language added");
    languages_changed(&state).await;
    admin_form::submitted(&state, &user, &form_state, &posted).await;
    let _ = session
        .insert(FLASH_KEY, format!("The language {label} has been created."))
        .await;
    Redirect::to(BASE).into_response()
}

async fn load(state: &AppState, code: &str) -> Result<Language, Response> {
    match Language::find_by_id(state.db(), code).await {
        Ok(Some(language)) => Ok(language),
        Ok(None) => Err(render_not_found()),
        Err(e) => {
            tracing::error!(error = %e, "failed to load a language");
            Err(render_server_error("The language could not be loaded."))
        }
    }
}

/// GET /admin/config/languages/{code}/edit
async fn edit_form(
    State(state): State<AppState>,
    session: Session,
    Path(code): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let language = match load(&state, &code).await {
        Ok(language) => language,
        Err(response) => return response,
    };
    let values = [
        language.label.clone(),
        language.direction.clone(),
        language.weight.to_string(),
        if language.is_default { "1" } else { "" }.to_string(),
    ];
    let action = format!("{BASE}/{code}/edit");
    let form = edit_language_form(&code, &values);
    render_form(&state, &user, &session, "Edit language", &action, form, &[]).await
}

/// POST /admin/config/languages/{code}/edit
async fn edit_submit(
    State(state): State<AppState>,
    session: Session,
    Path(code): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let language = match load(&state, &code).await {
        Ok(language) => language,
        Err(response) => return response,
    };
    let field = |name: &str| posted.get(name).unwrap_or_default().trim().to_string();
    // The default stays the default: another language is made default from
    // its own form, which moves the flag, so the site is never without one.
    let make_default = language.is_default || posted.contains("is_default");
    let values = [
        field("label"),
        field("direction"),
        field("weight"),
        if make_default { "1" } else { "" }.to_string(),
    ];
    let mut errors = Vec::new();
    if values[0].is_empty() {
        errors.push(ValidationError::field("label", "Enter the language name."));
    }
    let weight = parse_weight(&values[2], &mut errors);
    let action = format!("{BASE}/{code}/edit");
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, LANGUAGE_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                let form = edit_language_form(&code, &values);
                return render_form(
                    &state,
                    &user,
                    &session,
                    "Edit language",
                    &action,
                    form,
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };
    let input = CreateLanguage {
        id: code.clone(),
        label: values[0].clone(),
        weight: Some(weight),
        is_default: Some(make_default),
        direction: Some(if values[1] == "rtl" { "rtl" } else { "ltr" }.to_string()),
    };
    if let Err(e) = Language::upsert(state.db(), input).await {
        let errors = vec![ValidationError::form(format!(
            "The language could not be saved: {e}"
        ))];
        let form = edit_language_form(&code, &values);
        return render_form(
            &state,
            &user,
            &session,
            "Edit language",
            &action,
            form,
            &errors,
        )
        .await;
    }
    tracing::info!(log_type = "config", user_id = %user.id, language = %code, "language saved");
    languages_changed(&state).await;
    admin_form::submitted(&state, &user, &form_state, &posted).await;
    let _ = session
        .insert(
            FLASH_KEY,
            format!("The language {} has been saved.", values[0]),
        )
        .await;
    Redirect::to(BASE).into_response()
}

// =============================================================================
// Delete
// =============================================================================

fn delete_form_for(code: &str) -> crate::form::Form {
    crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{code}/delete"))
        .element("op", FormElement::submit("Delete").weight(10))
}

async fn render_delete(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    language: &Language,
    errors: &[ValidationError],
) -> Response {
    let form_html =
        match admin_form::render(state, session, user, delete_form_for(&language.id), errors).await
        {
            Ok(html) => html,
            Err(response) => return response,
        };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("language", &language.label);
    context.insert("code", &language.id);
    context.insert("form", &form_html);
    context.insert("path", &format!("{BASE}/{}/delete", language.id));
    render_admin_template(state, &viewer, "admin/language-delete.html", context).await
}

/// GET /admin/config/languages/{code}/delete
async fn delete_form(
    State(state): State<AppState>,
    session: Session,
    Path(code): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let language = match load(&state, &code).await {
        Ok(language) => language,
        Err(response) => return response,
    };
    render_delete(&state, &session, &user, &language, &[]).await
}

/// POST /admin/config/languages/{code}/delete
async fn delete_submit(
    State(state): State<AppState>,
    session: Session,
    Path(code): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let language = match load(&state, &code).await {
        Ok(language) => language,
        Err(response) => return response,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, DELETE_FORM_ID, vec![]).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_delete(&state, &session, &user, &language, &errors).await;
            }
            Err(Err(response)) => return response,
        };
    // The model refuses the default language and a language anything is
    // written in, in the same transaction as the delete.
    if let Err(e) = Language::delete(state.db(), &code).await {
        let errors = vec![ValidationError::form(format!(
            "The language could not be deleted: {e}"
        ))];
        return render_delete(&state, &session, &user, &language, &errors).await;
    }
    tracing::info!(log_type = "config", user_id = %user.id, language = %code, "language deleted");
    languages_changed(&state).await;
    admin_form::submitted(&state, &user, &form_state, &posted).await;
    let _ = session
        .insert(
            FLASH_KEY,
            format!("The language {} has been removed.", language.label),
        )
        .await;
    Redirect::to(BASE).into_response()
}
