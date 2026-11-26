//! Text formats admin (B108): `/admin/config/formats`.
//!
//! List the stored formats (`content::text_format`), edit one's name, order,
//! filter, allowed tags and attributes and which roles may use it, add a
//! format, and delete one that is not built in. Every route takes
//! `administer text formats`; every form goes through `FormService`.
//!
//! Which roles may use a format is the `use <format>` permission on each role,
//! the same grant the permission grid shows. A non-superuser may grant or
//! revoke it only when they hold it themselves, as on the grid.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::submission::PostedForm;
use crate::content::text_format::{self, FilterKind, TextFormat};
use crate::form::{FormElement, ValidationError};
use crate::models::User;
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    render_admin_template, render_forbidden, render_not_found, render_server_error,
    require_permission,
};

/// The permission every route here takes.
pub const PERMISSION: &str = "administer text formats";

const BASE: &str = "/admin/config/formats";
const FORMAT_FORM_ID: &str = "text_format_form";
const DELETE_FORM_ID: &str = "text_format_delete_form";
const FLASH_KEY: &str = "text_formats_flash";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(list_formats))
        .route(&format!("{BASE}/add"), get(add_form).post(add_submit))
        .route(
            &format!("{BASE}/{{format}}/edit"),
            get(edit_form).post(edit_submit),
        )
        .route(
            &format!("{BASE}/{{format}}/delete"),
            get(delete_form).post(delete_submit),
        )
}

// =============================================================================
// Roles
// =============================================================================

/// Each role, and whether it holds `permission`.
async fn role_grants(
    state: &AppState,
    permission: &str,
) -> anyhow::Result<Vec<(Uuid, String, bool)>> {
    let mut grants = Vec::new();
    for role in state.roles().list().await? {
        let held = state
            .roles()
            .get_permissions(role.id)
            .await?
            .iter()
            .any(|p| p == permission);
        grants.push((role.id, role.name, held));
    }
    Ok(grants)
}

/// Whether `user` may change who holds `permission`: a superuser always, and
/// anyone else only for a permission they hold themselves.
async fn may_grant(state: &AppState, user: &User, permission: &str) -> bool {
    user.is_admin
        || super::helpers::user_context_for(state, user)
            .await
            .can(permission)
}

// =============================================================================
// List
// =============================================================================

#[derive(Debug, Serialize)]
struct FormatRow {
    format: String,
    name: String,
    weight: i32,
    filter: &'static str,
    tags: String,
    roles: Vec<String>,
    everyone: bool,
    built_in: bool,
}

/// GET /admin/config/formats
async fn list_formats(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let formats = match text_format::list(state.db()).await {
        Ok(formats) => formats,
        Err(e) => {
            tracing::error!(error = %e, "failed to list text formats");
            return render_server_error("The text formats could not be loaded.");
        }
    };
    let mut rows = Vec::new();
    for format in formats {
        let (roles, everyone) = match format.permission() {
            Some(permission) => (
                role_grants(&state, &permission)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, _, held)| *held)
                    .map(|(_, name, _)| name)
                    .collect(),
                false,
            ),
            None => (Vec::new(), true),
        };
        rows.push(FormatRow {
            tags: if format.filter == FilterKind::Html {
                text_format::format_tags(&format.allowed_tags)
            } else {
                String::new()
            },
            filter: format.filter.label(),
            built_in: format.is_built_in(),
            format: format.format,
            name: format.name,
            weight: format.weight,
            roles,
            everyone,
        });
    }
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("formats", &rows);
    context.insert("flash", &flash);
    context.insert("path", BASE);
    render_admin_template(&state, &viewer, "admin/text-formats.html", context).await
}

// =============================================================================
// Add and edit
// =============================================================================

/// What the form shows: a stored format, or what was posted.
struct Values {
    format: String,
    name: String,
    weight: String,
    filter: String,
    tags: String,
    attributes: String,
    convert_newlines: bool,
    convert_urls: bool,
    roles: Vec<String>,
}

impl Values {
    fn of(format: &TextFormat, roles: Vec<String>) -> Self {
        Self {
            format: format.format.clone(),
            name: format.name.clone(),
            weight: format.weight.to_string(),
            filter: format.filter.as_str().to_string(),
            tags: text_format::format_tags(&format.allowed_tags),
            attributes: text_format::format_attributes(&format.allowed_attributes),
            convert_newlines: format.convert_newlines,
            convert_urls: format.convert_urls,
            roles,
        }
    }

