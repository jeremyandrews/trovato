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
pub(crate) async fn build(
    state: &AppState,
    session: &Session,
    user: &UserContext,
    content_type: &ContentTypeDefinition,
    item: Option<&Item>,
    action: &str,
    extra: Vec<(String, FormElement)>,
) -> anyhow::Result<Form> {
    let type_name = &content_type.machine_name;
    let modes =
        super::item::field_modes_for(state, user, type_name, content_type, item.is_none()).await;
    let titles = match item {
        Some(item) => reference_titles(state, user, content_type, item, &modes).await,
        None => HashMap::new(),
    };
    let builder = FormBuilder::new(content_type.clone())
        .with_permitted_formats(permitted_text_formats(user))
        .with_reference_titles(titles)
        .with_field_modes(modes)
        .with_publish_allowed(user.can(PUBLISH_CONTENT));
    let form = match item {
        Some(item) => builder.build_edit_form(item, action),
        None => builder.build_add_form(action),
    }
    .elements(extra);

    let mut form_state = FormState::new(form.form_id.clone(), form.form_build_id.clone());
    form_state.user_id = Some(user.id);
    form_state.item_id = item.map(|i| i.id);
    form_state.item_type = Some(type_name.clone());
    state
        .forms()
        .build(form, session, &tap_state(state, user), form_state)
        .await
}

/// The display title of every `RecordReference` target the item points at,
/// for a target this user may view: one query for all of them, multi value
/// fields included. A target they may not see keeps its id and no title.
async fn reference_titles(
    state: &AppState,
    user: &UserContext,
    content_type: &ContentTypeDefinition,
    item: &Item,
    modes: &HashMap<String, FieldMode>,
) -> HashMap<String, String> {
    let mut ids: Vec<Uuid> = Vec::new();
    for field in &content_type.fields {
        if !matches!(field.field_type, FieldType::RecordReference(_))
            || modes.get(&field.field_name) == Some(&FieldMode::Hidden)
        {
            continue;
        }
        let values: Vec<&Value> = match item.fields.get(&field.field_name) {
            Some(Value::Array(values)) => values.iter().collect(),
            Some(value) => vec![value],
            None => Vec::new(),
        };
        ids.extend(values.into_iter().filter_map(|v| {
            crate::content::extract_reference_id(Some(v))
                .parse::<Uuid>()
                .ok()
        }));
    }
    ids.sort_unstable();
    ids.dedup();

    let targets = Item::find_by_ids(state.db(), &ids)
        .await
        .unwrap_or_default();
    let mut titles = HashMap::new();
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
