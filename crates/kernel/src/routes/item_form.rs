//! The item form, built and processed through the one forms pipeline.
//!
//! `/admin/content/add/{type}`, `/admin/content/{id}/edit`, `/item/add/{type}`
//! and `/item/{id}/edit` all build the same form here: `FormBuilder` turns the
//! content type into a typed `Form`, the route adds its own elements (stage,
//! URL alias), `FormService::build` runs `tap_form_alter` and saves the form
//! state, and the theme renders it. A submission goes through
//! `FormService::process`, whose built-in validation is the submission
//! builder, and a save through `FormService::submitted`.

use std::collections::HashMap;

use serde_json::{Map, Value};
use tower_sessions::Session;
use trovato_sdk::types::{ContentTypeDefinition, FieldType};
use uuid::Uuid;

use crate::content::preview::{self, PreviewMode};
use crate::content::submission::{PostedForm, build_item_fields, permitted_text_formats};
use crate::content::{FieldMode, FormBuilder, PUBLISH_CONTENT};
use crate::form::{ElementType, Form, FormElement, FormState, ProcessError, ValidationError};
use crate::models::Item;
use crate::state::AppState;
use crate::tap::{RequestState, UserContext};

/// The request state a form tap runs with: the user, and the full services
/// handle, so a plugin altering or validating a form can use the host API.
pub(crate) fn tap_state(state: &AppState, user: &UserContext) -> RequestState {
    RequestState::new(user.clone(), state.tap_services().clone())
}

/// Build the item form for `item` (or a new item of `content_type`), with the
/// route's own `extra` elements, through `FormService::build`.
///
/// `previewed` says the form is shown again under a preview of what was
/// submitted, which is what lets a type whose preview is required offer Save.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build(
    state: &AppState,
    session: &Session,
    user: &UserContext,
    content_type: &ContentTypeDefinition,
    item: Option<&Item>,
    action: &str,
    extra: Vec<(String, FormElement)>,
    previewed: bool,
) -> anyhow::Result<Form> {
    let type_name = &content_type.machine_name;
    // On a moderated type the form edits the latest revision, which can be a
    // draft ahead of the published one, and a state select replaces the
    // published checkbox.
    let moderation = moderation_context(state, type_name, item).await;
    let working = moderation.as_ref().and_then(|m| m.working.as_ref());
    let item = working.or(item);
    let modes =
        super::item::field_modes_for(state, user, type_name, content_type, item.is_none()).await;
    let titles = match item {
        Some(item) => reference_titles(state, user, content_type, item, &modes).await,
        None => HashMap::new(),
    };
    let language = active_language(state, session).await;
    let builder = FormBuilder::new(content_type.clone())
        .with_permitted_formats(permitted_text_formats(user))
        .with_reference_titles(titles)
        .with_item_id(item.map(|i| i.id))
        .with_placeholders(record_reference_placeholders(
            state,
            content_type,
            &language,
        ))
        .with_default_placeholder(translate(state, "Search...", &language))
        .with_owner_hint(translate(state, "Choose the owner first", &language))
        .with_field_modes(modes)
        .with_publish_allowed(user.can(PUBLISH_CONTENT) && moderation.is_none());
    let mut extra = extra;
    if let Some(moderation) = &moderation {
        extra.push(moderation_element(user, moderation.state));
    }
    extra.extend(publishing_element(state, user, type_name, item).await);
    let mode = preview_mode(state, type_name).await;
    if mode != PreviewMode::Disabled {
        let text = super::admin_nav::AdminText::for_page(state, user, action).await;
        extra.push(preview_button(&text.html("Preview", &[])));
    }
    let mut form = match item {
        Some(item) => builder.build_edit_form(item, action),
        None => builder.build_add_form(action),
    }
    .elements(extra);
    // A type that must be previewed offers Save only under a preview.
    if mode == PreviewMode::Required && !previewed {
        form.elements.remove("op");
    }

    let mut form_state = FormState::new(form.form_id.clone(), form.form_build_id.clone());
    if previewed {
        form_state
            .extra
            .insert(preview::PREVIEWED.to_string(), Value::Bool(true));
    }
    form_state.user_id = Some(user.id);
    form_state.item_id = item.map(|i| i.id);
    form_state.item_type = Some(type_name.clone());
    state
        .forms()
        .build(form, session, &tap_state(state, user), form_state)
        .await
}