    fn posted(posted: &PostedForm, existing: Option<&TextFormat>) -> Self {
        let get = |name: &str| posted.get(name).unwrap_or_default().to_string();
        Self {
            format: existing
                .map(|f| f.format.clone())
                .unwrap_or_else(|| get("format").trim().to_string()),
            name: get("name").trim().to_string(),
            weight: get("weight").trim().to_string(),
            // A built-in format's filter is not on its form.
            filter: existing
                .filter(|f| f.is_built_in())
                .map(|f| f.filter.as_str().to_string())
                .unwrap_or_else(|| get("filter")),
            tags: get("allowed_tags"),
            attributes: get("allowed_attributes"),
            convert_newlines: posted.contains("convert_newlines"),
            convert_urls: posted.contains("convert_urls"),
            roles: posted
                .0
                .iter()
                .filter(|(k, _)| k == "roles[]" || k == "roles")
                .map(|(_, v)| v.clone())
                .collect(),
        }
    }

    /// The format these values describe, or the errors to show.
    fn parse(&self, existing: Option<&TextFormat>) -> Result<TextFormat, Vec<ValidationError>> {
        let mut errors = Vec::new();
        let weight = match self.weight.parse::<i32>() {
            Ok(w) if (-100..=100).contains(&w) => w,
            _ if self.weight.is_empty() => 0,
            _ => {
                errors.push(ValidationError::field(
                    "weight",
                    "Weight must be a whole number from -100 to 100.",
                ));
                0
            }
        };
        let filter = FilterKind::parse(&self.filter).unwrap_or_else(|| {
            errors.push(ValidationError::field("filter", "Choose a filter."));
            FilterKind::Escape
        });
        let attributes = match text_format::parse_attributes(&self.attributes) {
            Ok(attributes) => attributes,
            Err(message) => {
                errors.push(ValidationError::field("allowed_attributes", message));
                Default::default()
            }
        };
        let format = TextFormat {
            format: self.format.clone(),
            name: self.name.clone(),
            weight,
            filter,
            allowed_tags: if filter == FilterKind::Html {
                text_format::parse_tags(&self.tags)
            } else {
                Vec::new()
            },
            allowed_attributes: if filter == FilterKind::Html {
                attributes
            } else {
                Default::default()
            },
            convert_newlines: self.convert_newlines,
            convert_urls: self.convert_urls,
        };
        errors.extend(
            text_format::validate(&format, existing)
                .into_iter()
                .map(|(field, message)| ValidationError::field(field, message)),
        );
        if errors.is_empty() {
            Ok(format)
        } else {
            Err(errors)
        }
    }
}

