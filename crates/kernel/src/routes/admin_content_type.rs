//! Admin routes for content type management.
//!
//! Every form these screens serve is built and checked by the kernel's form
//! pipeline (`super::admin_form`): the content type add and edit form, the
//! add-field form on the field list, the field edit form, the field delete
//! confirmation, and the search settings page's forms.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::content::{AddFieldError, CreateContentTypeError};
use crate::form::{FormElement, ValidationError};
use crate::search::SearchSwitches;
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    MACHINE_NAME_ERROR, html_escape, is_valid_machine_name, render_admin_template, render_error,
    render_not_found, render_server_error, require_permission,
};

// =============================================================================
// Form data
// =============================================================================

/// The form id of the content type add and edit form.
const CONTENT_TYPE_FORM_ID: &str = "content_type_form";

/// The form id of the field edit form.
const FIELD_EDIT_FORM_ID: &str = "field_edit_form";

/// The form id of the field delete confirmation.
const FIELD_DELETE_FORM_ID: &str = "field_delete_form";

/// The form id of the add-field form on the field list of `type_name`.
///
/// It names the content type because the form's "Add field" button also
/// works over AJAX, and [`handle_ajax_add_field`] reads which content type
/// to add the field to from the saved form state's id, never from the
/// request.
fn field_add_form_id(type_name: &str) -> String {
    format!("manage_fields_{type_name}")
}

/// The field types the add-field form offers: value and English label,
/// translated by the select template.
const FIELD_TYPE_OPTIONS: &[(&str, &str)] = &[
    ("text", "Text (plain)"),
    ("text_long", "Text (long)"),
    ("integer", "Integer"),
    ("float", "Float"),
    ("boolean", "Boolean"),
    ("date", "Date"),
    ("email", "Email"),
    ("record_reference", "Reference"),
    ("category_reference", "Category reference (tags)"),
    ("link", "Link"),
    ("list", "List (fixed choices)"),
    ("telephone", "Telephone"),
];

/// The preview choices of a content type: value and English label,
/// translated by the select template. Each label says what it means on its
/// own, so none needs a translation context.
const PREVIEW_OPTIONS: &[(&str, &str)] = &[
    ("disabled", "No preview"),
    ("optional", "Preview offered"),
    ("required", "Preview required before saving"),
];

/// The choices of the comment settings fieldset, value and label.
const COMMENT_MODE_OPTIONS: &[(&str, &str)] = &[
    ("open", "Open"),
    ("closed", "Closed: shown, no new comments"),
    ("hidden", "Hidden: not shown, no new comments"),
];
const COMMENT_THREADING_OPTIONS: &[(&str, &str)] = &[
    (
        "threaded",
        "Threaded: replies under the comment they answer",
    ),
    ("flat", "Flat: every comment in date order"),
];
const COMMENT_ORDER_OPTIONS: &[(&str, &str)] =
    &[("oldest", "Oldest first"), ("newest", "Newest first")];

/// A constant option list as the form's select takes it.
fn options(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(value, label)| ((*value).to_string(), (*label).to_string()))
        .collect()
}

/// The string at `key` of a form's values, or `fallback` when it is missing
/// or empty.
fn value_or(values: &serde_json::Value, key: &str, fallback: &str) -> String {
    match values.get(key).and_then(serde_json::Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => fallback.to_string(),
    }
}

/// The Cancel link beside a form's button, back to `href`, in the viewer's
/// admin language.
async fn cancel_link(state: &AppState, user: &crate::models::User, href: &str) -> String {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, href).await;
    format!(
        r#"<a href="{}" class="button button--secondary">{}</a>"#,
        html_escape(href),
        text.html("Cancel", &[])
    )
}

/// Content type form data, as posted.
#[derive(Debug)]
struct ContentTypeFormData {
    label: String,
    machine_name: String,
    description: Option<String>,
    title_label: Option<String>,
    published_default: Option<String>,
    revision_default: Option<String>,
    promote_default: Option<String>,
    sticky_default: Option<String>,
    moderation: Option<String>,
    preview_mode: Option<String>,
    comment_mode: Option<String>,
    comment_threading: Option<String>,
    comment_order: Option<String>,
    comment_per_page: Option<String>,
}

/// The per-type defaults the content type form edits, by settings key. Each
/// is a checkbox, stored as a boolean in the type's `settings`.
const DEFAULT_SWITCHES: &[&str] = &[
    "published_default",
    "revision_default",
    "promote_default",
    "sticky_default",
    // Content moderation (`content::moderation`): Draft, Published and
    // Archived, with a permission per transition.
    "moderation",
];

impl ContentTypeFormData {
    /// Read the posted fields.
    fn from_posted(posted: &PostedForm) -> Self {
        let get = |name: &str| posted.get(name).map(str::to_string);
        Self {
            label: get("label").unwrap_or_default(),
            machine_name: get("machine_name").unwrap_or_default(),
            description: get("description"),
            title_label: get("title_label"),
            published_default: get("published_default"),
            revision_default: get("revision_default"),
            promote_default: get("promote_default"),
            sticky_default: get("sticky_default"),
            moderation: get("moderation"),
            preview_mode: get("preview_mode"),
            comment_mode: get("comment_mode"),
            comment_threading: get("comment_threading"),
            comment_order: get("comment_order"),
            comment_per_page: get("comment_per_page"),
        }
    }

    /// Whether the checkbox for settings key `key` was checked.
    fn switch(&self, key: &str) -> bool {
        match key {
            "published_default" => self.published_default.is_some(),
            "revision_default" => self.revision_default.is_some(),
            "promote_default" => self.promote_default.is_some(),
            "sticky_default" => self.sticky_default.is_some(),
            "moderation" => self.moderation.is_some(),
            _ => false,
        }
    }

    /// The settings this submission stores, merged into what the type holds.
    fn settings(&self) -> serde_json::Value {
        let mut settings = serde_json::Map::new();
        settings.insert(
            "title_label".to_string(),
            serde_json::Value::from(
                self.title_label
                    .clone()
                    .unwrap_or_else(|| "Title".to_string()),
            ),
        );
        for key in DEFAULT_SWITCHES {
            settings.insert(
                (*key).to_string(),
                serde_json::Value::Bool(self.switch(key)),
            );
        }
        settings.insert(
            crate::content::preview::SETTING.to_string(),
            serde_json::Value::from(self.preview_mode().as_str()),
        );
        for (key, value) in self.comment_settings().to_settings() {
            settings.insert(key.to_string(), value);
        }
        serde_json::Value::Object(settings)
    }

    /// The preview mode this submission asks for; an unknown value is the
    /// default.
    fn preview_mode(&self) -> crate::content::preview::PreviewMode {
        crate::content::preview::PreviewMode::parse(self.preview_mode.as_deref())
    }

    /// The comment settings this submission asks for; anything unrecognised
    /// is the default.
    fn comment_settings(&self) -> crate::content::comment_settings::CommentSettings {
        use crate::content::comment_settings::{CommentMode, CommentSettings};
        CommentSettings {
            mode: CommentMode::parse(self.comment_mode.as_deref()),
            threaded: self.comment_threading.as_deref() != Some("flat"),
            newest_first: self.comment_order.as_deref() == Some("newest"),
            per_page: CommentSettings::clamp_per_page(
                self.comment_per_page
                    .as_deref()
                    .and_then(|v| v.trim().parse().ok()),
            ),
        }
    }