/// The preview mode of `type_name`: its `preview_mode` setting, or the
/// default when the type cannot be read.
pub(crate) async fn preview_mode(state: &AppState, type_name: &str) -> PreviewMode {
    match crate::models::ItemType::find_by_type(state.db(), type_name).await {
        Ok(row) => PreviewMode::of(row.as_ref().map(|r| &r.settings)),
        Err(e) => {
            tracing::warn!(error = %e, type_name, "reading the type's preview mode failed");
            PreviewMode::default()
        }
    }
}

/// The Preview button. A `<button>` the form submits like Save, by name: the
/// routes see `preview` in the submission and render it instead of saving.
///
/// `label` is the button's text, escaped: "Preview" in the viewer's admin
/// language on an admin page, the English source elsewhere.
fn preview_button(label: &str) -> (String, FormElement) {
    (
        preview::BUTTON.to_string(),
        FormElement::markup(format!(
            r#"<div class="form-actions form-actions--preview"><button name="{}" value="1" class="button button--secondary">{label}</button></div>"#,
            preview::BUTTON
        ))
        .weight(crate::content::SUBMIT_WEIGHT),
    )
}

/// Whether a submission asks for a preview rather than a save.
pub(crate) fn wants_preview(posted: &PostedForm) -> bool {
    posted.contains(preview::BUTTON)
}

/// The preview of a submission: its title and fields rendered as the item
/// page renders them, above the form shown again. Nothing is saved.
pub(crate) async fn render_preview(
    state: &AppState,
    user: &UserContext,
    content_type: &ContentTypeDefinition,
    title: &str,
    fields: &Map<String, Value>,
    page_path: &str,
) -> String {
    let (fields_html, _stylesheets, _page_root) = super::item::render_item_fields(
        state,
        user,
        &Value::Object(fields.clone()),
        &content_type.fields,
    )
    .await;
    // The preview's own words in the admin language on an admin page
    // (`page_path`), the English source on the front end.
    let text = super::admin_nav::AdminText::for_page(state, user, page_path).await;
    format!(
        r#"<section class="item-preview" aria-label="{}"><p class="item-preview__notice">{}</p><article class="item item--preview item--{}"><h1 class="item__title">{}</h1>{}</article></section>"#,
        text.html("Preview", &[]),
        text.html("This is a preview. It has not been saved.", &[]),
        super::helpers::html_escape(&content_type.machine_name),
        if crate::i18n::markers_enabled() {
            crate::i18n::data_html(title)
        } else {
            super::helpers::html_escape(title)
        },
        fields_html
    )
}

