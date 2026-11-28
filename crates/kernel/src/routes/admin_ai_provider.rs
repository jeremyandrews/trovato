//! Admin routes for AI provider management.
//!
//! Every form here goes through `FormService` (B83c): the add and edit form,
//! the default providers form, the delete confirmation, and the one form the
//! list's Test buttons belong to.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::models::User;
use crate::services::ai_provider::{
    AiDefaults, AiOperationType, AiProviderConfig, AiProviderService, KeyRefDisplay,
    OperationModel, ProviderProtocol, validate_base_url, validate_env_var_name,
};
use crate::state::AppState;

use super::admin_form;
use super::admin_nav::AdminText;
use super::helpers::{
    render_admin_template, render_error, render_not_found, render_server_error, require_permission,
    require_permission_json, user_context_for,
};

/// Session key for flash messages on the AI providers list page.
const FLASH_KEY: &str = "ai_provider_flash";

/// The list, where every form here returns.
const BASE: &str = "/admin/system/ai-providers";

/// The add and edit form.
const PROVIDER_FORM_ID: &str = "ai_provider_form";

/// The default provider for each operation, on the list.
const DEFAULTS_FORM_ID: &str = "ai_provider_defaults_form";

/// The form every row's Test button belongs to (`form=`), each posting to its
/// own provider's test address (`formaction`).
const TEST_FORM_ID: &str = "ai_provider_test_form";

/// The delete confirmation.
const DELETE_FORM_ID: &str = "ai_provider_delete_form";

// =============================================================================
// Helpers
// =============================================================================

/// Parse the operation+model pairs from the flat form fields.
///
/// The form sends pairs like `op_0=chat`, `model_0=gpt-4o`, `op_1=embedding`, etc.
fn parse_operation_models(posted: &PostedForm) -> Vec<OperationModel> {
    let mut models = Vec::new();
    for i in 0..20 {
        let Some(op_str) = posted.get(&format!("op_{i}")) else {
            continue;
        };
        let Some(model_str) = posted.get(&format!("model_{i}")) else {
            continue;
        };
        if op_str.is_empty() || model_str.trim().is_empty() {
            continue;
        }
        if let Ok(op) =
            serde_json::from_value::<AiOperationType>(serde_json::Value::String(op_str.into()))
        {
            models.push(OperationModel {
                operation: op,
                model: model_str.trim().to_string(),
            });
        }
    }
    models
}

/// Parse a protocol string from the form select.
fn parse_protocol(s: &str) -> Option<ProviderProtocol> {
    match s {
        "open_ai_compatible" => Some(ProviderProtocol::OpenAiCompatible),
        "anthropic" => Some(ProviderProtocol::Anthropic),
        _ => None,
    }
}

/// The key an operation type is posted and stored under (`chat`).
fn operation_key(op: &AiOperationType) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Every operation type as its key and English label, for the model rows and
/// the script that adds one.
fn operations() -> Vec<serde_json::Value> {
    AiOperationType::ALL
        .iter()
        .map(|op| serde_json::json!({ "key": operation_key(op), "label": op.to_string() }))
        .collect()
}

/// Build template-friendly provider data with masked key info.
///
/// Values are not pre-escaped here because Tera autoescapes all `{{ }}`
/// interpolations. Pre-escaping would cause double encoding.
///
/// `key_ref_state` and `api_key_env` carry [`KeyRefDisplay`] apart so the
/// template can translate the sentence with `t()` (B253): the env var name
/// itself is data, never translated, only the words around it.
fn provider_view(config: &AiProviderConfig) -> serde_json::Value {
    let (key_ref_state, api_key_env) = match AiProviderService::mask_key_ref(config) {
        KeyRefDisplay::NotConfigured => ("not_configured", String::new()),
        KeyRefDisplay::Set(env) => ("set", env),
        KeyRefDisplay::NotSet(env) => ("not_set", env),
    };
    serde_json::json!({
        "id": config.id,
        "label": config.label,
        "protocol": config.protocol.to_string(),
        "base_url": config.base_url,
        "key_ref_state": key_ref_state,
        "api_key_env": api_key_env,
        "models": config.models.iter().map(|m| serde_json::json!({
            "operation": m.operation.to_string(),
            "model": m.model,
        })).collect::<Vec<_>>(),
        "rate_limit_rpm": config.rate_limit_rpm,
        "enabled": config.enabled,
    })
}

