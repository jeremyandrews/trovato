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
//! In particular there is **no workflow membership field**, because there is no
//! such thing to edit. The tutorial ships `variable.workflow.editorial.yml`
//! describing stage transitions, and nothing in the kernel reads it: a
//! repository-wide search for `workflow.editorial` finds the file and no consumer.
//! A form field for a relationship the kernel does not model would be a field that
//! does nothing.
//!
//! ## The guard rails, and where they live
//!
//! Every rule is on the model ([`Stage::update`], [`Stage::delete`]) rather than in
//! these handlers, so `config import` and any future caller are held to the same
//! ones. The form's job is to say what happened in a sentence rather than a
//! constraint violation.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::form::csrf::generate_csrf_token;
use crate::models::stage::{LIVE_STAGE_ID, Stage, StageVisibility, UpdateStage};
use crate::models::{CreateStage, StageReferences};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, MACHINE_NAME_ERROR, is_valid_machine_name, render_admin_template,
    render_not_found, render_server_error, require_admin, require_csrf,
};

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

#[derive(Debug, Deserialize)]
struct StageFormData {
    #[serde(rename = "_token")]
    token: String,
    machine_name: String,
    label: String,
    description: Option<String>,
    visibility: String,
    /// Checkbox: absent when unchecked.
    is_default: Option<String>,
    weight: Option<i16>,
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

// =============================================================================
// Handlers
// =============================================================================

/// List stages.
///
/// GET /admin/structure/stages
async fn list_stages(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
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
        let references = match Stage::reference_counts(state.db(), stage.id).await {
            Ok(references) => references,
            Err(e) => {
                tracing::error!(error = %e, stage = %stage.machine_name, "failed to count stage references");
                return render_server_error("Failed to count what references a stage.");
            }
        };
        let is_live = stage.id == LIVE_STAGE_ID;
        rows.push(StageRow {
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
            // hiding, so the button stays and the confirmation says what holds it.
            deletable: !is_live && !stage.is_default && stage.visibility != StageVisibility::Public,
            references,
        });
    }

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("stages", &rows);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/structure/stages");

    render_admin_template(&state, "admin/stages.html", context).await
}

/// Render the add/edit form.
async fn render_form(
    state: &AppState,
    session: &Session,
    stage_id: Option<Uuid>,
    values: serde_json::Value,
    errors: Vec<String>,
) -> Response {
    let action = match stage_id {
        Some(id) => format!("/admin/structure/stages/{id}/edit"),
        None => "/admin/structure/stages/add".to_string(),
    };

    let csrf_token = generate_csrf_token(session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("action", &action);
    context.insert("editing", &stage_id.is_some());
    context.insert("is_live", &(stage_id == Some(LIVE_STAGE_ID)));
    context.insert("values", &values);
    context.insert("visibilities", &VISIBILITIES);
    context.insert("errors", &errors);
    context.insert("path", &action);

    render_admin_template(state, "admin/stage-form.html", context).await
}

/// Empty form values, so the template never reads an undefined variable.
fn blank_values() -> serde_json::Value {
    serde_json::json!({
        "machine_name": "",
        "label": "",
        "description": "",
        "visibility": "internal",
        "is_default": false,
        "weight": 0,
    })
}

fn submitted_values(form: &StageFormData) -> serde_json::Value {
    serde_json::json!({
        "machine_name": form.machine_name,
        "label": form.label,
        "description": form.description.clone().unwrap_or_default(),
        "visibility": form.visibility,
        "is_default": form.is_default.is_some(),
        "weight": form.weight.unwrap_or(0),
    })
}

/// Add-stage form.
///
/// GET /admin/structure/stages/add
async fn add_stage_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    render_form(&state, &session, None, blank_values(), Vec::new()).await
}

/// Validate the fields both submit paths share.
fn validate(form: &StageFormData) -> Vec<String> {
    let mut errors = Vec::new();

    if form.machine_name.trim().is_empty() {
        errors.push("Machine name is required.".to_string());
    } else if !is_valid_machine_name(form.machine_name.trim()) {
        errors.push(MACHINE_NAME_ERROR.to_string());
    }
    if form.label.trim().is_empty() {
        errors.push("Label is required.".to_string());
    }
    if form.visibility.parse::<StageVisibility>().is_err() {
        errors.push("Visibility must be internal, public or accessible.".to_string());
    }

    errors
}