fn format_form(
    action: &str,
    values: &Values,
    editing: Option<&TextFormat>,
    roles: &[(Uuid, String, bool)],
    roles_editable: bool,
    text: &super::admin_nav::AdminText,
) -> crate::form::Form {
    let mut form = crate::form::Form::new(FORMAT_FORM_ID)
        .action(action)
        .element(
            "name",
            FormElement::textfield()
                .title("Name")
                .description("What editors see in the text format selector.")
                .default_value(values.name.clone())
                .max_length(255)
                .required()
                .weight(0),
        );
    form = match editing {
        None => form.element(
            "format",
            FormElement::textfield()
                .title("Machine name")
                .description(
                    "Lowercase letters, digits and underscores. It cannot be changed later; \
                     the permission that grants the format is \"use <machine name>\".",
                )
                .default_value(values.format.clone())
                .max_length(64)
                .required()
                .weight(1),
        ),
        Some(format) => form.element(
            "format_markup",
            FormElement::markup(format!(
                "<p class=\"form-item__description\">{}</p>",
                text.html("Machine name: {name}", &[("name", &format.format)])
            ))
            .weight(1),
        ),
    };
    let built_in = editing.is_some_and(TextFormat::is_built_in);
    form = if built_in {
        form.element(
            "filter_markup",
            FormElement::markup(format!(
                "<p class=\"form-item__description\">{}</p>",
                text.html(
                    "Filter: {filter} (fixed for a built-in format)",
                    &[(
                        "filter",
                        FilterKind::parse(&values.filter)
                            .map(FilterKind::label)
                            .unwrap_or_default()
                    )]
                )
            ))
            .weight(2),
        )
    } else {
        form.element(
            "filter",
            FormElement::radio(
                [FilterKind::Html, FilterKind::Escape, FilterKind::None]
                    .iter()
                    .map(|k| (k.as_str().to_string(), k.label().to_string()))
                    .collect(),
            )
            .title("Filter")
            .description(
                "No filtering cleans text with a fixed, permissive rule rather than a \
                 configured allowlist: ordinary rich markup (headings, lists, tables, \
                 images, classes) is kept, and a <script>, an event handler and a \
                 javascript: URL are always removed.",
            )
            .default_value(if values.filter.is_empty() {
                "html".to_string()
            } else {
                values.filter.clone()
            })
            .required()
            .weight(2),
        )
    };
    let shows_html = !built_in || values.filter == FilterKind::Html.as_str();
    if shows_html {
        form = form
            .element(
                "allowed_tags",
                FormElement::textfield()
                    .title("Allowed HTML tags")
                    .description(
                        "For the \"Limit to allowed tags\" filter: the tags kept, such as \
                         <p> <a> <em>. Every other tag is removed; <script> and <style> \
                         can never be allowed.",
                    )
                    .default_value(values.tags.clone())
                    .max_length(2000)
                    .weight(3),
            )
            .element(
                "allowed_attributes",
                FormElement::textarea(5)
                    .title("Allowed attributes")
                    .description(
                        "One allowed tag per line: a: href title target. Event handlers \
                         (on…) and style are never kept.",
                    )
                    .default_value(values.attributes.clone())
                    .weight(4),
            );
    }
    form = form
        .element(
            "convert_newlines",
            FormElement::checkbox()
                .title("Convert line breaks into <br>")
                .default_value(values.convert_newlines)
                .weight(5),
        )
        .element(
            "convert_urls",
            FormElement::checkbox()
                .title("Turn web addresses into links")
                .default_value(values.convert_urls)
                .weight(6),
        )
        .element(
            "weight",
            FormElement::input("number")
                .title("Weight")
                .description("Lighter formats are listed first in the selector.")
                .default_value(values.weight.clone())
                .weight(7),
        );
    let everyone = editing.is_some_and(|f| f.permission().is_none());
    form = if everyone {
        form.element(
            "roles_markup",
            FormElement::markup(
                "<p class=\"form-item__description\">Every role may use this format: it is \
                 what any user can always write in.</p>"
                    .to_string(),
            )
            .weight(8),
        )
    } else {
        let mut element = FormElement::checkboxes(
            roles
                .iter()
                .map(|(id, name, _)| (id.to_string(), name.clone()))
                .collect(),
        )
        .title("Roles")
        .description(if roles_editable {
            "The roles whose members may use this format: the \"use\" permission for it."
        } else {
            "You may change this only for a permission you hold yourself."
        })
        .default_value(serde_json::json!(values.roles))
        .weight(8);
        if !roles_editable {
            element = element.disabled();
        }
        form.element("roles", element)
    };
    form.element("op", FormElement::submit("Save format").weight(20))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

async fn render_format_form(
    state: &AppState,
    session: &Session,
    user: &User,
    editing: Option<&TextFormat>,
    values: &Values,
    errors: &[ValidationError],
) -> Response {
    let action = match editing {
        Some(f) => format!("{BASE}/{}/edit", f.format),
        None => format!("{BASE}/add"),
    };
    let permission =
        text_format::permission_for(&values.format).unwrap_or_else(|| "use new format".to_string());
    let roles = role_grants(state, &permission).await.unwrap_or_default();
    let roles_editable = editing.is_none() || may_grant(state, user, &permission).await;
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, &action).await;
    let form_html = match admin_form::render(
        state,
        session,
        user,
        format_form(&action, values, editing, &roles, roles_editable, &text),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    // The heading is the template's, one translatable sentence each way.
    context.insert("editing_name", &editing.map(|f| f.name.clone()));
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/text-format-form.html", context).await
}

/// GET /admin/config/formats/add
async fn add_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let values = Values {
        format: String::new(),
        name: String::new(),
        weight: "0".to_string(),
        filter: "html".to_string(),
        tags: "<p> <br> <strong> <em> <a> <ul> <ol> <li>".to_string(),
        attributes: "a: href title".to_string(),
        convert_newlines: false,
        convert_urls: true,
        roles: Vec::new(),
    };
    render_format_form(&state, &session, &user, None, &values, &[]).await
}

/// POST /admin/config/formats/add
async fn add_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    save(state, session, PostedForm(pairs), None).await
}

/// GET /admin/config/formats/{format}/edit
async fn edit_form(
    State(state): State<AppState>,
    session: Session,
    Path(format): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let stored = match text_format::find(state.db(), &format).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load text format");
            return render_server_error("The text format could not be loaded.");
        }
    };
    let roles = match stored.permission() {
        Some(permission) => role_grants(&state, &permission)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, _, held)| *held)
            .map(|(id, _, _)| id.to_string())
            .collect(),
        None => Vec::new(),
    };
    let values = Values::of(&stored, roles);
    render_format_form(&state, &session, &user, Some(&stored), &values, &[]).await
}

/// POST /admin/config/formats/{format}/edit
async fn edit_submit(
    State(state): State<AppState>,
    session: Session,
    Path(format): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    save(state, session, PostedForm(pairs), Some(format)).await
}