/// A Cancel link back to the list, in the page's language, for beside a
/// submit button.
fn cancel_link(text: &AdminText) -> String {
    format!(
        r#"<a href="{BASE}" class="button button--secondary">{}</a>"#,
        text.html("Cancel", &[])
    )
}

// =============================================================================
// Forms
// =============================================================================

/// What the provider form shows: a stored provider, or what was posted.
#[derive(Debug, Default)]
struct ProviderValues {
    label: String,
    protocol: String,
    base_url: String,
    api_key_env: String,
    rate_limit_rpm: String,
    enabled: bool,
    models: Vec<OperationModel>,
}

impl ProviderValues {
    /// The values of a stored provider.
    fn of(config: &AiProviderConfig) -> Self {
        Self {
            label: config.label.clone(),
            protocol: match config.protocol {
                ProviderProtocol::OpenAiCompatible => "open_ai_compatible",
                ProviderProtocol::Anthropic => "anthropic",
            }
            .to_string(),
            base_url: config.base_url.clone(),
            api_key_env: config.api_key_env.clone(),
            rate_limit_rpm: config.rate_limit_rpm.to_string(),
            enabled: config.enabled,
            models: config.models.clone(),
        }
    }

    /// The values a submission posted.
    fn posted(posted: &PostedForm) -> Self {
        let text = |name: &str| posted.get(name).unwrap_or_default().to_string();
        Self {
            label: text("label"),
            protocol: text("protocol"),
            base_url: text("base_url"),
            api_key_env: text("api_key_env"),
            rate_limit_rpm: text("rate_limit_rpm"),
            enabled: posted.contains("enabled"),
            models: parse_operation_models(posted),
        }
    }
}

/// The operation model rows (`admin/ai-provider-models.html`) in the page's
/// language: the part of the provider form `static/js/ai-admin.js` adds rows
/// to. `None` (logged) when the template fails to render.
fn model_rows_html(
    state: &AppState,
    text: &AdminText,
    models: &[OperationModel],
) -> Option<String> {
    let rows: Vec<serde_json::Value> = models
        .iter()
        .map(|m| serde_json::json!({ "operation": operation_key(&m.operation), "model": m.model }))
        .collect();
    let mut context = tera::Context::new();
    context.insert("operations", &operations());
    context.insert("models", &rows);
    text.scope(|| {
        state
            .theme()
            .tera()
            .render("admin/ai-provider-models.html", &context)
    })
    .map_err(|e| tracing::error!(error = %e, "failed to render the AI provider model rows"))
    .ok()
}

/// The add and edit form.
fn provider_form(
    action: &str,
    text: &AdminText,
    values: &ProviderValues,
    model_rows: String,
) -> crate::form::Form {
    crate::form::Form::new(PROVIDER_FORM_ID)
        .action(action)
        .element(
            "label",
            FormElement::textfield()
                .title("Label")
                .description(
                    r#"A human-readable name for this provider (e.g. "OpenAI Production")."#,
                )
                .default_value(values.label.clone())
                .required()
                .weight(0),
        )
        .element(
            "protocol",
            FormElement::select(vec![
                (
                    "open_ai_compatible".to_string(),
                    "OpenAI Compatible".to_string(),
                ),
                ("anthropic".to_string(), "Anthropic".to_string()),
            ])
            .title("Protocol")
            .description("OpenAI Compatible covers OpenAI, Azure, Ollama, and vLLM endpoints.")
            .default_value(values.protocol.clone())
            .required()
            .weight(1),
        )
        .element(
            "base_url",
            FormElement::input("url")
                .title("Base URL")
                .description(
                    "The API base URL without trailing slash, for example \
                     https://api.openai.com/v1.",
                )
                .default_value(values.base_url.clone())
                .required()
                .attribute("autocomplete", "off")
                .attribute("spellcheck", "false")
                .weight(2),
        )
        .element(
            "api_key_env",
            FormElement::textfield()
                .title("API Key Environment Variable")
                .description(
                    "Name of the environment variable containing the API key, for example \
                     OPENAI_API_KEY. The key itself is never stored in the database.",
                )
                .default_value(values.api_key_env.clone())
                .attribute("autocomplete", "off")
                .attribute("spellcheck", "false")
                .attribute("autocapitalize", "characters")
                .weight(3),
        )
        .element(
            "rate_limit_rpm",
            FormElement::input("number")
                .title("Rate Limit (requests per minute)")
                .description("0 = no rate limiting.")
                .default_value(if values.rate_limit_rpm.is_empty() {
                    "0".to_string()
                } else {
                    values.rate_limit_rpm.clone()
                })
                .attribute("min", "0")
                .weight(4),
        )
        .element(
            "enabled",
            FormElement::checkbox()
                .title("Enabled")
                .default_value(values.enabled)
                .weight(5),
        )
        .element("models", FormElement::markup(model_rows).weight(6))
        .element(
            "op",
            FormElement::submit("Save provider")
                .suffix(cancel_link(text))
                .weight(10),
        )
}