    /// The values to show this submission again with.
    fn values(&self) -> serde_json::Value {
        let mut values = serde_json::json!({
            "label": self.label,
            "machine_name": self.machine_name,
            "description": self.description,
            "title_label": self.title_label,
        });
        for key in DEFAULT_SWITCHES {
            values[*key] = serde_json::Value::Bool(self.switch(key));
        }
        values[crate::content::preview::SETTING] =
            serde_json::Value::from(self.preview_mode().as_str());
        for (key, value) in self.comment_settings().to_settings() {
            values[key] = value;
        }
        values
    }
}

/// The content type add and edit form, showing `values`. On the edit form
/// the machine name is read only. `cancel` is the Cancel link beside the
/// button.
fn content_type_form(
    action: &str,
    editing: bool,
    values: &serde_json::Value,
    cancel: String,
) -> crate::form::Form {
    let mut machine_name = FormElement::textfield()
        .title("Machine name")
        .description(
            "A unique machine-readable name. Can only contain lowercase letters, numbers, and \
             underscores.",
        )
        .default_value(value_or(values, "machine_name", ""))
        .required()
        .attribute("pattern", "[a-z][a-z0-9_]*")
        .attribute("spellcheck", "false")
        .attribute("autocapitalize", "none")
        .weight(1);
    if editing {
        machine_name = machine_name.attribute("readonly", "readonly");
    }
    let switch = |key: &str| {
        values
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let per_page = values
        .get(crate::content::comment_settings::PER_PAGE)
        .cloned()
        .unwrap_or_else(|| serde_json::Value::from(50));
    crate::form::Form::new(CONTENT_TYPE_FORM_ID)
        .action(action)
        .element(
            "label",
            FormElement::textfield()
                .title("Name")
                .description("The human-readable name of this content type.")
                .default_value(value_or(values, "label", ""))
                .required()
                .attribute("data-machine-name-source", "machine_name")
                .weight(0),
        )
        .element("machine_name", machine_name)
        .element(
            "description",
            FormElement::textarea(3)
                .title("Description")
                .description(
                    "Describe this content type. The text will be displayed on the content \
                     type administration page.",
                )
                .default_value(value_or(values, "description", ""))
                .weight(2),
        )
        .element(
            "submission_settings",
            FormElement::fieldset()
                .title("Submission form settings")
                .weight(3)
                .child(
                    "title_label",
                    FormElement::textfield()
                        .title("Title field label")
                        .default_value(value_or(values, "title_label", "Title")),
                ),
        )
        .element(
            "preview_settings",
            FormElement::fieldset()
                .title("Preview before submitting")
                .weight(4)
                .child(
                    "preview_mode",
                    FormElement::select(options(PREVIEW_OPTIONS))
                        .title("Preview")
                        .description(
                            "Whether the item form offers a Preview button, and whether an \
                             item must be previewed before it can be saved.",
                        )
                        .default_value(value_or(
                            values,
                            crate::content::preview::SETTING,
                            "optional",
                        )),
                ),
        )
        .element(
            "comment_settings",
            FormElement::fieldset()
                .title("Comment settings")
                .weight(5)
                .child(
                    "comment_mode",
                    FormElement::select(options(COMMENT_MODE_OPTIONS))
                        .title("Comments")
                        .description(
                            "Open: readers with permission may comment. Closed: existing \
                             comments are shown and no new ones are taken. Hidden: no comments \
                             are shown and none are taken.",
                        )
                        .default_value(value_or(values, "comment_mode", "open"))
                        .weight(0),
                )
                .child(
                    "comment_threading",
                    FormElement::select(options(COMMENT_THREADING_OPTIONS))
                        .title("Display mode")
                        .default_value(value_or(values, "comment_threading", "threaded"))
                        .weight(1),
                )
                .child(
                    "comment_order",
                    FormElement::select(options(COMMENT_ORDER_OPTIONS))
                        .title("Display order")
                        .description(
                            "In threaded mode this orders the threads; replies stay under the \
                             comment they answer, in the order they were written.",
                        )
                        .default_value(value_or(values, "comment_order", "oldest"))
                        .weight(2),
                )
                .child(
                    "comment_per_page",
                    FormElement::input("number")
                        .title("Comments per page")
                        .default_value(per_page)
                        .attribute("min", "1")
                        .attribute("max", "300")
                        .weight(3),
                ),
        )
        .element(
            "publishing_options",
            FormElement::fieldset()
                .title("Publishing options")
                .weight(6)
                .child(
                    "published_default",
                    FormElement::checkbox()
                        .title("Published by default")
                        .default_value(switch("published_default"))
                        .weight(0),
                )
                .child(
                    "revision_default",
                    FormElement::checkbox()
                        .title("Create new revision by default")
                        .default_value(switch("revision_default"))
                        .weight(1),
                )
                .child(
                    "promote_default",
                    FormElement::checkbox()
                        .title("Promoted to front page by default")
                        .default_value(switch("promote_default"))
                        .weight(2),
                )
                .child(
                    "sticky_default",
                    FormElement::checkbox()
                        .title("Sticky at top of lists by default")
                        .default_value(switch("sticky_default"))
                        .weight(3),
                )
                .child(
                    "moderation",
                    FormElement::checkbox()
                        .title("Moderate content of this type")
                        .description(
                            "Items move between Draft, Published and Archived by transitions \
                             each role is granted (use editorial transition …). A draft saved \
                             over a published item waits beside it: visitors keep seeing the \
                             published version until the draft is published.",
                        )
                        .default_value(switch("moderation"))
                        .weight(4),
                ),
        )
        .element(
            "op",
            FormElement::submit("Save content type")
                .suffix(cancel)
                .weight(10),
        )
}

// =============================================================================
// Content Type Management
// =============================================================================

/// List all content types.
///
/// GET /admin/structure/types
async fn list_content_types(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/structure/types");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-types.html",
        context,
    )
    .await
}

/// The content type add or edit page: the form at `action` showing `values`
/// and `errors`. `editing` is the machine name of the type being edited,
/// `None` on the add page.
async fn render_content_type_form(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    editing: Option<&str>,
    values: &serde_json::Value,
    errors: &[ValidationError],
) -> Response {
    let action = match editing {
        Some(type_name) => format!("/admin/structure/types/{type_name}/edit"),
        None => "/admin/structure/types/add".to_string(),
    };
    let cancel = cancel_link(state, user, "/admin/structure/types").await;
    let form_html = match admin_form::render(
        state,
        session,
        user,
        content_type_form(&action, editing.is_some(), values, cancel),
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
    context.insert("values", values);
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/content-type-form.html", context).await
}

/// Show add content type form.
///
/// GET /admin/structure/types/add
async fn add_content_type_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_content_type_form(&state, &session, &user, None, &serde_json::json!({}), &[]).await
}

