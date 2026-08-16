//! Admin routes for AI provider management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::services::ai_provider::{
    AiDefaults, AiOperationType, AiProviderConfig, AiProviderService, OperationModel,
    ProviderProtocol, validate_base_url, validate_env_var_name,
};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_csrf,
    require_permission, require_permission_json,
};

/// Session key for flash messages on the AI providers list page.
const FLASH_KEY: &str = "ai_provider_flash";

// =============================================================================
// Form data
// =============================================================================

/// Provider add/edit form data.
#[derive(Debug, Deserialize)]
struct ProviderFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    label: String,
    protocol: String,
    base_url: String,
    api_key_env: String,
    rate_limit_rpm: Option<String>,
    enabled: Option<String>,
    /// Repeated operation fields: `op_{index}` = operation type key.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

/// Defaults form data.
#[derive(Debug, Deserialize)]
struct DefaultsFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    /// `default_{operation}` = provider_id or empty.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

// =============================================================================
// Helpers
// =============================================================================

/// Parse the operation+model pairs from the flat form fields.
///
/// The form sends pairs like `op_0=chat`, `model_0=gpt-4o`, `op_1=embedding`, etc.
fn parse_operation_models(
    extra: &std::collections::HashMap<String, String>,
) -> Vec<OperationModel> {
    let mut models = Vec::new();
    for i in 0..20 {
        let op_key = format!("op_{i}");
        let model_key = format!("model_{i}");
        let Some(op_str) = extra.get(&op_key) else {
            continue;
        };
        let Some(model_str) = extra.get(&model_key) else {
            continue;
        };
        if op_str.is_empty() || model_str.trim().is_empty() {
            continue;
        }
        if let Ok(op) =
            serde_json::from_value::<AiOperationType>(serde_json::Value::String(op_str.clone()))
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

/// Build template-friendly provider data with masked key info.
///
/// Values are not pre-escaped here because Tera autoescapes all `{{ }}`
/// interpolations. Pre-escaping would cause double encoding.
fn provider_view(config: &AiProviderConfig) -> serde_json::Value {
    serde_json::json!({
        "id": config.id,
        "label": config.label,
        "protocol": config.protocol.to_string(),
        "base_url": config.base_url,
        "api_key_status": AiProviderService::mask_key_ref(config),
        "key_is_set": AiProviderService::key_is_set(config),
        "models": config.models.iter().map(|m| serde_json::json!({
            "operation": m.operation.to_string(),
            "model": m.model,
        })).collect::<Vec<_>>(),
        "rate_limit_rpm": config.rate_limit_rpm,
        "enabled": config.enabled,
    })
}

// =============================================================================
// Handlers
// =============================================================================

/// List all AI providers and show defaults form.
///
/// GET /admin/system/ai-providers
async fn list_providers(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }

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

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    // Read and clear flash message
    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear AI provider flash message");
    }

    let provider_views: Vec<serde_json::Value> = providers.iter().map(provider_view).collect();

    // Build operation types with their current default
    let operation_defaults: Vec<serde_json::Value> = AiOperationType::ALL
        .iter()
        .map(|op| {
            let key = serde_json::to_value(op)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            serde_json::json!({
                "key": key,
                "label": op.to_string(),
                "default_provider": defaults.defaults.get(op).cloned().unwrap_or_default(),
            })
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert("providers", &provider_views);
    context.insert("operation_defaults", &operation_defaults);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("flash", &flash);
    context.insert("path", "/admin/system/ai-providers");

    render_admin_template(&state, "admin/ai-providers.html", context).await
}

/// Show add provider form.
///
/// GET /admin/system/ai-providers/add
async fn add_provider_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let operations: Vec<serde_json::Value> = AiOperationType::ALL
        .iter()
        .map(|op| {
            let key = serde_json::to_value(op)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            serde_json::json!({ "key": key, "label": op.to_string() })
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/system/ai-providers/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("operations", &operations);
    context.insert("path", "/admin/system/ai-providers/add");

    render_admin_template(&state, "admin/ai-provider-form.html", context).await
}

/// Handle add provider form submission.
///
/// POST /admin/system/ai-providers/add
async fn add_provider_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ProviderFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Label is required.".to_string());
    }

    let protocol = match parse_protocol(&form.protocol) {
        Some(p) => p,
        None => {
            errors.push("Protocol is required.".to_string());
            ProviderProtocol::OpenAiCompatible
        }
    };

    let base_url = form.base_url.trim();
    if base_url.is_empty() {
        errors.push("Base URL is required.".to_string());
    } else if let Err(msg) = validate_base_url(base_url) {
        errors.push(msg);
    }

    let api_key_env = form.api_key_env.trim();
    if let Err(msg) = validate_env_var_name(api_key_env) {
        errors.push(msg);
    }

    let models = parse_operation_models(&form.extra);

    if !errors.is_empty() {
        return redisplay_form(
            &state,
            &session,
            "/admin/system/ai-providers/add",
            false,
            &errors,
            &form,
            &models,
        )
        .await;
    }

    let rate_limit_rpm = form
        .rate_limit_rpm
        .as_deref()
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);

    let config = AiProviderConfig {
        id: uuid::Uuid::new_v4().to_string(),
        label: form.label.trim().to_string(),
        protocol,
        base_url: base_url.to_string(),
        api_key_env: api_key_env.to_string(),
        models,
        rate_limit_rpm,
        enabled: form.enabled.is_some(),
    };

    match state.ai_providers().save_provider(config).await {
        Ok(()) => {
            let msg = format!("Provider \"{}\" has been created.", form.label.trim());
            let _ = session.insert(FLASH_KEY, &msg).await;
            tracing::info!(label = %form.label.trim(), "AI provider created");
            Redirect::to("/admin/system/ai-providers").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create AI provider");
            render_server_error("Failed to create AI provider.")
        }
    }
}

