//! Access rules admin (B108): `/admin/people/rules`.
//!
//! List, add, edit and delete the rules `services::access_rule` applies at
//! registration and sign in, and test a value against them. Every route takes
//! `administer access rules`; every form goes through `FormService`.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::services::access_rule::{self, AccessRule, RuleStatus, RuleType};
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    render_admin_template, render_not_found, render_server_error, require_permission,
};

/// The permission every route here takes.
pub const PERMISSION: &str = "administer access rules";

const BASE: &str = "/admin/people/rules";
const RULE_FORM_ID: &str = "access_rule_form";
const CHECK_FORM_ID: &str = "access_rule_check_form";
const DELETE_FORM_ID: &str = "access_rule_delete_form";
const FLASH_KEY: &str = "access_rules_flash";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(list_rules))
        .route(&format!("{BASE}/check"), axum::routing::post(check_submit))
        .route(&format!("{BASE}/add"), get(add_form).post(add_submit))
        .route(
            &format!("{BASE}/{{id}}/edit"),
            get(edit_form).post(edit_submit),
        )
        .route(
            &format!("{BASE}/{{id}}/delete"),
            get(delete_form).post(delete_submit),
        )
}

// =============================================================================
// Forms
// =============================================================================

fn type_options() -> Vec<(String, String)> {
    RuleType::ALL
        .iter()
        .map(|t| (t.as_str().to_string(), t.label().to_string()))
        .collect()
}

/// The add and edit form.
fn rule_form(action: &str, rule_type: &str, pattern: &str, status: &str) -> crate::form::Form {
    crate::form::Form::new(RULE_FORM_ID)
        .action(action)
        .element(
            "status",
            FormElement::radio(vec![
                ("deny".to_string(), "Deny".to_string()),
                ("allow".to_string(), "Allow".to_string()),
            ])
            .title("Access type")
            .description("An allow rule overrides any deny rule of the same type.")
            .default_value(if status.is_empty() { "deny" } else { status })
            .required()
            .weight(0),
        )
        .element(
            "rule_type",
            FormElement::radio(type_options())
                .title("Rule type")
                .description(
                    "Host is the client address as the server sees it, after any trusted proxy.",
                )
                .default_value(if rule_type.is_empty() {
                    "username"
                } else {
                    rule_type
                })
                .required()
                .weight(1),
        )
        .element(
            "pattern",
            FormElement::textfield()
                .title("Mask")
                .description(
                    "Matched against the whole value, ignoring case. % matches any run of \
                     characters: spam% blocks every username starting with spam, \
                     %@example.com every address at example.com.",
                )
                .default_value(pattern.to_string())
                .max_length(255)
                .required()
                .weight(2),
        )
        .element("op", FormElement::submit("Save rule").weight(10))
}

/// The "check a value" form on the list.
fn check_form(rule_type: &str, value: &str) -> crate::form::Form {
    crate::form::Form::new(CHECK_FORM_ID)
        .action(format!("{BASE}/check"))
        .element(
            "rule_type",
            FormElement::select(type_options())
                .title("Type")
                .default_value(if rule_type.is_empty() {
                    "username"
                } else {
                    rule_type
                })
                .weight(0),
        )
        .element(
            "value",
            FormElement::textfield()
                .title("Value")
                .description("A username, an email address or a client address to test.")
                .default_value(value.to_string())
                .max_length(255)
                .required()
                .weight(1),
        )
        .element("op", FormElement::submit("Check").weight(10))
}

fn delete_form_for(id: Uuid) -> crate::form::Form {
    crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{id}/delete"))
        .element("op", FormElement::submit("Delete").weight(10))
}