/// Handle add content type form submission.
///
/// POST /admin/structure/types/add
async fn add_content_type_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form = ContentTypeFormData::from_posted(&posted);

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Name is required."));
    }

    if form.machine_name.trim().is_empty() {
        errors.push(ValidationError::field(
            "machine_name",
            "Machine name is required.",
        ));
    } else if !is_valid_machine_name(&form.machine_name) {
        errors.push(ValidationError::field("machine_name", MACHINE_NAME_ERROR));
    }

    // Check if machine name already exists. The cache answers the common
    // case; `create` refuses a name only the database knows.
    if state.content_types().get(&form.machine_name).is_some() {
        errors.push(ValidationError::field(
            "machine_name",
            CreateContentTypeError::AlreadyExists {
                machine_name: form.machine_name.clone(),
            }
            .to_string(),
        ));
    }

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        CONTENT_TYPE_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_content_type_form(
                &state,
                &session,
                &user,
                None,
                &form.values(),
                &errors,
            )
            .await;
        }
        Err(Err(response)) => return response,
    };

    // Create the content type
    let settings = form.settings();

    match state
        .content_types()
        .create(
            &form.machine_name,
            &form.label,
            form.description.as_deref(),
            settings,
        )
        .await
    {
        Ok(()) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(machine_name = %form.machine_name, "content type created");
            Redirect::to("/admin/structure/types").into_response()
        }
        Err(e @ CreateContentTypeError::AlreadyExists { .. }) => {
            render_content_type_form(
                &state,
                &session,
                &user,
                None,
                &form.values(),
                &[ValidationError::field("machine_name", e.to_string())],
            )
            .await
        }
        Err(CreateContentTypeError::Other(e)) => {
            tracing::error!(error = %e, "failed to create content type");
            render_server_error("Failed to create content type.")
        }
    }
}

/// Show edit content type form.
///
/// GET /admin/structure/types/{type}/edit
async fn edit_content_type_form(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    // Load the database model to access settings (not available on ContentTypeDefinition)
    let db_type = crate::models::ItemType::find_by_type(state.db(), &type_name)
        .await
        .ok()
        .flatten();
    let title_label = db_type
        .as_ref()
        .and_then(|it| it.title_label.as_deref())
        .unwrap_or("Title");
    let mut values = serde_json::json!({
        "label": content_type.label,
        "machine_name": content_type.machine_name,
        "description": content_type.description,
        "title_label": title_label,
    });
    for key in DEFAULT_SWITCHES {
        values[*key] = serde_json::Value::Bool(
            db_type
                .as_ref()
                .and_then(|it| it.settings.get(*key))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        );
    }
    values[crate::content::preview::SETTING] = serde_json::Value::from(
        crate::content::preview::PreviewMode::of(db_type.as_ref().map(|it| &it.settings)).as_str(),
    );
    for (key, value) in crate::content::comment_settings::CommentSettings::of(
        db_type.as_ref().map(|it| &it.settings),
    )
    .to_settings()
    {
        values[key] = value;
    }

    render_content_type_form(&state, &session, &user, Some(&type_name), &values, &[]).await
}

/// Handle edit content type form submission.
///
/// POST /admin/structure/types/{type}/edit
async fn edit_content_type_submit(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let form = ContentTypeFormData::from_posted(&posted);

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Name is required."));
    }

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        CONTENT_TYPE_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            // The machine name cannot change: show the type's own.
            let mut values = form.values();
            values["machine_name"] = serde_json::Value::from(type_name.as_str());
            return render_content_type_form(
                &state,
                &session,
                &user,
                Some(&type_name),
                &values,
                &errors,
            )
            .await;
        }
        Err(Err(response)) => return response,
    };

    // Update the content type
    let settings = form.settings();

    match state
        .content_types()
        .update(
            &type_name,
            &form.label,
            form.description.as_deref(),
            settings,
        )
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(machine_name = %type_name, "content type updated");
            Redirect::to("/admin/structure/types").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update content type");
            render_server_error("Failed to update content type.")
        }
    }
}

// =============================================================================
// Field list and add field
// =============================================================================

/// The add-field form's posted values, kept to show a refused add again.
#[derive(Debug, Default)]
struct FieldFormData {
    label: String,
    name: String,
    field_type: String,
    /// The category a `category_reference` field holds tags of.
    category: Option<String>,
}

impl FieldFormData {
    /// Read the posted fields.
    fn from_posted(posted: &PostedForm) -> Self {
        Self {
            label: posted.get("label").unwrap_or_default().to_string(),
            name: posted.get("name").unwrap_or_default().to_string(),
            field_type: posted.get("field_type").unwrap_or_default().to_string(),
            category: posted
                .get("category")
                .filter(|c| !c.is_empty())
                .map(str::to_string),
        }
    }
}

/// The add-field form of `type_name`'s field list, showing `values`.
///
/// Its button adds the field over AJAX when scripts run
/// ([`handle_ajax_add_field`]) and posts the form when they do not.
fn field_add_form(
    type_name: &str,
    values: &FieldFormData,
    category_options: Vec<(String, String)>,
) -> crate::form::Form {
    crate::form::Form::new(field_add_form_id(type_name))
        .action(format!("/admin/structure/types/{type_name}/fields/add"))
        .element(
            "label",
            FormElement::textfield()
                .title("Label")
                .default_value(values.label.clone())
                .required()
                .attribute("data-machine-name-source", "name")
                .attribute("data-machine-name-prefix", "field_")
                .attribute("data-machine-name-max", "32")
                .weight(0),
        )
        .element(
            "name",
            FormElement::textfield()
                .title("Machine name")
                .default_value(values.name.clone())
                .required()
                .attribute("pattern", "[a-z][a-z0-9_]*")
                .attribute("spellcheck", "false")
                .attribute("autocapitalize", "none")
                .weight(1),
        )
        .element(
            "field_type",
            FormElement::select(options(FIELD_TYPE_OPTIONS))
                .title("Type")
                .default_value(values.field_type.clone())
                .required()
                .weight(2),
        )
        .element(
            "category",
            FormElement::select(category_options)
                .title("Category")
                .description(
                    "Required for a category reference: the field holds tags of this category.",
                )
                .default_value(values.category.clone().unwrap_or_default())
                .weight(3),
        )
        .element(
            "op",
            FormElement::submit("Add field")
                .attribute("data-ajax-trigger", "add_field")
                .attribute("data-ajax-wrapper", "#fields-wrapper")
                .weight(10),
        )
}

/// The manage-fields page of `type_name`, its add-field form showing
/// `values` and `errors`, so a refused add does not make the person type it
/// again.
async fn render_field_list(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    type_name: &str,
    values: &FieldFormData,
    errors: &[ValidationError],
) -> Response {
    let Some(content_type) = state.content_types().get(type_name) else {
        return render_not_found();
    };

    let form_html = match admin_form::render(
        state,
        session,
        user,
        field_add_form(type_name, values, category_options(state).await),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("content_type", &content_type);
    context.insert("fields", &content_type.fields);
    field_index_context(
        state,
        &content_type.machine_name,
        &content_type.fields,
        &mut context,
    )
    .await;
    context.insert("add_field_form", &form_html);
    context.insert(
        "path",
        &format!("/admin/structure/types/{type_name}/fields"),
    );

    let viewer = super::helpers::user_context_for(state, user).await;
    render_admin_template(state, &viewer, "admin/field-list.html", context).await
}

/// Show manage fields page.
///
/// GET /admin/structure/types/{type}/fields
async fn manage_fields(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_field_list(
        &state,
        &session,
        &user,
        &type_name,
        &FieldFormData::default(),
        &[],
    )
    .await
}

/// Every category, for the category select a category reference field needs.
async fn categories(state: &AppState) -> Vec<crate::models::Category> {
    state
        .categories()
        .list_categories()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to list categories for the field screens");
            Vec::new()
        })
}

/// The category select's choices: id and label. The labels are the site's
/// data, shown as they are.
async fn category_options(state: &AppState) -> Vec<(String, String)> {
    categories(state)
        .await
        .into_iter()
        .map(|c| (c.id, c.label))
        .collect()
}