/// The display title of every `RecordReference` target the item points at,
/// for a target this user may view: one query for all of them, multi value
/// fields included. A target they may not see keeps its id and no title.
///
/// A `category_term` field points at a term, not an item, and shows the
/// term's name: the editor may edit this item, which already holds it, and
/// term names are what every term page shows.
async fn reference_titles(
    state: &AppState,
    user: &UserContext,
    content_type: &ContentTypeDefinition,
    item: &Item,
    modes: &HashMap<String, FieldMode>,
) -> HashMap<String, String> {
    let mut ids: Vec<Uuid> = Vec::new();
    let mut term_ids: Vec<Uuid> = Vec::new();
    for field in &content_type.fields {
        let FieldType::RecordReference(target) = &field.field_type else {
            continue;
        };
        if modes.get(&field.field_name) == Some(&FieldMode::Hidden) {
            continue;
        }
        let values: Vec<&Value> = match item.fields.get(&field.field_name) {
            Some(Value::Array(values)) => values.iter().collect(),
            Some(value) => vec![value],
            None => Vec::new(),
        };
        let found = values.into_iter().filter_map(|v| {
            crate::content::extract_reference_id(Some(v))
                .parse::<Uuid>()
                .ok()
        });
        if target == "category_term" {
            term_ids.extend(found);
        } else {
            ids.extend(found);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    term_ids.sort_unstable();
    term_ids.dedup();

    let mut titles = HashMap::new();
    for id in term_ids {
        if let Ok(Some(term)) = state.categories().get_tag(id).await {
            titles.insert(term.id.to_string(), term.label);
        }
    }
    let targets = Item::find_by_ids(state.db(), &ids)
        .await
        .unwrap_or_default();
    for target in targets {
        // Editing this item says nothing about reading the one it points at.
        if state
            .items()
            .check_access(&target, "view", user)
            .await
            .unwrap_or(false)
        {
            titles.insert(target.id.to_string(), target.title);
        }
    }
    titles
}

/// The language a `RecordReference` placeholder or hint is translated into:
/// the session's override, the same one `negotiate_language` reads, else the
/// site default. A form is built well after that middleware has run, and
/// nothing here carries its resolved [`ResolvedLanguage`] forward, so this
/// reads the same session key directly rather than widen every route handler
/// between it and here to thread the extension through.
///
/// [`ResolvedLanguage`]: crate::middleware::language::ResolvedLanguage
async fn active_language(state: &AppState, session: &Session) -> String {
    session
        .get::<Option<String>>(crate::middleware::language::SESSION_ACTIVE_LANGUAGE)
        .await
        .ok()
        .flatten()
        .flatten()
        .unwrap_or_else(|| state.default_language().to_string())
}

/// Translate an interface string the way every other one in the locale table
/// will, once something else uses it too: unchanged when no locale service is
/// configured.
fn translate(state: &AppState, source: &str, language: &str) -> String {
    match state.locale() {
        Some(locale) => locale.translate(source, "form", language),
        None => source.to_string(),
    }
}

/// The `RecordReference` placeholder for each target type `content_type`'s
/// fields point at, keyed by target type: a human label, never the machine
/// name. `category_term` always means "terms"; any other target type is
/// another content type, whose own label this reads from the registry. A
/// target type nothing can name (a stale field pointing at a removed type)
/// is left out, so the widget falls back to the builder's default
/// placeholder.
///
/// The label itself is not translated — it is the site's own data, the same
/// as a menu title or a tile's text, not an interface string — only the
/// surrounding "Search %s..." phrase is, through [`translate`].
fn record_reference_placeholders(
    state: &AppState,
    content_type: &ContentTypeDefinition,
    language: &str,
) -> HashMap<String, String> {
    let template = translate(state, "Search %s...", language);
    let mut placeholders = HashMap::new();
    for field in &content_type.fields {
        let FieldType::RecordReference(target) = &field.field_type else {
            continue;
        };
        if placeholders.contains_key(target) {
            continue;
        }
        let label = if target == "category_term" {
            Some(translate(state, "terms", language))
        } else {
            state.content_types().get(target).map(|ct| ct.label)
        };
        if let Some(label) = label.filter(|l| !l.trim().is_empty()) {
            placeholders.insert(target.clone(), template.replacen("%s", &label, 1));
        }
    }
    placeholders
}

/// What the item form needs of content moderation, on a moderated type.
pub(crate) struct ModerationContext {
    /// The state the item is in: Draft for a new item.
    pub state: &'static str,
    /// The item as an editor works on it ([`ItemService::load_for_moderation`]);
    /// `None` for a new item.
    ///
    /// [`ItemService::load_for_moderation`]: crate::content::ItemService::load_for_moderation
    pub working: Option<Item>,
}

/// The moderation context of the form for `item` (or a new item) of
/// `type_name`: `None` when the type is not moderated.
pub(crate) async fn moderation_context(
    state: &AppState,
    type_name: &str,
    item: Option<&Item>,
) -> Option<ModerationContext> {
    match item {
        None => state
            .items()
            .is_moderated(type_name)
            .await
            .then_some(ModerationContext {
                state: crate::content::moderation::DRAFT,
                working: None,
            }),
        Some(item) => match state.items().load_for_moderation(item).await {
            Ok(found) => found.map(|(state, working)| ModerationContext {
                state,
                working: Some(working),
            }),
            Err(e) => {
                tracing::warn!(error = %e, item_id = %item.id, "loading the latest revision failed");
                None
            }
        },
    }
}

/// The moderation control: the current state, and a `moderation_state`
/// select of the states this user may move the item to, defaulting to the
/// current one when they may stay. The service decides the transition again.
fn moderation_element(user: &UserContext, current: &'static str) -> (String, FormElement) {
    use crate::content::moderation;

    let targets = moderation::targets(current, user);
    let current_label = moderation::label(current);
    let mut fieldset = FormElement::fieldset()
        .title("Moderation state")
        .weight(crate::content::STATUS_WEIGHT)
        .attribute("id", "moderation-state")
        .child(
            "_moderation_current",
            FormElement::markup(format!(
                r#"<p class="moderation-current">Current state: <strong>{current_label}</strong></p>"#
            ))
            .weight(0),
        );
    if targets.is_empty() {
        fieldset = fieldset.child(
            "_moderation_none",
            FormElement::markup(
                r#"<p class="moderation-none">You may not change the state of this item.</p>"#,
            )
            .weight(1),
        );
    } else {
        let options: Vec<(String, String)> = targets
            .iter()
            .map(|s| ((*s).to_string(), moderation::label(s).to_string()))
            .collect();
        let default = moderation::default_target(current, user).unwrap_or(current);
        let mut select = FormElement::select(options)
            .title("Change to")
            .default_value(default)
            .weight(1);
        // Required, so the template offers no empty "- Select -" of its own.
        select.required = true;
        fieldset = fieldset.child("moderation_state", select);
    }
    ("moderation".to_string(), fieldset)
}

/// The stage selector: the stages this user may move `item` to by the
/// editorial workflow, or nothing when there are none. The submit decides the
/// move again.
pub(crate) async fn stage_element(
    state: &AppState,
    user: &UserContext,
    item: &Item,
) -> Vec<(String, FormElement)> {
    let targets = state
        .items()
        .move_targets(item, user)
        .await
        .unwrap_or_default();
    if targets.is_empty() {
        return Vec::new();
    }
    let current = state
        .items()
        .stage_view()
        .await
        .ok()
        .and_then(|view| view.stage(item.stage_id).map(|s| s.label.clone()))
        .unwrap_or_default();
    let mut options = vec![(String::new(), format!("Stay on {current}"))];
    options.extend(targets.into_iter().map(|s| (s.id.to_string(), s.label)));
    let mut select = FormElement::select(options)
        .title("Move to stage")
        .weight(crate::content::STATUS_WEIGHT + 10);
    // Required, so the template offers no "- Select -" of its own: the first
    // option, "Stay on …", is the empty answer.
    select.required = true;
    vec![("_stage".to_string(), select)]
}

/// Check a submission: CSRF, the form state, the title, the submission builder
/// and `tap_form_validate`. On success, the form state and the field map to
/// save.
pub(crate) async fn process(
    state: &AppState,
    session: &Session,
    user: &UserContext,
    content_type: &ContentTypeDefinition,
    item: Option<&Item>,
    posted: &PostedForm,
) -> Result<(FormState, Map<String, Value>), ProcessError> {
    let build_id = posted.get("_form_build_id").unwrap_or("").to_string();
    let formats = permitted_text_formats(user);
    // A save builds on what the form showed: on a moderated type, the latest
    // revision.
    let working = match item {
        Some(stored) => moderation_context(state, &content_type.machine_name, Some(stored))
            .await
            .and_then(|m| m.working),
        None => None,
    };
    let item = working.as_ref().or(item);
    // A save of a type that must be previewed needs a form built under a
    // preview. A preview itself is always allowed.
    let must_preview = !wants_preview(posted)
        && preview_mode(state, &content_type.machine_name).await == PreviewMode::Required;
    state
        .forms()
        .process(
            &build_id,
            posted,
            session,
            &tap_state(state, user),
            |form_state| {
                if form_state.item_type.as_deref() != Some(content_type.machine_name.as_str())
                    || form_state.item_id != item.map(|i| i.id)
                {
                    return Err(vec![ValidationError::form(crate::form::FORM_EXPIRED)]);
                }
                let mut errors = Vec::new();
                if must_preview
                    && form_state.extra.get(preview::PREVIEWED) != Some(&Value::Bool(true))
                {
                    errors.push(ValidationError::form("Preview this item before saving it."));
                }
                if posted.get("title").is_none_or(|t| t.trim().is_empty()) {
                    errors.push(ValidationError::field("title", "Title is required."));
                }
                let built = build_item_fields(
                    item.and_then(|i| i.fields.as_object()),
                    content_type,
                    &form_state.rendered_fields,
                    posted,
                    &formats,
                );
                // A page builder field's page is checked against the
                // registry here, so the form shows each refused value at its
                // field; ItemService checks it again on every other path.
                if let Ok(fields) = &built {
                    errors.extend(page_builder_errors(
                        state.page_builder_components(),
                        content_type,
                        fields,
                        item.map(|i| &i.fields),
                    ));
                }
                match built {
                    Ok(fields) if errors.is_empty() => Ok(fields),
                    Ok(_) => Err(errors),
                    Err(more) => {
                        errors.extend(more);
                        Err(errors)
                    }
                }
            },
        )
        .await
}

/// Every registry error in the page builder fields `fields` changes, each as
/// a form error on its item field naming the component and component field.
fn page_builder_errors(
    registry: &crate::content::page_builder_components::ComponentRegistry,
    content_type: &ContentTypeDefinition,
    fields: &Map<String, Value>,
    stored: Option<&Value>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    for def in content_type
        .fields
        .iter()
        .filter(|f| matches!(f.field_type, trovato_sdk::types::FieldType::PageBuilder))
    {
        let Some(value) = fields.get(&def.field_name).filter(|v| !v.is_null()) else {
            continue;
        };
        if stored.and_then(|s| s.get(&def.field_name)) == Some(value) {
            continue;
        }
        for error in registry.validate_page(value) {
            errors.push(ValidationError::field(
                def.field_name.clone(),
                format!("{}: {error}", def.label),
            ));
        }
    }
    errors
}

/// The form rebuilt for a submission that failed, showing what was posted.
///
/// A fresh build (new token, new state), then each posted value put back as
/// its element's default. A checkbox the form rendered and the post left out
/// is unchecked, which is what the user last saw.
pub(crate) fn redisplay(form: &mut Form, posted: &PostedForm) {
    fn walk(elements: &mut std::collections::BTreeMap<String, FormElement>, posted: &PostedForm) {
        for (name, element) in elements.iter_mut() {
            if element.disabled || name.starts_with('_') {
                continue;
            }
            match element.element_type {
                ElementType::Checkbox => {
                    element.default_value = Some(Value::Bool(posted.contains(name)));
                }
                ElementType::Submit { .. }
                | ElementType::Markup { .. }
                | ElementType::Hidden
                | ElementType::Password => {}
                _ => {
                    if let Some(value) = posted.get(name) {
                        element.default_value = Some(Value::String(value.to_string()));
                    }
                }
            }
            walk(&mut element.children, posted);
        }
    }
    walk(&mut form.elements, posted);
}

/// `POST /system/ajax`, after the CSRF header has been checked.
///
/// The form state must have been built for this user, and they must still be
/// allowed to open the form: `edit` access to the item for an edit form, the
/// type's create permission for an add form. Anything else is 403. The
/// built-in trigger `add_{field}` builds the next value of a multi value field
/// the form rendered as editable; any other trigger goes to `tap_form_ajax`,
/// with the services, so a plugin can read its tables.
pub(crate) async fn ajax(
    state: &AppState,
    session: &Session,
    request: &crate::form::AjaxRequest,
) -> axum::response::Response {
    use crate::form::AjaxResponse;
    use axum::Json;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let refused = || {
        (
            StatusCode::FORBIDDEN,
            Json(AjaxResponse::new().alert("You may not use this form. Reload the page.")),
        )
            .into_response()
    };

    let user = super::helpers::get_user_context(session, state).await;
    let Some(mut form_state) = state.forms().recall(&request.form_build_id, user.id).await else {
        return refused();
    };
    if !may_open(state, &user, &form_state).await {
        return refused();
    }

    let response = match request.trigger.strip_prefix("add_") {
        Some(field) if form_state.rendered_fields.iter().any(|f| f == field) => {
            let language = active_language(state, session).await;
            add_another(state, &mut form_state, field, &language)
        }
        _ => {
            state
                .forms()
                .ajax_tap(
                    &request.trigger,
                    &form_state,
                    &request.values,
                    &tap_state(state, &user),
                )
                .await
        }
    };
    if let Err(e) = state
        .forms()
        .save_state(&request.form_build_id, &form_state)
        .await
    {
        tracing::warn!(error = %e, "failed to save form state after AJAX");
    }
    Json(response).into_response()
}

/// Whether `user` may still open the form `form_state` was built for.
async fn may_open(state: &AppState, user: &UserContext, form_state: &FormState) -> bool {
    if let Some(item_id) = form_state.item_id {
        let Ok(Some(item)) = state.items().load(item_id).await else {
            return false;
        };
        return state
            .items()
            .check_access(&item, "edit", user)
            .await
            .unwrap_or(false);
    }
    if let Some(item_type) = &form_state.item_type {
        return user.can(&format!("create {item_type} content"));
    }
    // Any other form: the state was built for this user, which is the check.
    true
}

/// The next value of the multi value field `field`: the widget `FormBuilder`
/// builds for an empty value, appended to the field's items, until the
/// field's cardinality is reached.
fn add_another(
    state: &AppState,
    form_state: &mut FormState,
    field: &str,
    language: &str,
) -> crate::form::AjaxResponse {
    use crate::form::AjaxResponse;

    let Some(definition) = form_state
        .item_type
        .as_deref()
        .and_then(|t| state.content_types().get(t))
        .and_then(|ct| {
            ct.fields
                .iter()
                .find(|f| f.field_name == field && f.cardinality != 1)
                .cloned()
                .map(|f| (ct, f))
        })
    else {
        return AjaxResponse::new();
    };
    let (content_type, definition) = definition;

    let count_key = format!("{field}_count");
    let count = form_state
        .extra
        .get(&count_key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    if usize::try_from(definition.cardinality).is_ok_and(|max| count >= max) {
        return AjaxResponse::new().alert(format!(
            "{} allows at most {} values.",
            definition.label, definition.cardinality
        ));
    }

    let placeholders = record_reference_placeholders(state, &content_type, language);
    let mut widget = FormBuilder::new(content_type)
        .with_item_id(form_state.item_id)
        .with_placeholders(placeholders)
        .with_default_placeholder(translate(state, "Search...", language))
        .with_owner_hint(translate(state, "Choose the owner first", language))
        .widget(&definition, None, FieldMode::Editable);
    widget.title = None;
    widget.required = false;
    let name = format!("{field}[{count}]");
    let html = match state.theme().render_form_element_html(&name, &widget) {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, field, "failed to render another value");
            return AjaxResponse::new().alert("An error occurred. Please try again.");
        }
    };
    form_state.extra.insert(count_key, Value::from(count + 1));
    AjaxResponse::new()
        .append(format!("#{field}-items"), html)
        .invoke(
            "Trovato.updateFieldDelta",
            serde_json::json!({ "field": field, "count": count + 1 }),
        )
}

/// Posted beside promote and sticky when the form rendered them: an unchecked
/// box posts nothing, so without it "unchecked" and "never offered" are the
/// same request.
pub(crate) const PROMOTION_RENDERED: &str = "_promotion_rendered";
/// Posted beside the author and authored on inputs when the form rendered
/// them.
pub(crate) const AUTHORING_RENDERED: &str = "_authoring_rendered";

/// The `datetime-local` value format without seconds, which the authored on
/// input also reads.
const AUTHORED_ON_FORMAT: &str = "%Y-%m-%dT%H:%M";
/// The same with seconds, the shape the form shows a stored time in.
const AUTHORED_ON_SECONDS_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// The "Publishing options" section of the item form (B107): promoted to the
/// front page and sticky at the top of lists for a holder of `publish content`
/// or `administer content`, and the author and authored on time for a holder
/// of `administer content`. Nothing for a user who may set none of them. The
/// service decides every value again ([`ItemService::gate_promotion`],
/// [`ItemService::gate_authoring`]); this only decides what is offered.
///
/// [`ItemService::gate_promotion`]: crate::content::ItemService::gate_promotion
/// [`ItemService::gate_authoring`]: crate::content::ItemService::gate_authoring
pub(crate) async fn publishing_element(
    state: &AppState,
    user: &UserContext,
    item_type: &str,
    item: Option<&Item>,
) -> Vec<(String, FormElement)> {
    use crate::content::ADMINISTER_CONTENT;

    let may_author = user.can(ADMINISTER_CONTENT);
    let may_promote = may_author || user.can(PUBLISH_CONTENT);
    if !may_promote {
        return Vec::new();
    }

    // A new item starts from its type's defaults, an existing one from what
    // it holds.
    let (promoted, sticky) = match item {
        Some(item) => (item.is_promoted(), item.is_sticky()),
        None => {
            let (promote, sticky) = state.items().publishing_defaults(item_type).await;
            (promote == 1, sticky == 1)
        }
    };

    let mut fieldset = FormElement::fieldset()
        .title("Publishing options")
        .weight(crate::content::STATUS_WEIGHT + 5)
        .attribute("id", "publishing-options")
        .child(
            "promote",
            FormElement::checkbox()
                .title("Promoted to front page")
                .default_value(promoted)
                .weight(0),
        )
        .child(
            "sticky",
            FormElement::checkbox()
                .title("Sticky at top of lists")
                .default_value(sticky)
                .weight(1),
        )
        .child(
            PROMOTION_RENDERED,
            FormElement::hidden().default_value("1").weight(2),
        );

    if may_author {
        let author = match item {
            Some(item) => state
                .users()
                .find_by_id(item.author_id)
                .await
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_default(),
            None => String::new(),
        };
        let authored_on = item
            .and_then(|i| chrono::DateTime::from_timestamp(i.created, 0))
            // To the second, as stored: a minute value would move `created`
            // back to the minute on every save that changed nothing. Whole
            // minutes without seconds, as a browser normalizes them.
            .map(|t| {
                use chrono::Timelike;
                let format = if t.second() == 0 {
                    AUTHORED_ON_FORMAT
                } else {
                    AUTHORED_ON_SECONDS_FORMAT
                };
                t.format(format).to_string()
            })
            .unwrap_or_default();
        fieldset = fieldset
            .child(
                "author",
                FormElement::textfield()
                    .title("Authored by")
                    .description("A username. Leave empty for yourself.")
                    .default_value(author)
                    .weight(3),
            )
            .child(
                "authored_on",
                FormElement::input("datetime-local")
                    .title("Authored on (UTC)")
                    // Seconds are allowed, so a stored time to the second is valid.
                    .attribute("step", "1")
                    .description("Leave empty for now.")
                    .default_value(authored_on)
                    .weight(4),
            )
            .child(
                AUTHORING_RENDERED,
                FormElement::hidden().default_value("1").weight(5),
            );
    }

    vec![("publishing".to_string(), fieldset)]
}

/// What a submission asked of the publishing options: `None` for anything it
/// did not say, which the service reads as "leave it".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Publishing {
    pub promote: Option<i16>,
    pub sticky: Option<i16>,
    pub author_id: Option<Uuid>,
    pub created: Option<i64>,
}