/// What the rule form posted, checked.
fn read_rule(
    posted: &PostedForm,
) -> (
    Result<(RuleType, String, RuleStatus), Vec<ValidationError>>,
    [String; 3],
) {
    let rule_type = posted.get("rule_type").unwrap_or_default().to_string();
    let pattern = posted.get("pattern").unwrap_or_default().trim().to_string();
    let status = posted.get("status").unwrap_or_default().to_string();
    let mut errors = Vec::new();
    let parsed_type = RuleType::parse(&rule_type);
    if parsed_type.is_none() {
        errors.push(ValidationError::field("rule_type", "Choose a rule type."));
    }
    let parsed_status = RuleStatus::parse(&status);
    if parsed_status.is_none() {
        errors.push(ValidationError::field("status", "Choose allow or deny."));
    }
    if pattern.is_empty() {
        errors.push(ValidationError::field("pattern", "Mask is required."));
    } else if pattern.chars().count() > 255 {
        errors.push(ValidationError::field(
            "pattern",
            "Mask must be at most 255 characters.",
        ));
    }
    let values = [rule_type, pattern.clone(), status];
    match (parsed_type, parsed_status) {
        (Some(t), Some(s)) if errors.is_empty() => (Ok((t, pattern, s)), values),
        _ => (Err(errors), values),
    }
}

// =============================================================================
// List and check
// =============================================================================

#[derive(Debug, Serialize)]
struct RuleRow {
    id: Uuid,
    rule_type: &'static str,
    pattern: String,
    status: &'static str,
    created: i64,
}

impl From<AccessRule> for RuleRow {
    fn from(rule: AccessRule) -> Self {
        Self {
            id: rule.id,
            rule_type: rule.rule_type.label(),
            pattern: rule.pattern,
            status: rule.status.as_str(),
            created: rule.created,
        }
    }
}

/// The result of a check, as the list shows it.
#[derive(Debug, Serialize)]
struct CheckResult {
    rule_type: &'static str,
    value: String,
    blocked: bool,
}

async fn render_list(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    check: Option<CheckResult>,
    check_values: (&str, &str),
    check_errors: &[ValidationError],
) -> Response {
    let rules = match access_rule::list(state.db()).await {
        Ok(rules) => rules,
        Err(e) => {
            tracing::error!(error = %e, "failed to list access rules");
            return render_server_error("The access rules could not be loaded.");
        }
    };
    let rows: Vec<RuleRow> = rules.into_iter().map(RuleRow::from).collect();
    let form_html = match admin_form::render(
        state,
        session,
        user,
        check_form(check_values.0, check_values.1),
        check_errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    let viewer = super::helpers::user_context_for(state, user).await;

    let mut context = tera::Context::new();
    context.insert("rules", &rows);
    context.insert("check_form", &form_html);
    context.insert("check", &check);
    context.insert("flash", &flash);
    context.insert("path", BASE);
    render_admin_template(state, &viewer, "admin/access-rules.html", context).await
}

/// GET /admin/people/rules
async fn list_rules(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_list(&state, &session, &user, None, ("", ""), &[]).await
}

/// POST /admin/people/rules/check: test a value against the rules.
async fn check_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let rule_type_raw = posted.get("rule_type").unwrap_or_default().to_string();
    let value = posted.get("value").unwrap_or_default().trim().to_string();
    let mut errors = Vec::new();
    let rule_type = RuleType::parse(&rule_type_raw);
    if rule_type.is_none() {
        errors.push(ValidationError::field("rule_type", "Choose a type."));
    }
    if value.is_empty() {
        errors.push(ValidationError::field("value", "Enter a value to check."));
    }
    match admin_form::check(&state, &session, &user, &posted, CHECK_FORM_ID, errors).await {
        Ok(()) => {}
        Err(Ok(errors)) => {
            return render_list(
                &state,
                &session,
                &user,
                None,
                (&rule_type_raw, &value),
                &errors,
            )
            .await;
        }
        Err(Err(response)) => return response,
    }
    let Some(rule_type) = rule_type else {
        return render_not_found();
    };
    let blocked = access_rule::is_blocked(state.db(), rule_type, &value).await;
    let result = CheckResult {
        rule_type: rule_type.label(),
        value: value.clone(),
        blocked,
    };
    render_list(
        &state,
        &session,
        &user,
        Some(result),
        (&rule_type_raw, &value),
        &[],
    )
    .await
}

// =============================================================================
// Add and edit
// =============================================================================