/// Add a field to a content type.
///
/// POST /admin/structure/types/{type}/fields/add
async fn add_field(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let form = FieldFormData::from_posted(&posted);

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Label is required."));
    }

    if form.name.trim().is_empty() {
        errors.push(ValidationError::field("name", "Machine name is required."));
    } else if !is_valid_machine_name(&form.name) {
        errors.push(ValidationError::field("name", MACHINE_NAME_ERROR));
    }

    if form.field_type.is_empty() {
        errors.push(ValidationError::field(
            "field_type",
            "Field type is required.",
        ));
    }

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        &field_add_form_id(&type_name),
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_field_list(&state, &session, &user, &type_name, &form, &errors).await;
        }
        Err(Err(response)) => return response,
    };

    // Add the field
    match state
        .content_types()
        .add_field_in_category(
            &type_name,
            &form.name,
            &form.label,
            &form.field_type,
            form.category.as_deref(),
        )
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(
                content_type = %type_name,
                field = %form.name,
                "field added"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/fields")).into_response()
        }
        // The ones the person at the screen can fix are shown to them. The
        // registry owns the wording, so it is not written out again here.
        Err(
            e @ (AddFieldError::DuplicateFieldName { .. }
            | AddFieldError::UnknownFieldType { .. }
            | AddFieldError::CategoryRequired
            | AddFieldError::UnknownCategory { .. }),
        ) => {
            let field = match e {
                AddFieldError::UnknownFieldType { .. } => "field_type",
                AddFieldError::CategoryRequired | AddFieldError::UnknownCategory { .. } => {
                    "category"
                }
                _ => "name",
            };
            render_field_list(
                &state,
                &session,
                &user,
                &type_name,
                &form,
                &[ValidationError::field(field, e.to_string())],
            )
            .await
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to add field");
            render_server_error("Failed to add field.")
        }
    }
}

// =============================================================================
// Field Edit / Delete
// =============================================================================

/// Each field's index column on the field list (B207): `field_indexes`, by
/// field name, with whether the field is marked and its index's state and
/// build progress; and `field_indexes_building`, so the page refreshes
/// itself while a build runs.
async fn field_index_context(
    state: &AppState,
    type_name: &str,
    fields: &[trovato_sdk::types::FieldDefinition],
    context: &mut tera::Context,
) {
    use crate::gather::field_index;
    let used = field_index::fields_used_by_gathers(state.gather());
    let statuses = field_index::statuses(state.db(), type_name)
        .await
        .unwrap_or_default();
    let mut by_field = serde_json::Map::new();
    let mut building = false;
    for field in fields {
        let available = crate::gather::field_sql::FieldKind::of(field).is_some();
        let marked = available
            && field_index::is_marked(
                field,
                used.contains(&(type_name.to_string(), field.field_name.clone())),
            );
        let status = statuses.iter().find(|s| s.field_name == field.field_name);
        building |= status.is_some_and(|s| s.state == "queued" || s.state == "building");
        by_field.insert(
            field.field_name.clone(),
            serde_json::json!({
                "available": available,
                "marked": marked,
                "status": status,
            }),
        );
    }
    context.insert("field_indexes", &by_field);
    context.insert("field_indexes_building", &building);
}

/// The "Filterable and sortable" facts the field form shows (B207): whether
/// the field can have an index, whether it is marked, whether that is its
/// own setting or the default, and its index's state.
struct Filterable {
    available: bool,
    marked: bool,
    own: bool,
    used_by_gathers: bool,
    status: Option<crate::gather::field_index::FieldIndexStatus>,
}

impl Filterable {
    /// The facts of `field` of `type_name`.
    async fn of(
        state: &AppState,
        type_name: &str,
        field: &trovato_sdk::types::FieldDefinition,
    ) -> Self {
        use crate::gather::field_index;
        let used = field_index::fields_used_by_gathers(state.gather())
            .contains(&(type_name.to_string(), field.field_name.clone()));
        let status = field_index::statuses(state.db(), type_name)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.field_name == field.field_name);
        Self {
            available: crate::gather::field_sql::FieldKind::of(field).is_some(),
            marked: field_index::is_marked(field, used),
            own: field_index::has_own_setting(field),
            used_by_gathers: used,
            status,
        }
    }

    /// The checkbox's hint: what an index does, and why it is on or off.
    fn hint(&self) -> &'static str {
        if self.own {
            "An index on this field lets listings filter and sort on it quickly at any size. \
             It costs a little time on every save and some disk space. Without one, a listing \
             that filters or sorts on this field reads every item of this type, which is slow \
             on large sites."
        } else if self.used_by_gathers {
            "On by default: a listing filters or sorts on this field. Without an index, such a \
             listing reads every item of this type, which is slow on large sites. An index \
             costs a little time on every save and some disk space."
        } else {
            "Off by default: no listing filters or sorts on this field yet. Turn it on before \
             you add one, so it is fast on large sites. An index costs a little time on every \
             save and some disk space."
        }
    }
}

/// The category a field holds tags of, when it is a category reference.
fn field_category(field: &trovato_sdk::types::FieldDefinition) -> Option<&str> {
    match &field.field_type {
        trovato_sdk::types::FieldType::CategoryReference(category) => Some(category),
        _ => None,
    }
}

/// A field type's name as the field screens show it: the type's variant
/// name, which the template filter `field_type_label` also gives.
fn field_type_name(field_type: &trovato_sdk::types::FieldType) -> String {
    match serde_json::to_value(field_type) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(serde_json::Value::Object(map)) => map
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string()),
        _ => "Unknown".to_string(),
    }
}

/// What the field is and cannot become, shown in the edit form below its
/// label: facts, not fields.
fn field_facts_markup(
    text: &super::admin_nav::AdminText,
    field: &trovato_sdk::types::FieldDefinition,
) -> String {
    format!(
        r#"<dl class="field-facts">
    <div class="field-facts__row">
        <dt>{machine_name}</dt>
        <dd><code>{name}</code></dd>
        <dd class="description">{name_fixed}</dd>
    </div>
    <div class="field-facts__row">
        <dt>{field_type}</dt>
        <dd><code>{type_label}</code></dd>
        <dd class="description">{type_fixed}</dd>
    </div>
</dl>"#,
        machine_name = text.html("Machine name", &[]),
        name = crate::i18n::data_html(&field.field_name),
        name_fixed = text.html("Machine names cannot be changed after creation.", &[]),
        field_type = text.html("Field type", &[]),
        type_label = text.html(&field_type_name(&field.field_type), &[]),
        type_fixed = text.html("Field types cannot be changed after creation.", &[]),
    )
}

/// The state of the field's index, shown under the "Filterable and
/// sortable" checkbox.
fn index_state_markup(
    text: &super::admin_nav::AdminText,
    status: &crate::gather::field_index::FieldIndexStatus,
) -> String {
    let words = match status.state.as_str() {
        "ready" => text.html("Index: built.", &[]),
        "building" => {
            let mut words = text.html("Index: building", &[]);
            if let Some(percent) = status.percent.filter(|p| *p != 0) {
                words.push_str(&format!(
                    r#" <progress max="100" value="{percent}">{percent}%</progress> {percent}%"#
                ));
            }
            words
        }
        "queued" => text.html("Index: queued.", &[]),
        "failed" => format!(
            "{} {}",
            text.html("Index: failed.", &[]),
            crate::i18n::data_html(status.error.as_deref().unwrap_or_default())
        ),
        "dropping" => text.html("Index: being dropped.", &[]),
        _ => String::new(),
    };
    format!(
        r#"<p class="field-index-state" data-field-index="{}">{words}</p>"#,
        html_escape(&status.state)
    )
}

