//! Admin routes for editorial stage management.
//!
//! A stage is two rows: a `category_tag` in the `stages` category carrying label,
//! description and weight, and a `stage_config` row carrying machine name,
//! visibility and the default flag. Until these routes existed the only way to
//! write either was `trovato config import`, which `KNOWN-ISSUES.md` and
//! `ROADMAP.md` both listed as a thing to fix before 1.0.
//!
//! ## What the form offers, and what it does not
//!
//! Machine name, label, description, visibility, default, weight. That is what the
//! schema models, and the form does not widen it.
//!
//! In particular there is **no workflow membership field**. Which moves between
//! stages are allowed is the `workflow.editorial` site configuration variable
//! (`models/workflow.rs`), managed by `config import`, which validates it; a
//! stage does not belong to a workflow, a transition names two stages. See
//! `docs/design/stage-workflow.md`.
//!
//! ## The guard rails, and where they live
//!
//! Every rule is on the model ([`Stage::update`], [`Stage::delete`]) rather than in
//! these handlers, so `config import` and any future caller are held to the same
//! ones. The form's job is to say what happened in a sentence rather than a
//! constraint violation.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::models::stage::{LIVE_STAGE_ID, Stage, StageVisibility, UpdateStage};
use crate::models::{CreateStage, StageReferences, User};
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    MACHINE_NAME_ERROR, is_valid_machine_name, render_admin_template, render_not_found,
    render_server_error, require_permission,
};

/// The id of the add and edit form.
const STAGE_FORM_ID: &str = "stage_form";

/// The id of the form that confirms a stage's deletion.
const DELETE_FORM_ID: &str = "stage_delete_form";

/// The stage listing, where every form here returns.
const BASE: &str = "/admin/structure/stages";

/// The visibility values a stage may take, for the select.
const VISIBILITIES: [(&str, &str); 3] = [
    (
        "internal",
        "Internal — visible to editors with stage access",
    ),
    ("public", "Public — visible to every visitor"),
    (
        "accessible",
        "Accessible — reachable by direct URL, absent from listings",
    ),
];

// =============================================================================
// Form data
// =============================================================================

/// What the add and edit form shows in its fields: a stage's saved values, or
/// what was submitted when the form is shown again with its errors.
#[derive(Debug, Clone)]
struct StageValues {
    machine_name: String,
    label: String,
    description: String,
    visibility: String,
    is_default: bool,
    /// The weight as typed, so a value that is not a number is shown back.
    weight: String,
}

impl StageValues {
    /// Empty values, for a stage not yet made.
    fn blank() -> Self {
        Self {
            machine_name: String::new(),
            label: String::new(),
            description: String::new(),
            visibility: "internal".to_string(),
            is_default: false,
            weight: "0".to_string(),
        }
    }

    /// A saved stage's values.
    fn of(stage: &Stage) -> Self {
        Self {
            machine_name: stage.machine_name.clone(),
            label: stage.label.clone(),
            description: stage.description.clone().unwrap_or_default(),
            visibility: stage.visibility.as_str().to_string(),
            is_default: stage.is_default,
            weight: stage.weight.to_string(),
        }
    }

    /// What was posted. `is_default` is a checkbox, absent when unchecked.
    fn posted(posted: &PostedForm) -> Self {
        Self {
            machine_name: posted.get("machine_name").unwrap_or_default().to_string(),
            label: posted.get("label").unwrap_or_default().to_string(),
            description: posted.get("description").unwrap_or_default().to_string(),
            visibility: posted.get("visibility").unwrap_or_default().to_string(),
            is_default: posted.get("is_default").is_some(),
            weight: posted.get("weight").unwrap_or_default().to_string(),
        }
    }

    /// The description to save: trimmed, and `None` when blank.
    fn description(&self) -> Option<String> {
        Some(self.description.trim().to_string()).filter(|d| !d.is_empty())
    }
}

// =============================================================================
// Display structs
// =============================================================================

/// One stage in the listing.
#[derive(Debug, Serialize)]
struct StageRow {
    id: Uuid,
    machine_name: String,
    label: String,
    description: Option<String>,
    visibility: String,
    is_default: bool,
    weight: i16,
    /// Whether this is the Live stage, which cannot be deleted or demoted.
    is_live: bool,
    /// What references it, so a delete refusal can be predicted rather than
    /// discovered.
    references: StageReferences,
    /// A rendering of `references` for the confirmation text.
    references_summary: String,
    /// Whether delete is offered at all.
    deletable: bool,
}