async fn render_rule_form(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    editing: Option<Uuid>,
    values: [&str; 3],
    errors: &[ValidationError],
) -> Response {
    let action = match editing {
        Some(id) => format!("{BASE}/{id}/edit"),
        None => format!("{BASE}/add"),
    };
    let form_html = match admin_form::render(
        state,
        session,
        user,
        rule_form(&action, values[0], values[1], values[2]),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert(
        "title",
        if editing.is_some() {
            "Edit access rule"
        } else {
            "Add access rule"
        },
    );
    context.insert("form", &form_html);
    context.insert("editing", &editing);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/access-rule-form.html", context).await
}

/// GET /admin/people/rules/add
async fn add_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_rule_form(&state, &session, &user, None, ["", "", ""], &[]).await
}

/// POST /admin/people/rules/add
async fn add_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    save(state, session, PostedForm(pairs), None).await
}

/// GET /admin/people/rules/{id}/edit
async fn edit_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let rule = match access_rule::find(state.db(), id).await {
        Ok(Some(rule)) => rule,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load access rule");
            return render_server_error("The access rule could not be loaded.");
        }
    };
    render_rule_form(
        &state,
        &session,
        &user,
        Some(id),
        [rule.rule_type.as_str(), &rule.pattern, rule.status.as_str()],
        &[],
    )
    .await
}

/// POST /admin/people/rules/{id}/edit
async fn edit_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    save(state, session, PostedForm(pairs), Some(id)).await
}

async fn save(state: AppState, session: Session, posted: PostedForm, id: Option<Uuid>) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (parsed, values) = read_rule(&posted);
    let errors = parsed.as_ref().err().cloned().unwrap_or_default();
    match admin_form::check(&state, &session, &user, &posted, RULE_FORM_ID, errors).await {
        Ok(()) => {}
        Err(Ok(errors)) => {
            return render_rule_form(
                &state,
                &session,
                &user,
                id,
                [&values[0], &values[1], &values[2]],
                &errors,
            )
            .await;
        }
        Err(Err(response)) => return response,
    }
    let Ok((rule_type, pattern, status)) = parsed else {
        return render_not_found();
    };
    let saved = match id {
        Some(id) => access_rule::update(state.db(), id, rule_type, &pattern, status)
            .await
            .map(|found| found.then_some(id)),
        None => access_rule::create(state.db(), rule_type, &pattern, status)
            .await
            .map(Some),
    };
    match saved {
        Ok(Some(_)) => {
            tracing::info!(
                log_type = "config",
                user_id = %user.id,
                rule_type = rule_type.as_str(),
                status = status.as_str(),
                pattern = %pattern,
                "access rule saved"
            );
            let _ = session
                .insert(
                    FLASH_KEY,
                    format!(
                        "The {} rule for {} {pattern} was saved.",
                        status.as_str(),
                        rule_type.as_str()
                    ),
                )
                .await;
            Redirect::to(BASE).into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to save access rule");
            render_server_error("The access rule could not be saved.")
        }
    }
}

// =============================================================================
// Delete
// =============================================================================

/// GET /admin/people/rules/{id}/delete: confirm.
async fn delete_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let rule = match access_rule::find(state.db(), id).await {
        Ok(Some(rule)) => rule,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load access rule");
            return render_server_error("The access rule could not be loaded.");
        }
    };
    let form_html =
        match admin_form::render(&state, &session, &user, delete_form_for(id), &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("rule", &RuleRow::from(rule));
    context.insert("form", &form_html);
    context.insert("path", &format!("{BASE}/{id}/delete"));
    render_admin_template(&state, &viewer, "admin/access-rule-delete.html", context).await
}

/// POST /admin/people/rules/{id}/delete
async fn delete_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
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
    match access_rule::delete(state.db(), id).await {
        Ok(true) => {
            tracing::info!(log_type = "config", user_id = %user.id, rule_id = %id, "access rule deleted");
            let _ = session
                .insert(FLASH_KEY, "The access rule was deleted.")
                .await;
            Redirect::to(BASE).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete access rule");
            render_server_error("The access rule could not be deleted.")
        }
    }
}