/// The default provider for each operation: one select per operation type,
/// offering the enabled providers.
fn defaults_form(
    text: &AdminText,
    providers: &[AiProviderConfig],
    defaults: &AiDefaults,
) -> crate::form::Form {
    let options: Vec<(String, String)> = providers
        .iter()
        .filter(|p| p.enabled)
        .map(|p| (p.id.clone(), p.label.clone()))
        .collect();
    let mut fieldset = FormElement::fieldset()
        .title("Default provider for each operation")
        .weight(0);
    for (weight, op) in (0..).zip(AiOperationType::ALL.iter()) {
        fieldset = fieldset.child(
            format!("default_{}", operation_key(op)),
            FormElement::select(options.clone())
                .title(op.to_string())
                .placeholder("— None —")
                .default_value(defaults.defaults.get(op).cloned().unwrap_or_default())
                .weight(weight),
        );
    }
    crate::form::Form::new(DEFAULTS_FORM_ID)
        .action(format!("{BASE}/defaults"))
        .element("defaults", fieldset)
        .element(
            "op",
            FormElement::submit("Save defaults")
                .suffix(cancel_link(text))
                .weight(10),
        )
}

/// The delete confirmation for the provider `id`.
fn delete_form_for(id: &str) -> crate::form::Form {
    crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{id}/delete"))
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .weight(10),
        )
}

/// What the provider form posted, checked: the provider to save, or the
/// problems with it.
fn read_provider(
    posted: &PostedForm,
) -> Result<(String, ProviderProtocol, String, String, u32, bool), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let label = posted.get("label").unwrap_or_default().trim().to_string();
    if label.is_empty() {
        errors.push(ValidationError::field("label", "Label is required."));
    }
    let protocol = parse_protocol(posted.get("protocol").unwrap_or_default());
    if protocol.is_none() {
        errors.push(ValidationError::field("protocol", "Protocol is required."));
    }
    let base_url = posted
        .get("base_url")
        .unwrap_or_default()
        .trim()
        .to_string();
    if base_url.is_empty() {
        errors.push(ValidationError::field("base_url", "Base URL is required."));
    } else if let Err(msg) = validate_base_url(&base_url) {
        errors.push(ValidationError::field("base_url", msg));
    }
    let api_key_env = posted
        .get("api_key_env")
        .unwrap_or_default()
        .trim()
        .to_string();
    if let Err(msg) = validate_env_var_name(&api_key_env) {
        errors.push(ValidationError::field("api_key_env", msg));
    }
    let rate_limit_rpm = posted
        .get("rate_limit_rpm")
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);
    match protocol {
        Some(protocol) if errors.is_empty() => Ok((
            label,
            protocol,
            base_url,
            api_key_env,
            rate_limit_rpm,
            posted.contains("enabled"),
        )),
        _ => Err(errors),
    }
}

// =============================================================================
// List, defaults and test
// =============================================================================