/// Show edit provider form.
///
/// GET /admin/system/ai-providers/{id}/edit
async fn edit_provider_form(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }

    let config = match state.ai_providers().get_provider(&provider_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load AI provider");
            return render_server_error("Failed to load AI provider.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let operations: Vec<serde_json::Value> = AiOperationType::ALL
        .iter()
        .map(|op| {
            let key = serde_json::to_value(op)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            serde_json::json!({ "key": key, "label": op.to_string() })
        })
        .collect();

    let protocol_key = match config.protocol {
        ProviderProtocol::OpenAiCompatible => "open_ai_compatible",
        ProviderProtocol::Anthropic => "anthropic",
    };

    let model_values: Vec<serde_json::Value> = config
        .models
        .iter()
        .map(|m| {
            let op_key = serde_json::to_value(m.operation)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            serde_json::json!({ "operation": op_key, "model": m.model })
        })
        .collect();

    let values = serde_json::json!({
        "label": config.label,
        "protocol": protocol_key,
        "base_url": config.base_url,
        "api_key_env": config.api_key_env,
        "rate_limit_rpm": config.rate_limit_rpm,
        "enabled": config.enabled,
        "models": model_values,
    });

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/system/ai-providers/{provider_id}/edit"),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("provider_id", &provider_id);
    context.insert("values", &values);
    context.insert("operations", &operations);
    context.insert(
        "path",
        &format!("/admin/system/ai-providers/{provider_id}/edit"),
    );

    render_admin_template(&state, "admin/ai-provider-form.html", context).await
}

/// Handle edit provider form submission.
///
/// POST /admin/system/ai-providers/{id}/edit
async fn edit_provider_submit(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
    Form(form): Form<ProviderFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Verify provider exists
    match state.ai_providers().get_provider(&provider_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load AI provider");
            return render_server_error("Failed to load AI provider.");
        }
    }

    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Label is required.".to_string());
    }

    let protocol = match parse_protocol(&form.protocol) {
        Some(p) => p,
        None => {
            errors.push("Protocol is required.".to_string());
            ProviderProtocol::OpenAiCompatible
        }
    };

    let base_url = form.base_url.trim();
    if base_url.is_empty() {
        errors.push("Base URL is required.".to_string());
    } else if let Err(msg) = validate_base_url(base_url) {
        errors.push(msg);
    }

    let api_key_env = form.api_key_env.trim();
    if let Err(msg) = validate_env_var_name(api_key_env) {
        errors.push(msg);
    }

    let models = parse_operation_models(&form.extra);

    if !errors.is_empty() {
        return redisplay_form(
            &state,
            &session,
            &format!("/admin/system/ai-providers/{provider_id}/edit"),
            true,
            &errors,
            &form,
            &models,
        )
        .await;
    }

    let rate_limit_rpm = form
        .rate_limit_rpm
        .as_deref()
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);

    let config = AiProviderConfig {
        id: provider_id.clone(),
        label: form.label.trim().to_string(),
        protocol,
        base_url: base_url.to_string(),
        api_key_env: api_key_env.to_string(),
        models,
        rate_limit_rpm,
        enabled: form.enabled.is_some(),
    };

    match state.ai_providers().save_provider(config).await {
        Ok(()) => {
            let msg = format!("Provider \"{}\" has been updated.", form.label.trim());
            let _ = session.insert(FLASH_KEY, &msg).await;
            tracing::info!(id = %provider_id, "AI provider updated");
            Redirect::to("/admin/system/ai-providers").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update AI provider");
            render_server_error("Failed to update AI provider.")
        }
    }
}