/// `stage` as the listing and the delete confirmation show it, with what
/// references it.
async fn stage_row(state: &AppState, stage: Stage) -> Result<StageRow, Response> {
    let references = match Stage::reference_counts(state.db(), stage.id).await {
        Ok(references) => references,
        Err(e) => {
            tracing::error!(error = %e, stage = %stage.machine_name, "failed to count stage references");
            return Err(render_server_error(
                "Failed to count what references a stage.",
            ));
        }
    };
    let is_live = stage.id == LIVE_STAGE_ID;
    Ok(StageRow {
        id: stage.id,
        machine_name: stage.machine_name,
        label: stage.label,
        description: stage.description,
        visibility: stage.visibility.as_str().to_string(),
        is_default: stage.is_default,
        weight: stage.weight,
        is_live,
        references_summary: references.describe(),
        // Live, the default and the public stage are refused by the model.
        // Content is refused too, but that is worth showing rather than
        // hiding, so the link stays and the confirmation says what holds it.
        deletable: !is_live && !stage.is_default && stage.visibility != StageVisibility::Public,
        references,
    })
}

// =============================================================================
// Handlers
// =============================================================================

/// List stages.
///
/// GET /admin/structure/stages
async fn list_stages(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let stages = match Stage::list_all(state.db()).await {
        Ok(stages) => stages,
        Err(e) => {
            tracing::error!(error = %e, "failed to list stages");
            return render_server_error("Failed to load stages.");
        }
    };

    let mut rows = Vec::with_capacity(stages.len());
    for stage in stages {
        match stage_row(&state, stage).await {
            Ok(row) => rows.push(row),
            Err(response) => return response,
        }
    }

    let mut context = tera::Context::new();
    context.insert("stages", &rows);
    context.insert("path", BASE);

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/stages.html",
        context,
    )
    .await
}

/// The add and edit form, filled with `values`.
fn stage_form(
    action: &str,
    editing: bool,
    values: &StageValues,
    cancel: String,
) -> crate::form::Form {
    let visibilities = VISIBILITIES
        .iter()
        .map(|(value, label)| ((*value).to_string(), (*label).to_string()))
        .collect();
    crate::form::Form::new(STAGE_FORM_ID)
        .action(action)
        .element(
            "machine_name",
            FormElement::textfield()
                .title("Machine name")
                .description(
                    "Lowercase letters, numbers and underscores, starting with a letter. This is \
                     what config files and workflow definitions refer to the stage by.",
                )
                .default_value(values.machine_name.clone())
                .attribute("spellcheck", "false")
                .attribute("autocapitalize", "none")
                .required()
                .weight(0),
        )
        .element(
            "label",
            FormElement::textfield()
                .title("Label")
                .default_value(values.label.clone())
                .required()
                .weight(1),
        )
        .element(
            "description",
            FormElement::textfield()
                .title("Description")
                .description("Optional. Shown to editors in the stage listing.")
                .default_value(values.description.clone())
                .weight(2),
        )
        .element(
            "visibility",
            FormElement::select(visibilities)
                .title("Visibility")
                .description("Only one stage can be public, and it is the Live stage.")
                .default_value(values.visibility.clone())
                .required()
                .weight(3),
        )
        .element(
            "is_default",
            FormElement::checkbox()
                .title("Default stage for new content")
                .description(
                    "Exactly one stage is the default. Checking this here clears it on whichever \
                     stage holds it now. It cannot be cleared without giving it to another \
                     stage: new content has to land somewhere.",
                )
                .default_value(values.is_default)
                .weight(4),
        )
        .element(
            "weight",
            FormElement::input("number")
                .title("Weight")
                .description("Lower sorts first in the listing.")
                .default_value(values.weight.clone())
                .attribute("step", "1")
                .weight(5),
        )
        .element(
            "op",
            FormElement::submit(if editing {
                "Save changes"
            } else {
                "Create stage"
            })
            .suffix(cancel)
            .weight(10),
        )
}

/// Render the add/edit form.
async fn render_form(
    state: &AppState,
    session: &Session,
    user: &User,
    stage_id: Option<Uuid>,
    values: &StageValues,
    errors: &[ValidationError],
) -> Response {
    let action = match stage_id {
        Some(id) => format!("{BASE}/{id}/edit"),
        None => format!("{BASE}/add"),
    };

    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, BASE).await;
    let cancel = format!(
        r#"<a href="{BASE}" class="button button--secondary">{}</a>"#,
        text.html("Cancel", &[])
    );
    let form = stage_form(&action, stage_id.is_some(), values, cancel);
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("editing", &stage_id.is_some());
    context.insert("is_live", &(stage_id == Some(LIVE_STAGE_ID)));
    context.insert("label", &values.label);
    context.insert("form", &form_html);
    context.insert("path", &action);

    render_admin_template(state, &viewer, "admin/stage-form.html", context).await
}

/// Add-stage form.
///
/// GET /admin/structure/stages/add
async fn add_stage_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_form(&state, &session, &user, None, &StageValues::blank(), &[]).await
}