/// Read the publishing options from a submission, checking that a named
/// author is a user and that the authored on time parses. Whether this user
/// may set them is the service's decision, not this one's.
pub(crate) async fn requested_publishing(
    state: &AppState,
    posted: &PostedForm,
) -> Result<Publishing, Vec<ValidationError>> {
    let mut out = Publishing::default();
    let mut errors = Vec::new();

    if posted.contains(PROMOTION_RENDERED) {
        out.promote = Some(i16::from(posted.contains("promote")));
        out.sticky = Some(i16::from(posted.contains("sticky")));
    }

    if posted.contains(AUTHORING_RENDERED) {
        if let Some(name) = posted
            .get("author")
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            match state.users().find_by_name(name).await {
                Ok(Some(user)) => out.author_id = Some(user.id),
                Ok(None) => errors.push(ValidationError::field(
                    "author",
                    format!("There is no user named {name}."),
                )),
                Err(e) => {
                    tracing::error!(error = %e, "author lookup failed");
                    errors.push(ValidationError::field(
                        "author",
                        "The author could not be checked.",
                    ));
                }
            }
        }
        if let Some(value) = posted
            .get("authored_on")
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            match parse_authored_on(value) {
                Some(created) => out.created = Some(created),
                None => errors.push(ValidationError::field(
                    "authored_on",
                    "Authored on must be a date and time.",
                )),
            }
        }
    }

    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// A `datetime-local` value (with or without seconds), read as UTC.
fn parse_authored_on(value: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(value, AUTHORED_ON_FORMAT)
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(value, AUTHORED_ON_SECONDS_FORMAT))
        .ok()
        .map(|t| t.and_utc().timestamp())
}

#[cfg(test)]
mod publishing_tests {
    use super::parse_authored_on;

    #[test]
    fn authored_on_reads_a_datetime_local_value_as_utc() {
        assert_eq!(parse_authored_on("2026-01-02T03:04"), Some(1_767_323_040));
        assert_eq!(
            parse_authored_on("2026-01-02T03:04:05"),
            Some(1_767_323_045)
        );
        assert_eq!(parse_authored_on("yesterday"), None);
        assert_eq!(parse_authored_on("2026-13-02T03:04"), None);
    }
}