/// The values the field edit form shows: the field's own, or a refused
/// submission's.
struct FieldEditValues {
    label: String,
    required: bool,
    cardinality: i32,
    category: Option<String>,
    filterable: bool,
    /// A List field's allowed values, one `key|label` per line.
    allowed_values: Option<String>,
}

/// Parse a List field's "Allowed values" textarea: one `key|label` per line,
/// blank lines skipped, a line with no `|` using its text as both. Keys must
/// be non empty, at most 255 characters, hold no `|` and be unique; labels
/// must be non empty. Returns every refused line's message, or the parsed
/// pairs.
fn parse_allowed_values(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut values = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, label) = match line.split_once('|') {
            Some((k, l)) => (k.trim(), l.trim()),
            None => (line, line),
        };
        if key.is_empty() {
            return Err(format!("\"{line}\": the key must not be empty."));
        }
        if key.chars().count() > 255 {
            return Err(format!(
                "\"{key}\": the key must be at most 255 characters."
            ));
        }
        if label.is_empty() {
            return Err(format!("\"{key}\": the label must not be empty."));
        }
        if !seen.insert(key.to_string()) {
            return Err(format!("\"{key}\": every key must be unique."));
        }
        values.push((key.to_string(), label.to_string()));
    }
    Ok(values)
}

/// A List field's allowed values as the edit form's textarea shows them, one
/// `key|label` per line.
fn allowed_values_text(field: &trovato_sdk::types::FieldDefinition) -> String {
    crate::content::field_value::list_options(&field.settings)
        .into_iter()
        .map(|(key, label)| format!("{key}|{label}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The field edit form of `field` of `type_name`, showing `values`.
fn field_edit_form(
    type_name: &str,
    field: &trovato_sdk::types::FieldDefinition,
    values: &FieldEditValues,
    filterable: &Filterable,
    category_options: Vec<(String, String)>,
    text: &super::admin_nav::AdminText,
    cancel: String,
) -> crate::form::Form {
    let mut form = crate::form::Form::new(FIELD_EDIT_FORM_ID)
        .action(format!(
            "/admin/structure/types/{type_name}/fields/{}/edit",
            field.field_name
        ))
        .element(
            "label",
            FormElement::textfield()
                .title("Label")
                .default_value(values.label.clone())
                .required()
                .weight(0),
        )
        .element(
            "field_facts",
            FormElement::markup(field_facts_markup(text, field)).weight(1),
        );
    if let Some(category) = &values.category {
        form = form.element(
            "category",
            FormElement::select(category_options)
                .title("Category")
                .description(
                    "The field holds tags of this category. Values already saved are not \
                     changed; the next save of the field is checked against the category \
                     chosen here.",
                )
                .default_value(category.clone())
                .required()
                .weight(2),
        );
    }
    if let Some(allowed_values) = &values.allowed_values {
        form = form.element(
            "allowed_values",
            FormElement::textarea(6)
                .title("Allowed values")
                .description(
                    "One value per line, as key|label (a line with no | uses the text as \
                     both). Values already saved that name a key you remove here are not \
                     changed.",
                )
                .default_value(allowed_values.clone())
                .required()
                .weight(2),
        );
    }
    form = form
        .element(
            "required",
            FormElement::checkbox()
                .title("Required")
                .description(
                    "If checked, this field must be filled in when creating or editing content.",
                )
                .default_value(values.required)
                .weight(3),
        )
        .element(
            "cardinality",
            FormElement::input("number")
                .title("Number of values")
                .description(
                    if matches!(field.field_type, trovato_sdk::types::FieldType::Link) {
                        "A Link field holds a single value: this must be 1."
                    } else {
                        "Maximum number of values. Use 1 for a single value, -1 for unlimited."
                    },
                )
                .default_value(values.cardinality.to_string())
                .attribute("min", "-1")
                .weight(4),
        );
    if filterable.available {
        form = form
            .element(
                "filterable_offered",
                FormElement::hidden().default_value("1").weight(5),
            )
            .element(
                "filterable",
                FormElement::checkbox()
                    .title("Filterable and sortable")
                    .description(filterable.hint())
                    .default_value(values.filterable)
                    .weight(6),
            );
        if let Some(status) = &filterable.status {
            form = form.element(
                "field_index_state",
                FormElement::markup(index_state_markup(text, status)).weight(7),
            );
        }
    }
    form.element("op", FormElement::submit("Save").suffix(cancel).weight(10))
}

/// The field edit page of `field_name` of `type_name`. `values` are a
/// refused submission's, shown with `errors`; `None` shows the field's own.
async fn render_field_edit(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    type_name: &str,
    field_name: &str,
    values: Option<FieldEditValues>,
    errors: &[ValidationError],
) -> Response {
    let Some(content_type) = state.content_types().get(type_name) else {
        return render_not_found();
    };
    let Some(field) = content_type
        .fields
        .iter()
        .find(|f| f.field_name == field_name)
    else {
        return render_not_found();
    };

    let filterable = Filterable::of(state, type_name, field).await;
    let is_list_field = matches!(field.field_type, trovato_sdk::types::FieldType::List);
    let values = values.unwrap_or_else(|| FieldEditValues {
        label: field.label.clone(),
        required: field.required,
        cardinality: field.cardinality,
        category: field_category(field).map(str::to_string),
        filterable: filterable.marked,
        allowed_values: is_list_field.then(|| allowed_values_text(field)),
    });
    let path = format!("/admin/structure/types/{type_name}/fields/{field_name}/edit");
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, &path).await;
    let cancel = cancel_link(
        state,
        user,
        &format!("/admin/structure/types/{type_name}/fields"),
    )
    .await;
    let form = field_edit_form(
        type_name,
        field,
        &values,
        &filterable,
        category_options(state).await,
        &text,
        cancel,
    );
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("type_name", type_name);
    context.insert("field", field);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(state, &viewer, "admin/field-edit.html", context).await
}

/// Show field edit form.
///
/// GET /admin/structure/types/{type}/fields/{field}/edit
async fn edit_field_form(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_field_edit(&state, &session, &user, &type_name, &field_name, None, &[]).await
}

/// Handle field edit form submission.
///
/// POST /admin/structure/types/{type}/fields/{field}/edit
async fn edit_field_submit(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let Some(field) = content_type
        .fields
        .iter()
        .find(|f| f.field_name == field_name)
    else {
        return render_not_found();
    };

    let label = posted.get("label").unwrap_or_default().to_string();
    let required = posted.contains("required");
    let filterable_offered = posted.contains("filterable_offered");
    let filterable = posted.contains("filterable");

    // Validate
    let mut errors = Vec::new();

    if label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Label is required."));
    }

    // An empty or missing number is a single value; one that is not a number
    // at all is refused like any other invalid count.
    let cardinality = match posted.get("cardinality").map(str::trim) {
        None | Some("") => Some(1),
        Some(raw) => raw.parse::<i32>().ok(),
    };
    let cardinality = match cardinality {
        Some(n) if n != 0 && n >= -1 => n,
        other => {
            errors.push(ValidationError::field(
                "cardinality",
                "Number of values must be 1 or more, or -1 for unlimited.",
            ));
            other.unwrap_or(1)
        }
    };
    if matches!(field.field_type, trovato_sdk::types::FieldType::Link) && cardinality != 1 {
        errors.push(ValidationError::field(
            "cardinality",
            "A Link field holds a single value: Number of values must be 1.",
        ));
    }

    let is_list_field = matches!(field.field_type, trovato_sdk::types::FieldType::List);
    let allowed_values_input = posted.get("allowed_values").unwrap_or_default().to_string();
    let allowed_values = if is_list_field {
        match parse_allowed_values(&allowed_values_input) {
            Ok(values) if values.is_empty() => {
                errors.push(ValidationError::field(
                    "allowed_values",
                    "At least one allowed value is required.",
                ));
                None
            }
            Ok(values) => Some(values),
            Err(message) => {
                errors.push(ValidationError::field("allowed_values", message));
                None
            }
        }
    } else {
        None
    };

    // A category reference keeps a category: the one chosen, which must exist.
    let category = match field_category(field) {
        Some(current) => {
            let chosen = posted.get("category").map(str::trim).unwrap_or(current);
            if chosen.is_empty() {
                errors.push(ValidationError::field(
                    "category",
                    AddFieldError::CategoryRequired.to_string(),
                ));
            } else if !categories(&state).await.iter().any(|c| c.id == chosen) {
                errors.push(ValidationError::field(
                    "category",
                    AddFieldError::UnknownCategory {
                        category: chosen.to_string(),
                    }
                    .to_string(),
                ));
            }
            Some(chosen.to_string())
        }
        None => None,
    };

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, FIELD_EDIT_FORM_ID, errors).await
        {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                let values = FieldEditValues {
                    label,
                    required,
                    cardinality,
                    category,
                    filterable,
                    allowed_values: is_list_field.then_some(allowed_values_input),
                };
                return render_field_edit(
                    &state,
                    &session,
                    &user,
                    &type_name,
                    &field_name,
                    Some(values),
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };

    if let Some(category) = category.as_deref()
        && let Err(e) = state
            .content_types()
            .set_field_category(&type_name, &field_name, category)
            .await
    {
        tracing::error!(error = %e, "failed to change the field's category");
        return render_server_error("Failed to update field.");
    }

    if let Some(values) = allowed_values
        && let Err(e) = state
            .content_types()
            .set_field_allowed_values(&type_name, &field_name, &values)
            .await
    {
        tracing::error!(error = %e, "failed to change the field's allowed values");
        return render_server_error("Failed to update field.");
    }

    if filterable_offered
        && let Err(e) = state
            .content_types()
            .set_field_setting(
                &type_name,
                &field_name,
                crate::gather::field_index::SETTING,
                serde_json::Value::Bool(filterable),
            )
            .await
    {
        tracing::error!(error = %e, "failed to change the field's filterable setting");
        return render_server_error("Failed to update field.");
    }

    match state
        .content_types()
        .update_field(&type_name, &field_name, &label, required, cardinality)
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            // Build or drop the field's index in the background (B207).
            state.reconcile_field_indexes();
            tracing::info!(
                content_type = %type_name,
                field = %field_name,
                required = %required,
                "field updated"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/fields")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update field");
            render_server_error("Failed to update field.")
        }
    }
}