/// List all AI providers and show defaults form.
///
/// GET /admin/system/ai-providers
async fn list_providers(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let providers = match state.ai_providers().list_providers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to list AI providers");
            return render_server_error("Failed to load AI providers.");
        }
    };

    let defaults = state
        .ai_providers()
        .get_defaults()
        .await
        .unwrap_or_default();

    let viewer = user_context_for(&state, &user).await;
    let text = AdminText::for_page(&state, &viewer, BASE).await;
    let (test_form, defaults_form) = if providers.is_empty() {
        (String::new(), String::new())
    } else {
        let test = crate::form::Form::new(TEST_FORM_ID).action(BASE);
        let test = match admin_form::render(&state, &session, &user, test, &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        };
        let form = defaults_form(&text, &providers, &defaults);
        let defaults = match admin_form::render(&state, &session, &user, form, &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        };
        (test, defaults)
    };

    // Read and clear flash message
    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear AI provider flash message");
    }

    let provider_views: Vec<serde_json::Value> = providers.iter().map(provider_view).collect();

    let mut context = tera::Context::new();
    context.insert("providers", &provider_views);
    context.insert("test_form", &test_form);
    context.insert("test_form_id", TEST_FORM_ID);
    context.insert("defaults_form", &defaults_form);
    context.insert("flash", &flash);
    context.insert("path", BASE);

    render_admin_template(&state, &viewer, "admin/ai-providers.html", context).await
}

/// Test a provider connection.
///
/// POST /admin/system/ai-providers/{id}/test
///
/// Returns JSON in all cases (success, auth failure, CSRF failure).
async fn test_provider(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission_json(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(refusal) => return refusal.into_response(),
    };
    let Ok(form_state) =
        admin_form::check(&state, &session, &user, &posted, TEST_FORM_ID, Vec::new()).await
    else {
        return Json(serde_json::json!({
            "success": false,
            "message": "CSRF validation failed.",
            "latency_ms": 0
        }))
        .into_response();
    };

    let config = match state.ai_providers().get_provider(&provider_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Json(serde_json::json!({
                "success": false,
                "message": "Provider not found.",
                "latency_ms": 0
            }))
            .into_response();
        }
        Err(e) => {
            return Json(serde_json::json!({
                "success": false,
                "message": format!("Failed to load provider: {e}"),
                "latency_ms": 0
            }))
            .into_response();
        }
    };

    let result = state.ai_providers().test_connection(&config).await;
    admin_form::submitted(&state, &user, &form_state, &posted).await;
    Json(result).into_response()
}

/// Save default provider assignments.
///
/// POST /admin/system/ai-providers/defaults
async fn save_defaults(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        DEFAULTS_FORM_ID,
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

    let mut defaults = AiDefaults::default();
    for op in AiOperationType::ALL {
        if let Some(provider_id) = posted.get(&format!("default_{}", operation_key(op)))
            && !provider_id.is_empty()
        {
            defaults.defaults.insert(*op, provider_id.to_string());
        }
    }

    match state.ai_providers().save_defaults(defaults).await {
        Ok(()) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            let _ = session
                .insert(FLASH_KEY, "Default providers have been updated.")
                .await;
            tracing::info!("AI provider defaults updated");
            Redirect::to(BASE).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to save AI defaults");
            render_server_error("Failed to save defaults.")
        }
    }
}

// =============================================================================
// Add and edit
// =============================================================================

/// Render the provider form at `action`, with `values` and `errors`.
async fn render_provider_form(
    state: &AppState,
    session: &Session,
    user: &User,
    action: &str,
    editing: bool,
    values: &ProviderValues,
    errors: &[ValidationError],
) -> Response {
    let viewer = user_context_for(state, user).await;
    let text = AdminText::for_page(state, &viewer, action).await;
    let Some(model_rows) = model_rows_html(state, &text, &values.models) else {
        return render_server_error("The form could not be displayed.");
    };
    let form = provider_form(action, &text, values, model_rows);
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("form", &form_html);
    context.insert("editing", &editing);
    context.insert("operations", &operations());
    context.insert("path", action);

    render_admin_template(state, &viewer, "admin/ai-provider-form.html", context).await
}

/// Show add provider form.
///
/// GET /admin/system/ai-providers/add
async fn add_provider_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_provider_form(
        &state,
        &session,
        &user,
        &format!("{BASE}/add"),
        false,
        &ProviderValues::default(),
        &[],
    )
    .await
}

/// Handle add provider form submission.
///
/// POST /admin/system/ai-providers/add
async fn add_provider_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    save_provider(state, session, PostedForm(pairs), None).await
}

/// Show edit provider form.
///
/// GET /admin/system/ai-providers/{id}/edit
async fn edit_provider_form(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let config = match state.ai_providers().get_provider(&provider_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load AI provider");
            return render_server_error("Failed to load AI provider.");
        }
    };

    render_provider_form(
        &state,
        &session,
        &user,
        &format!("{BASE}/{provider_id}/edit"),
        true,
        &ProviderValues::of(&config),
        &[],
    )
    .await
}