/// The refusal of a machine name another stage already has.
fn already_exists(machine_name: &str) -> ValidationError {
    ValidationError::field(
        "machine_name",
        format!("A stage named '{machine_name}' already exists."),
    )
}

/// Validate the fields both submit paths share, returning the errors and the
/// weight to save (`fallback` when the field is blank).
fn validate(values: &StageValues, fallback: i16) -> (Vec<ValidationError>, i16) {
    let mut errors = Vec::new();

    if values.machine_name.trim().is_empty() {
        errors.push(ValidationError::field(
            "machine_name",
            "Machine name is required.",
        ));
    } else if !is_valid_machine_name(values.machine_name.trim()) {
        errors.push(ValidationError::field("machine_name", MACHINE_NAME_ERROR));
    }
    if values.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Label is required."));
    }
    if values.visibility.parse::<StageVisibility>().is_err() {
        errors.push(ValidationError::field(
            "visibility",
            "Visibility must be internal, public or accessible.",
        ));
    }
    let weight = match values.weight.trim() {
        "" => fallback,
        raw => raw.parse::<i16>().unwrap_or_else(|_| {
            errors.push(ValidationError::field(
                "weight",
                "Weight must be a whole number.",
            ));
            fallback
        }),
    };

    (errors, weight)
}

/// Add-stage submit.
///
/// POST /admin/structure/stages/add
async fn add_stage_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let values = StageValues::posted(&posted);
    let (mut errors, weight) = validate(&values, 0);
    let machine_name = values.machine_name.trim().to_string();

    if errors.is_empty() {
        match Stage::find_by_machine_name(state.db(), &machine_name).await {
            Ok(Some(_)) => {
                errors.push(already_exists(&machine_name));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to check for an existing stage");
                errors.push(ValidationError::form(
                    "Failed to check for an existing stage.",
                ));
            }
        }
    }

    // A second public stage cannot exist: the partial unique index on
    // `visibility = 'public'` says so, and the render layer resolves published
    // content through the one that does.
    if errors.is_empty() && values.visibility == "public" {
        errors.push(ValidationError::field(
            "visibility",
            "Only one stage can be public, and it is the Live stage. Use internal or accessible.",
        ));
    }

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, STAGE_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_form(&state, &session, &user, None, &values, &errors).await;
            }
            Err(Err(response)) => return response,
        };

    let input = CreateStage {
        label: values.label.trim().to_string(),
        machine_name,
        description: values.description(),
        visibility: Some(values.visibility.clone()),
        is_default: Some(values.is_default),
        weight: Some(weight),
    };

    // A new default has to displace the old one, and only `Stage::update` knows
    // how to do that atomically, so create first and then set the flag.
    let wants_default = values.is_default;
    let created = match state
        .stage()
        .create_stage(CreateStage {
            is_default: Some(false),
            ..input
        })
        .await
    {
        Ok(stage) => stage,
        Err(e) => {
            tracing::error!(error = %e, "failed to create stage");
            return render_form(
                &state,
                &session,
                &user,
                None,
                &values,
                &[ValidationError::form(format!(
                    "Failed to create the stage: {e}"
                ))],
            )
            .await;
        }
    };

    if wants_default
        && let Err(e) = state
            .stage()
            .update_stage(
                created.id,
                UpdateStage {
                    is_default: Some(true),
                    ..UpdateStage::default()
                },
            )
            .await
    {
        tracing::error!(error = %e, "failed to make the new stage default");
        return render_form(
            &state,
            &session,
            &user,
            Some(created.id),
            &values,
            &[ValidationError::form(format!(
                "The stage was created, but making it the default failed: {e}"
            ))],
        )
        .await;
    }

    admin_form::submitted(&state, &user, &form_state, &posted).await;
    tracing::info!(stage = %created.machine_name, "stage created");
    Redirect::to(BASE).into_response()
}

/// Load the stage `stage_id`, or the response that says why not.
async fn load_stage(state: &AppState, stage_id: Uuid) -> Result<Stage, Response> {
    match Stage::find_by_id(state.db(), stage_id).await {
        Ok(Some(stage)) => Ok(stage),
        Ok(None) => Err(render_not_found()),
        Err(e) => {
            tracing::error!(error = %e, "failed to load stage");
            Err(render_server_error("Failed to load the stage."))
        }
    }
}

/// Edit-stage form.
///
/// GET /admin/structure/stages/{id}/edit
async fn edit_stage_form(
    State(state): State<AppState>,
    session: Session,
    Path(stage_id): Path<Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let stage = match load_stage(&state, stage_id).await {
        Ok(stage) => stage,
        Err(response) => return response,
    };
    render_form(
        &state,
        &session,
        &user,
        Some(stage_id),
        &StageValues::of(&stage),
        &[],
    )
    .await
}