/// Delete a provider.
///
/// POST /admin/system/ai-providers/{id}/delete
async fn delete_provider(
    State(state): State<AppState>,
    session: Session,
    Path(provider_id): Path<String>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state.ai_providers().delete_provider(&provider_id).await {
        Ok(true) => {
            let _ = session
                .insert(FLASH_KEY, "Provider has been deleted.")
                .await;
            tracing::info!(id = %provider_id, "AI provider deleted");
            Redirect::to("/admin/system/ai-providers").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete AI provider");
            render_server_error("Failed to delete AI provider.")
        }
    }
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
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err((status, json)) = require_permission_json(&state, &session, "configure ai").await {
        return (status, json).into_response();
    }
    if require_csrf(&session, &form.token).await.is_err() {
        return Json(serde_json::json!({
            "success": false,
            "message": "CSRF validation failed.",
            "latency_ms": 0
        }))
        .into_response();
    }

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
    Json(result).into_response()
}

/// Save default provider assignments.
///
/// POST /admin/system/ai-providers/defaults
async fn save_defaults(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<DefaultsFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let mut defaults = AiDefaults::default();
    for op in AiOperationType::ALL {
        let key = serde_json::to_value(op)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        let form_key = format!("default_{key}");
        if let Some(provider_id) = form.extra.get(&form_key)
            && !provider_id.is_empty()
        {
            defaults.defaults.insert(*op, provider_id.clone());
        }
    }

    match state.ai_providers().save_defaults(defaults).await {
        Ok(()) => {
            let _ = session
                .insert(FLASH_KEY, "Default providers have been updated.")
                .await;
            tracing::info!("AI provider defaults updated");
            Redirect::to("/admin/system/ai-providers").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to save AI defaults");
            render_server_error("Failed to save defaults.")
        }
    }
}

// =============================================================================
// Shared: redisplay form with errors
// =============================================================================

/// Re-render the provider form with validation errors and submitted values.
async fn redisplay_form(
    state: &AppState,
    session: &Session,
    action: &str,
    editing: bool,
    errors: &[String],
    form: &ProviderFormData,
    models: &[OperationModel],
) -> Response {
    let csrf_token = generate_csrf_token(session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let operations: Vec<serde_json::Value> = AiOperationType::ALL
        .iter()
        .map(|op| {
            let key = serde_json::to_value(op)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            serde_json::json!({ "key": key, "label": op.to_string() })
        })
        .collect();

    let model_values: Vec<serde_json::Value> = models
        .iter()
        .map(|m| {
            let op_key = serde_json::to_value(m.operation)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            serde_json::json!({ "operation": op_key, "model": m.model })
        })
        .collect();

    let values = serde_json::json!({
        "label": form.label,
        "protocol": form.protocol,
        "base_url": form.base_url,
        "api_key_env": form.api_key_env,
        "rate_limit_rpm": form.rate_limit_rpm.as_deref().unwrap_or("0"),
        "enabled": form.enabled.is_some(),
        "models": model_values,
    });

    let mut context = tera::Context::new();
    context.insert("action", action);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &editing);
    context.insert("errors", errors);
    context.insert("values", &values);
    context.insert("operations", &operations);
    context.insert("path", action);

    render_admin_template(state, "admin/ai-provider-form.html", context).await
}

// =============================================================================
// Router
// =============================================================================

/// Build the AI provider admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/system/ai-providers", get(list_providers))
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
            post(delete_provider),
        )
        .route("/admin/system/ai-providers/{id}/test", post(test_provider))
        .route("/admin/system/ai-providers/defaults", post(save_defaults))
}