async fn save(
    state: AppState,
    session: Session,
    posted: PostedForm,
    editing: Option<String>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let existing = match &editing {
        Some(format) => match text_format::find(state.db(), format).await {
            Ok(Some(stored)) => Some(stored),
            Ok(None) => return render_not_found(),
            Err(e) => {
                tracing::error!(error = %e, "failed to load text format");
                return render_server_error("The text format could not be loaded.");
            }
        },
        None => None,
    };
    let values = Values::posted(&posted, existing.as_ref());
    let parsed = values.parse(existing.as_ref());
    let mut errors = parsed.as_ref().err().cloned().unwrap_or_default();
    if existing.is_none()
        && parsed.is_ok()
        && matches!(
            text_format::find(state.db(), &values.format).await,
            Ok(Some(_))
        )
    {
        errors.push(ValidationError::field(
            "format",
            format!("A text format named {} already exists.", values.format),
        ));
    }
    match admin_form::check(&state, &session, &user, &posted, FORMAT_FORM_ID, errors).await {
        Ok(()) => {}
        Err(Ok(errors)) => {
            return render_format_form(
                &state,
                &session,
                &user,
                existing.as_ref(),
                &values,
                &errors,
            )
            .await;
        }
        Err(Err(response)) => return response,
    }
    let Ok(format) = parsed else {
        return render_not_found();
    };
    if let Err(e) = text_format::save(state.db(), &format).await {
        tracing::error!(error = %e, "failed to save text format");
        return render_server_error("The text format could not be saved.");
    }

    // Who may use it: the `use <format>` grant on each role, within what the
    // editor holds.
    if let Some(permission) = format.permission()
        && may_grant(&state, &user, &permission).await
    {
        let wanted: Vec<Uuid> = values.roles.iter().filter_map(|r| r.parse().ok()).collect();
        match role_grants(&state, &permission).await {
            Ok(grants) => {
                for (role_id, _, held) in grants {
                    let want = wanted.contains(&role_id);
                    let result = if want && !held {
                        state.roles().add_permission(role_id, &permission).await
                    } else if !want && held {
                        state.roles().remove_permission(role_id, &permission).await
                    } else {
                        Ok(())
                    };
                    if let Err(e) = result {
                        tracing::error!(error = %e, %role_id, "failed to change a format grant");
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to read the format's grants"),
        }
    }

    text_format::changed(state.db()).await;
    tracing::info!(
        log_type = "config",
        user_id = %user.id,
        format = %format.format,
        "text format saved"
    );
    let _ = session
        .insert(
            FLASH_KEY,
            format!("The {} text format was saved.", format.name),
        )
        .await;
    Redirect::to(BASE).into_response()
}

// =============================================================================
// Delete
// =============================================================================

/// The stored format named `format`, if it may be deleted.
async fn deletable(state: &AppState, format: &str) -> Result<TextFormat, Response> {
    match text_format::find(state.db(), format).await {
        Ok(Some(stored)) if stored.is_built_in() => Err(render_forbidden(
            "A built-in text format cannot be deleted.",
        )),
        Ok(Some(stored)) => Ok(stored),
        Ok(None) => Err(render_not_found()),
        Err(e) => {
            tracing::error!(error = %e, "failed to load text format");
            Err(render_server_error("The text format could not be loaded."))
        }
    }
}

/// GET /admin/config/formats/{format}/delete: confirm.
async fn delete_form(
    State(state): State<AppState>,
    session: Session,
    Path(format): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let stored = match deletable(&state, &format).await {
        Ok(stored) => stored,
        Err(response) => return response,
    };
    let form = crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{}/delete", stored.format))
        .element("op", FormElement::submit("Delete").weight(10));
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("name", &stored.name);
    context.insert("format", &stored.format);
    context.insert("form", &form_html);
    context.insert("path", &format!("{BASE}/{}/delete", stored.format));
    render_admin_template(&state, &viewer, "admin/text-format-delete.html", context).await
}

/// POST /admin/config/formats/{format}/delete
async fn delete_submit(
    State(state): State<AppState>,
    session: Session,
    Path(format): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let stored = match deletable(&state, &format).await {
        Ok(stored) => stored,
        Err(response) => return response,
    };
    match admin_form::check(&state, &session, &user, &posted, DELETE_FORM_ID, Vec::new()).await {
        Ok(()) => {}
        Err(Ok(_)) => {
            return super::helpers::render_error(
                "The form has expired. Reload the page and try again.",
            );
        }
        Err(Err(response)) => return response,
    }
    match text_format::delete(state.db(), &stored.format).await {
        Ok(_) => {
            state.permissions().invalidate_all();
            text_format::changed(state.db()).await;
            tracing::info!(log_type = "config", user_id = %user.id, format = %stored.format, "text format deleted");
            let _ = session
                .insert(
                    FLASH_KEY,
                    format!("The {} text format was deleted.", stored.name),
                )
                .await;
            Redirect::to(BASE).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to delete text format");
            render_server_error("The text format could not be deleted.")
        }
    }
}