/// Ask before deleting a field.
///
/// GET /admin/structure/types/{type}/fields/{field}/delete
async fn delete_field_form(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let Some(field) = content_type
        .fields
        .iter()
        .find(|f| f.field_name == field_name)
    else {
        return render_not_found();
    };
    let path = format!("/admin/structure/types/{type_name}/fields/{field_name}/delete");
    let form = crate::form::Form::new(FIELD_DELETE_FORM_ID)
        .action(path.clone())
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .weight(10),
        );
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("type_name", &type_name);
    context.insert("field", field);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(&state, &viewer, "admin/field-delete.html", context).await
}

/// Handle field deletion.
///
/// POST /admin/structure/types/{type}/fields/{field}/delete
async fn delete_field_submit(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        FIELD_DELETE_FORM_ID,
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

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    match state
        .content_types()
        .delete_field(&type_name, &field_name)
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(
                content_type = %type_name,
                field = %field_name,
                "field deleted"
            );
            // Drop the deleted field's index, if it had one (B207).
            state.reconcile_field_indexes();
            Redirect::to(&format!("/admin/structure/types/{type_name}/fields")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to delete field");
            render_server_error("Failed to delete field.")
        }
    }
}

// =============================================================================
// Search Configuration
// =============================================================================

/// The form id of the search switches form.
const SEARCH_SWITCHES_FORM_ID: &str = "search_switches_form";

/// The form id of the "Weight a field" form.
const SEARCH_WEIGHT_FORM_ID: &str = "search_weight_form";

/// The form id of the confirmation that removes a field's search weight.
const SEARCH_WEIGHT_DELETE_FORM_ID: &str = "search_weight_delete_form";

/// The form id of the "Reindex content" form.
const SEARCH_REINDEX_FORM_ID: &str = "search_reindex_form";

/// The weights a field may be given (A is the title's): value and English
/// label, translated by the select template.
const SEARCH_WEIGHT_OPTIONS: &[(&str, &str)] = &[
    ("B", "B - High priority"),
    ("C", "C - Normal priority"),
    ("D", "D - Low priority"),
];

/// A content type's search switches as stored, defaults when it has none.
async fn stored_search_switches(state: &AppState, type_name: &str) -> SearchSwitches {
    match crate::models::ItemType::find_by_type(state.db(), type_name).await {
        Ok(Some(row)) => SearchSwitches::from_settings(&row.settings),
        Ok(None) => SearchSwitches::default(),
        Err(e) => {
            tracing::warn!(error = %e, content_type = %type_name, "failed to read search switches");
            SearchSwitches::default()
        }
    }
}

/// The two checkboxes, through the kernel's form pipeline.
fn search_switches_form(type_name: &str, switches: SearchSwitches) -> crate::form::Form {
    crate::form::Form::new(SEARCH_SWITCHES_FORM_ID)
        .action(format!("/admin/structure/types/{type_name}/search"))
        .element(
            "include",
            FormElement::checkbox()
                .title("Include this content type in search")
                .description(
                    "Unchecked, no item of this type is found by site search, in the browser \
                     index or on the server.",
                )
                .default_value(switches.include)
                .weight(0),
        )
        .element(
            "comments",
            FormElement::checkbox()
                .title("Include comments in search")
                .description(
                    "Unchecked, an item of this type is no longer found by words that appear \
                     only in its published comments.",
                )
                .default_value(switches.comments)
                .weight(1),
        )
        .element("op", FormElement::submit("Save search settings").weight(10))
}

/// The "Weight a field" form: one of the fields with no weight yet, and the
/// weight to give it, `weight` chosen.
fn search_weight_form(
    type_name: &str,
    unweighted: Vec<(String, String)>,
    field_name: &str,
    weight: &str,
) -> crate::form::Form {
    crate::form::Form::new(SEARCH_WEIGHT_FORM_ID)
        .action(format!("/admin/structure/types/{type_name}/search/add"))
        .element(
            "field_name",
            FormElement::select(unweighted)
                .title("Field")
                .default_value(field_name.to_string())
                .required()
                .weight(0),
        )
        .element(
            "weight",
            FormElement::select(options(SEARCH_WEIGHT_OPTIONS))
                .title("Weight")
                .default_value(if weight.is_empty() { "C" } else { weight })
                .required()
                .weight(1),
        )
        .element("op", FormElement::submit("Set weight").weight(10))
}

/// Interface text for a form field's value: `source` translated into
/// `lang` with `args` filled, not escaped (the form template escapes it).
fn field_text(state: &AppState, lang: &str, source: &str, args: &[(&str, &str)]) -> String {
    let args: Vec<(String, String)> = args
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    crate::i18n::translate(
        state.locale().map(|l| l.as_ref()),
        lang,
        &crate::i18n::Message::new(source),
        &args,
    )
}