/// Handle edit provider form submission.
///
/// POST /admin/system/ai-providers/{id}/edit
async fn edit_provider_submit(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    save_provider(state, session, PostedForm(pairs), Some(provider_id)).await
}

/// Check and save a submission of the provider form: a new provider when
/// `editing` is `None`, else the provider of that id.
async fn save_provider(
    state: AppState,
    session: Session,
    posted: PostedForm,
    editing: Option<String>,
) -> Response {
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let action = match &editing {
        Some(id) => format!("{BASE}/{id}/edit"),
        None => format!("{BASE}/add"),
    };

    if let Some(id) = &editing {
        match state.ai_providers().get_provider(id).await {
            Ok(Some(_)) => {}
            Ok(None) => return render_not_found(),
            Err(e) => {
                tracing::error!(error = %e, "failed to load AI provider");
                return render_server_error("Failed to load AI provider.");
            }
        }
    }

    let parsed = read_provider(&posted);
    let errors = parsed.as_ref().err().cloned().unwrap_or_default();
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, PROVIDER_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_provider_form(
                    &state,
                    &session,
                    &user,
                    &action,
                    editing.is_some(),
                    &ProviderValues::posted(&posted),
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };
    let Ok((label, protocol, base_url, api_key_env, rate_limit_rpm, enabled)) = parsed else {
        return render_not_found();
    };

    let config = AiProviderConfig {
        id: editing
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        label: label.clone(),
        protocol,
        base_url,
        api_key_env,
        models: parse_operation_models(&posted),
        rate_limit_rpm,
        enabled,
    };

    match state.ai_providers().save_provider(config).await {
        Ok(()) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            let msg = match &editing {
                Some(id) => {
                    tracing::info!(id = %id, "AI provider updated");
                    format!("Provider \"{label}\" has been updated.")
                }
                None => {
                    tracing::info!(label = %label, "AI provider created");
                    format!("Provider \"{label}\" has been created.")
                }
            };
            let _ = session.insert(FLASH_KEY, &msg).await;
            Redirect::to(BASE).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to save AI provider");
            render_server_error(if editing.is_some() {
                "Failed to update AI provider."
            } else {
                "Failed to create AI provider."
            })
        }
    }
}

// =============================================================================
// Delete
// =============================================================================

/// Confirm deleting a provider.
///
/// GET /admin/system/ai-providers/{id}/delete
async fn delete_provider_form(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let config = match state.ai_providers().get_provider(&provider_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load AI provider");
            return render_server_error("Failed to load AI provider.");
        }
    };
    let path = format!("{BASE}/{provider_id}/delete");
    let viewer = user_context_for(&state, &user).await;
    let form = delete_form_for(&provider_id);
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("provider", &provider_view(&config));
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(&state, &viewer, "admin/ai-provider-delete.html", context).await
}

/// Delete a provider.
///
/// POST /admin/system/ai-providers/{id}/delete
async fn delete_provider(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, DELETE_FORM_ID, Vec::new()).await
        {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return render_error("The form has expired. Reload the page and try again.");
            }
            Err(Err(response)) => return response,
        };

    match state.ai_providers().delete_provider(&provider_id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            let _ = session
                .insert(FLASH_KEY, "Provider has been deleted.")
                .await;
            tracing::info!(id = %provider_id, "AI provider deleted");
            Redirect::to(BASE).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete AI provider");
            render_server_error("Failed to delete AI provider.")
        }
    }
}

// =============================================================================
// Router
// =============================================================================

/// Build the AI provider admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(list_providers))
        .route(
            "/admin/system/ai-providers/add",
            get(add_provider_form).post(add_provider_submit),
        )
        .route(
            "/admin/system/ai-providers/{id}/edit",
            get(edit_provider_form).post(edit_provider_submit),
        )
        .route(
            "/admin/system/ai-providers/{id}/delete",
            get(delete_provider_form).post(delete_provider),
        )
        .route("/admin/system/ai-providers/{id}/test", post(test_provider))
        .route("/admin/system/ai-providers/defaults", post(save_defaults))
}