/// Add-stage submit.
///
/// POST /admin/structure/stages/add
async fn add_stage_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<StageFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let mut errors = validate(&form);
    let machine_name = form.machine_name.trim().to_string();

    if errors.is_empty() {
        match Stage::find_by_machine_name(state.db(), &machine_name).await {
            Ok(Some(_)) => {
                errors.push(format!("A stage named '{machine_name}' already exists."));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to check for an existing stage");
                errors.push("Failed to check for an existing stage.".to_string());
            }
        }
    }

    // A second public stage cannot exist: the partial unique index on
    // `visibility = 'public'` says so, and the render layer resolves published
    // content through the one that does.
    if errors.is_empty() && form.visibility == "public" {
        errors.push(
            "Only one stage can be public, and it is the Live stage. Use internal or accessible."
                .to_string(),
        );
    }

    if !errors.is_empty() {
        return render_form(&state, &session, None, submitted_values(&form), errors).await;
    }

    let description = form
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string);

    let input = CreateStage {
        label: form.label.trim().to_string(),
        machine_name,
        description,
        visibility: Some(form.visibility.clone()),
        is_default: Some(form.is_default.is_some()),
        weight: Some(form.weight.unwrap_or(0)),
    };

    // A new default has to displace the old one, and only `Stage::update` knows
    // how to do that atomically, so create first and then set the flag.
    let wants_default = form.is_default.is_some();
    let created = match Stage::create(
        state.db(),
        CreateStage {
            is_default: Some(false),
            ..input
        },
    )
    .await
    {
        Ok(stage) => stage,
        Err(e) => {
            tracing::error!(error = %e, "failed to create stage");
            return render_form(
                &state,
                &session,
                None,
                submitted_values(&form),
                vec![format!("Failed to create the stage: {e}")],
            )
            .await;
        }
    };

    if wants_default
        && let Err(e) = Stage::update(
            state.db(),
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
            Some(created.id),
            submitted_values(&form),
            vec![format!(
                "The stage was created, but making it the default failed: {e}"
            )],
        )
        .await;
    }

    tracing::info!(stage = %created.machine_name, "stage created");
    Redirect::to("/admin/structure/stages").into_response()
}

/// Edit-stage form.
///
/// GET /admin/structure/stages/{id}/edit
async fn edit_stage_form(
    State(state): State<AppState>,
    session: Session,
    Path(stage_id): Path<Uuid>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let stage = match Stage::find_by_id(state.db(), stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load stage");
            return render_server_error("Failed to load the stage.");
        }
    };

    let values = serde_json::json!({
        "machine_name": stage.machine_name,
        "label": stage.label,
        "description": stage.description.unwrap_or_default(),
        "visibility": stage.visibility.as_str(),
        "is_default": stage.is_default,
        "weight": stage.weight,
    });

    render_form(&state, &session, Some(stage_id), values, Vec::new()).await
}

/// Edit-stage submit.
///
/// POST /admin/structure/stages/{id}/edit
async fn edit_stage_submit(
    State(state): State<AppState>,
    session: Session,
    Path(stage_id): Path<Uuid>,
    Form(form): Form<StageFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Ok(Some(existing)) = Stage::find_by_id(state.db(), stage_id).await else {
        return render_not_found();
    };

    let mut errors = validate(&form);
    let machine_name = form.machine_name.trim().to_string();

    if errors.is_empty() && machine_name != existing.machine_name {
        match Stage::find_by_machine_name(state.db(), &machine_name).await {
            Ok(Some(_)) => errors.push(format!("A stage named '{machine_name}' already exists.")),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to check for an existing stage");
                errors.push("Failed to check for an existing stage.".to_string());
            }
        }
    }

    // Promoting a second stage to public is refused for the same reason a second
    // public stage cannot be created.
    if errors.is_empty()
        && form.visibility == "public"
        && existing.visibility != StageVisibility::Public
    {
        errors.push(
            "Only one stage can be public, and the Live stage already is. Use internal or \
             accessible."
                .to_string(),
        );
    }

    if !errors.is_empty() {
        return render_form(
            &state,
            &session,
            Some(stage_id),
            submitted_values(&form),
            errors,
        )
        .await;
    }

    let description = form
        .description
        .as_deref()
        .map(str::trim)
        .map(str::to_string)
        .filter(|d| !d.is_empty());

    let input = UpdateStage {
        label: Some(form.label.trim().to_string()),
        machine_name: Some(machine_name),
        description: Some(description),
        visibility: Some(form.visibility.clone()),
        is_default: Some(form.is_default.is_some()),
        weight: Some(form.weight.unwrap_or(existing.weight)),
    };

    match Stage::update(state.db(), stage_id, input).await {
        Ok(Some(_)) => {
            tracing::info!(stage_id = %stage_id, "stage updated");
            Redirect::to("/admin/structure/stages").into_response()
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
                Some(stage_id),
                submitted_values(&form),
                vec![format!("{e}")],
            )
            .await
        }
    }
}

/// Delete a stage.
///
/// POST /admin/structure/stages/{id}/delete
async fn delete_stage(
    State(state): State<AppState>,
    session: Session,
    Path(stage_id): Path<Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match Stage::delete(state.db(), stage_id).await {
        Ok(true) => {
            tracing::info!(stage_id = %stage_id, "stage deleted");
            Redirect::to("/admin/structure/stages").into_response()
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
        .route("/admin/structure/stages", get(list_stages))
        .route("/admin/structure/stages/add", get(add_stage_form))
        .route("/admin/structure/stages/add", post(add_stage_submit))
        .route("/admin/structure/stages/{id}/edit", get(edit_stage_form))
        .route("/admin/structure/stages/{id}/edit", post(edit_stage_submit))
        .route("/admin/structure/stages/{id}/delete", post(delete_stage))
}