/// The "Reindex content" form. It asks first (`?confirm`, see
/// `super::admin_confirm`), with the question and the button of the
/// confirmation page in its hidden fields, in the viewer's admin language.
fn search_reindex_form(
    state: &AppState,
    lang: &str,
    type_name: &str,
    type_label: &str,
) -> crate::form::Form {
    let args = [("type", type_label)];
    crate::form::Form::new(SEARCH_REINDEX_FORM_ID)
        .action(format!(
            "/admin/structure/types/{type_name}/search/reindex?confirm"
        ))
        .element(
            "_confirm",
            FormElement::hidden()
                .default_value(field_text(
                    state,
                    lang,
                    "This will update the search index for all {type} content. Continue?",
                    &args,
                ))
                .weight(0),
        )
        .element(
            "_confirm_label",
            FormElement::hidden()
                .default_value(field_text(state, lang, "Reindex all {type} content", &args))
                .weight(1),
        )
        .element(
            "op",
            FormElement::submit("Reindex all content")
                .attribute("class", "button--secondary")
                .weight(10),
        )
}

/// What the search settings page shows in its forms: the switches, and the
/// "Weight a field" form's choice, each with its errors.
struct SearchPageForms {
    switches: SearchSwitches,
    switch_errors: Vec<ValidationError>,
    weight_field: String,
    weight: String,
    weight_errors: Vec<ValidationError>,
}

impl SearchPageForms {
    /// The forms as stored: no choice made, no errors.
    fn stored(switches: SearchSwitches) -> Self {
        Self {
            switches,
            switch_errors: Vec::new(),
            weight_field: String::new(),
            weight: String::new(),
            weight_errors: Vec::new(),
        }
    }
}

/// Manage search field configuration for a content type.
///
/// GET /admin/structure/types/{type}/search
async fn manage_search_config(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let switches = stored_search_switches(&state, &type_name).await;
    render_search_config(
        &state,
        &session,
        &user,
        &type_name,
        SearchPageForms::stored(switches),
    )
    .await
}

/// The search settings page, its forms showing `forms`.
async fn render_search_config(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    type_name: &str,
    forms: SearchPageForms,
) -> Response {
    let Some(content_type) = state.content_types().get(type_name) else {
        return render_not_found();
    };

    let switches_form = match admin_form::render(
        state,
        session,
        user,
        search_switches_form(type_name, forms.switches),
        &forms.switch_errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let flash: Option<String> = session.remove(SEARCH_SWITCHES_FLASH).await.ok().flatten();

    // Get current search configs
    let search_configs = match state.search().list_field_configs(type_name).await {
        Ok(configs) => configs,
        Err(e) => {
            tracing::error!(error = %e, "failed to list search configs");
            vec![]
        }
    };

    // Build a map of field_name -> weight for easy template access
    let config_map: std::collections::HashMap<String, char> = search_configs
        .iter()
        .map(|c| (c.field_name.clone(), c.weight))
        .collect();

    // Every text field is searched whether or not it has a weight here; the
    // page says so rather than calling it "not indexed".
    let text_fields: Vec<&str> = content_type
        .fields
        .iter()
        .filter(|f| crate::search::is_text_field(&f.field_type))
        .map(|f| f.field_name.as_str())
        .collect();

    // The weight form, when there are fields to weight.
    let weight_form = if content_type.fields.is_empty() {
        None
    } else {
        let unweighted: Vec<(String, String)> = content_type
            .fields
            .iter()
            .filter(|f| !config_map.contains_key(&f.field_name))
            .map(|f| (f.field_name.clone(), f.label.clone()))
            .collect();
        match admin_form::render(
            state,
            session,
            user,
            search_weight_form(type_name, unweighted, &forms.weight_field, &forms.weight),
            &forms.weight_errors,
        )
        .await
        {
            Ok(html) => Some(html),
            Err(response) => return response,
        }
    };

    let viewer = super::helpers::user_context_for(state, user).await;
    let path = format!("/admin/structure/types/{type_name}/search");
    let text = super::admin_nav::AdminText::for_page(state, &viewer, &path).await;
    let reindex_form = match admin_form::render(
        state,
        session,
        user,
        search_reindex_form(state, &text.lang, type_name, &content_type.label),
        &[],
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("content_type", &content_type);
    context.insert("fields", &content_type.fields);
    context.insert("search_configs", &config_map);
    context.insert("text_fields", &text_fields);
    context.insert("switches_form", &switches_form);
    context.insert("weight_form", &weight_form);
    context.insert("reindex_form", &reindex_form);
    context.insert("flash", &flash);
    context.insert("path", &path);

    render_admin_template(state, &viewer, "admin/search-config.html", context).await
}

/// Where the save confirmation waits for the redirect.
const SEARCH_SWITCHES_FLASH: &str = "search_switches_flash";

/// Save a content type's two search switches.
///
/// POST /admin/structure/types/{type}/search
///
/// Stores them in the type's `settings` (merged, so nothing else there
/// changes), recomputes the type's full text vectors so `/search` follows at
/// once, and queues every item of the type for the Pagefind index, which the
/// next cron run updates.
async fn save_search_switches(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let wanted = SearchSwitches {
        include: posted.contains("include"),
        comments: posted.contains("comments"),
    };
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        SEARCH_SWITCHES_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            let mut forms = SearchPageForms::stored(wanted);
            forms.switch_errors = errors;
            return render_search_config(&state, &session, &user, &type_name, forms).await;
        }
        Err(Err(response)) => return response,
    };

    let patch = serde_json::json!({ SearchSwitches::SETTINGS_KEY: wanted });
    if let Err(e) = state
        .content_types()
        .update(
            &type_name,
            &content_type.label,
            Some(content_type.description.as_str()),
            patch,
        )
        .await
    {
        tracing::error!(error = %e, content_type = %type_name, "failed to save search switches");
        return render_server_error("The search settings could not be saved.");
    }
    admin_form::submitted(&state, &user, &form_state, &posted).await;
    if let Err(e) = state.search().reindex_bundle(&type_name).await {
        tracing::error!(error = %e, content_type = %type_name, "failed to reindex after search switches");
    }
    // Every page of the type may change, or leave the index: queue them all.
    if let Err(e) =
        crate::search::index_queue::enqueue_type(state.db(), &type_name, "type_settings").await
    {
        tracing::error!(error = %e, content_type = %type_name, "failed to queue the type for the search index");
    }

    tracing::info!(
        log_type = "config",
        user_id = %user.id,
        content_type = %type_name,
        include = wanted.include,
        comments = wanted.comments,
        "search switches changed"
    );
    let _ = session
        .insert(
            SEARCH_SWITCHES_FLASH,
            "The search settings were saved. The search index is rebuilt at the next cron run.",
        )
        .await;
    Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
}