/// Edit-stage submit.
///
/// POST /admin/structure/stages/{id}/edit
async fn edit_stage_submit(
    State(state): State<AppState>,
    session: Session,
    Path(stage_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Ok(Some(existing)) = Stage::find_by_id(state.db(), stage_id).await else {
        return render_not_found();
    };

    let values = StageValues::posted(&posted);
    let (mut errors, weight) = validate(&values, existing.weight);
    let machine_name = values.machine_name.trim().to_string();

    if errors.is_empty() && machine_name != existing.machine_name {
        match Stage::find_by_machine_name(state.db(), &machine_name).await {
            Ok(Some(_)) => errors.push(already_exists(&machine_name)),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to check for an existing stage");
                errors.push(ValidationError::form(
                    "Failed to check for an existing stage.",
                ));
            }
        }
    }

    // Promoting a second stage to public is refused for the same reason a second
    // public stage cannot be created.
    if errors.is_empty()
        && values.visibility == "public"
        && existing.visibility != StageVisibility::Public
    {
        errors.push(ValidationError::field(
            "visibility",
            "Only one stage can be public, and the Live stage already is. Use internal or \
             accessible.",
        ));
    }

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, STAGE_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_form(&state, &session, &user, Some(stage_id), &values, &errors)
                    .await;
            }
            Err(Err(response)) => return response,
        };

    let input = UpdateStage {
        label: Some(values.label.trim().to_string()),
        machine_name: Some(machine_name),
        description: Some(values.description()),
        visibility: Some(values.visibility.clone()),
        is_default: Some(values.is_default),
        weight: Some(weight),
    };

    match state.stage().update_stage(stage_id, input).await {
        Ok(Some(_)) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(stage_id = %stage_id, "stage updated");
            Redirect::to(BASE).into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            // The model's refusals are the interesting messages here (the Live
            // stage staying public, the last default staying set), so they are
            // shown rather than flattened into "failed to save".
            tracing::info!(error = %e, stage_id = %stage_id, "stage update refused");
            render_form(
                &state,
                &session,
                &user,
                Some(stage_id),
                &values,
                &[ValidationError::form(e.to_string())],
            )
            .await
        }
    }
}

/// The form that confirms a stage's deletion: one button.
fn delete_form_for(stage_id: Uuid) -> crate::form::Form {
    crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{stage_id}/delete"))
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .weight(10),
        )
}

/// Ask before deleting a stage, saying what still uses it.
///
/// GET /admin/structure/stages/{id}/delete
async fn delete_stage_form(
    State(state): State<AppState>,
    session: Session,
    Path(stage_id): Path<Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let stage = match load_stage(&state, stage_id).await {
        Ok(stage) => stage,
        Err(response) => return response,
    };
    let row = match stage_row(&state, stage).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    // A stage the model will refuse to delete gets no button, only the reason,
    // as the listing shows it.
    let form_html = if row.deletable {
        match admin_form::render(&state, &session, &user, delete_form_for(stage_id), &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        }
    } else {
        String::new()
    };

    let mut context = tera::Context::new();
    context.insert("stage", &row);
    context.insert("form", &form_html);
    context.insert("path", &format!("{BASE}/{stage_id}/delete"));
    render_admin_template(
        &state,
        &super::helpers::user_context_for(&state, &user).await,
        "admin/stage-delete.html",
        context,
    )
    .await
}

/// Delete a stage.
///
/// POST /admin/structure/stages/{id}/delete
async fn delete_stage(
    State(state): State<AppState>,
    session: Session,
    Path(stage_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, DELETE_FORM_ID, Vec::new()).await
        {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return super::helpers::render_error(
                    "The form has expired. Reload the page and try again.",
                );
            }
            Err(Err(response)) => return response,
        };

    match state.stage().delete_stage(stage_id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(stage_id = %stage_id, "stage deleted");
            Redirect::to(BASE).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            // Every refusal here is a sentence the operator needs: which stage
            // cannot go, or how much content is in the way.
            tracing::info!(error = %e, stage_id = %stage_id, "stage delete refused");
            (
                axum::http::StatusCode::CONFLICT,
                axum::response::Html(format!(
                    "<h1>Stage not deleted</h1><p>{}</p>\
                     <p><a href=\"/admin/structure/stages\">Back to stages</a></p>",
                    super::helpers::html_escape(&e.to_string())
                )),
            )
                .into_response()
        }
    }
}

// =============================================================================
// Router
// =============================================================================

/// Stage administration routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(list_stages))
        .route(
            "/admin/structure/stages/add",
            get(add_stage_form).post(add_stage_submit),
        )
        .route(
            "/admin/structure/stages/{id}/edit",
            get(edit_stage_form).post(edit_stage_submit),
        )
        .route(
            "/admin/structure/stages/{id}/delete",
            get(delete_stage_form).post(delete_stage),
        )
}