/// Add or update a search field configuration.
///
/// POST /admin/structure/types/{type}/search/add
async fn add_search_config(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let field_name = posted.get("field_name").unwrap_or_default().to_string();
    let weight_raw = posted.get("weight").unwrap_or_default().to_string();

    // Validate weight
    let weight = weight_raw.chars().next().unwrap_or('C');
    let mut errors = Vec::new();
    if !['A', 'B', 'C', 'D'].contains(&weight) {
        errors.push(ValidationError::field(
            "weight",
            "Invalid weight. Must be A, B, C, or D.",
        ));
    }

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        SEARCH_WEIGHT_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            let switches = stored_search_switches(&state, &type_name).await;
            let mut forms = SearchPageForms::stored(switches);
            forms.weight_field = field_name;
            forms.weight = weight_raw;
            forms.weight_errors = errors;
            return render_search_config(&state, &session, &user, &type_name, forms).await;
        }
        Err(Err(response)) => return response,
    };

    // Configure the field
    match state
        .search()
        .configure_field(&type_name, &field_name, weight)
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(
                content_type = %type_name,
                field = %field_name,
                weight = %weight,
                "search field configured"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to configure search field");
            render_server_error("Failed to configure search field.")
        }
    }
}

/// Ask before removing a field's search weight.
///
/// GET /admin/structure/types/{type}/search/{field}/delete
async fn remove_search_config_form(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let Some(field) = content_type
        .fields
        .iter()
        .find(|f| f.field_name == field_name)
    else {
        return render_not_found();
    };
    let path = format!("/admin/structure/types/{type_name}/search/{field_name}/delete");
    let form = crate::form::Form::new(SEARCH_WEIGHT_DELETE_FORM_ID)
        .action(path.clone())
        .element(
            "op",
            FormElement::submit("Remove weight")
                .attribute("class", "button--danger")
                .weight(10),
        );
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("type_name", &type_name);
    context.insert("field", field);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(&state, &viewer, "admin/search-weight-delete.html", context).await
}

/// Remove a search field configuration.
///
/// POST /admin/structure/types/{type}/search/{field}/delete
async fn remove_search_config(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        SEARCH_WEIGHT_DELETE_FORM_ID,
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

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    match state
        .search()
        .remove_field_config(&type_name, &field_name)
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(
                content_type = %type_name,
                field = %field_name,
                "search field config removed"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to remove search field config");
            render_server_error("Failed to remove search field configuration.")
        }
    }
}

/// Reindex all content of a specific type.
///
/// POST /admin/structure/types/{type}/search/reindex
async fn reindex_content_type(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        SEARCH_REINDEX_FORM_ID,
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

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    match state.search().reindex_bundle(&type_name).await {
        Ok(count) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(
                content_type = %type_name,
                count = %count,
                "content type reindexed"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to reindex content type");
            render_server_error("Failed to reindex content.")
        }
    }
}

// =============================================================================
// AJAX Handler
// =============================================================================

/// Handle AJAX add_field trigger for manage_fields forms.
pub(crate) async fn handle_ajax_add_field(
    state: &AppState,
    request: &crate::form::AjaxRequest,
    lang: &str,
) -> Response {
    use crate::form::{AjaxCommand, AjaxResponse};

    // Load form state to get the content type name
    let form_state = match state.forms().load_state(&request.form_build_id).await {
        Ok(Some(fs)) => fs,
        Ok(None) => {
            return Json(
                AjaxResponse::new().alert("Form session expired. Please reload the page."),
            )
            .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load form state");
            return Json(AjaxResponse::new().alert("An error occurred. Please try again."))
                .into_response();
        }
    };

    // Extract content type name from form_id (format: "manage_fields_{type}")
    let type_name = form_state
        .form_id
        .strip_prefix("manage_fields_")
        .unwrap_or(&form_state.form_id);

    // Get field values from request
    let label = request
        .values
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let name = request
        .values
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let field_type = request
        .values
        .get("field_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let category = request.values.get("category").and_then(|v| v.as_str());

    // Validate
    if label.is_empty() {
        return Json(AjaxResponse::new().alert("Label is required.")).into_response();
    }
    if name.is_empty() {
        return Json(AjaxResponse::new().alert("Machine name is required.")).into_response();
    }
    if !is_valid_machine_name(name) {
        return Json(AjaxResponse::new().alert(MACHINE_NAME_ERROR)).into_response();
    }
    if field_type.is_empty() {
        return Json(AjaxResponse::new().alert("Field type is required.")).into_response();
    }

    // Add the field
    match state
        .content_types()
        .add_field_in_category(type_name, name, label, field_type, category)
        .await
    {
        Ok(()) => {}
        // Same user-fixable refusals as the form post, same wording: the
        // registry owns the message.
        Err(
            e @ (AddFieldError::DuplicateFieldName { .. }
            | AddFieldError::UnknownFieldType { .. }
            | AddFieldError::CategoryRequired
            | AddFieldError::UnknownCategory { .. }),
        ) => {
            return Json(AjaxResponse::new().alert(e.to_string())).into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to add field via AJAX");
            return Json(AjaxResponse::new().alert("Failed to add field.")).into_response();
        }
    }

    tracing::info!(content_type = %type_name, field = %name, "field added via AJAX");

    // The new row, as `admin/field-list.html` draws one, in the viewer's
    // admin language. Strings are stripped of i18n markers: this HTML never
    // passes through a template.
    if let Some(locale) = state.locale() {
        locale.ensure_loaded(lang).await;
    }
    let tr = |source: &str, args: &[(String, String)]| {
        crate::i18n::strip_markers(&crate::i18n::translate(
            state.locale().map(|l| l.as_ref()),
            lang,
            &crate::i18n::Message::new(source),
            args,
        ))
    };
    let row_html = format!(
        r#"<tr data-field="{name_esc}">
            <th scope="row">{label_esc}</th>
            <td><code>{name_esc}</code></td>
            <td>{type_esc}</td>
            <td>{no}</td>
            <td class="field-index" data-field-index="none">{not_indexed}</td>
            <td class="operations">
                <a href="/admin/structure/types/{ct_esc}/fields/{name_esc}/edit">{edit}</a>
            </td>
        </tr>"#,
        name_esc = html_escape(name),
        label_esc = html_escape(label),
        type_esc = html_escape(field_type),
        ct_esc = html_escape(type_name),
        no = html_escape(&tr("No", &[])),
        not_indexed = html_escape(&tr("Not indexed", &[])),
        edit = html_escape(&tr(
            "Edit {name}",
            &[("name".to_string(), label.to_string())]
        )),
    );

    // Return AJAX response to append row and reset form
    Json(
        AjaxResponse::new()
            .append("#fields-tbody", row_html)
            .invoke("Trovato.resetAddFieldForm", serde_json::json!({}))
            .command(AjaxCommand::Remove {
                selector: "#no-fields-message".to_string(),
            }),
    )
    .into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the content type admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/structure/types", get(list_content_types))
        .route(
            "/admin/structure/types/add",
            get(add_content_type_form).post(add_content_type_submit),
        )
        .route(
            "/admin/structure/types/{type}/edit",
            get(edit_content_type_form).post(edit_content_type_submit),
        )
        .route("/admin/structure/types/{type}/fields", get(manage_fields))
        .route("/admin/structure/types/{type}/fields/add", post(add_field))
        .route(
            "/admin/structure/types/{type}/fields/{field}/edit",
            get(edit_field_form).post(edit_field_submit),
        )
        .route(
            "/admin/structure/types/{type}/fields/{field}/delete",
            get(delete_field_form).post(delete_field_submit),
        )
        .route(
            "/admin/structure/types/{type}/search",
            get(manage_search_config).post(save_search_switches),
        )
        .route(
            "/admin/structure/types/{type}/search/add",
            post(add_search_config),
        )
        .route(
            "/admin/structure/types/{type}/search/{field}/delete",
            get(remove_search_config_form).post(remove_search_config),
        )
        .route(
            "/admin/structure/types/{type}/search/reindex",
            post(reindex_content_type),
        )
}
