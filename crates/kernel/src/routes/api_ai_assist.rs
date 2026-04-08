//! AI text assist API endpoint.
//!
//! `POST /api/v1/ai/assist` accepts a JSON body with text content and an
//! operation (rewrite, expand, shorten, translate, tone) and returns the
//! AI-transformed text. Used by the form AI Assist buttons injected by
//! the `trovato_ai` plugin's `tap_form_alter`.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::error::AppError;
use crate::routes::auth::SESSION_USER_ID;
use crate::routes::helpers::require_csrf_header;
use crate::services::ai_provider::{AiOperationType, ProviderProtocol};
use crate::state::AppState;

/// Supported AI assist operations.
const VALID_OPERATIONS: &[&str] = &["rewrite", "expand", "shorten", "translate", "tone"];

/// Request body for AI assist.
#[derive(Debug, Deserialize)]
pub struct AiAssistRequest {
    /// The text content to transform.
    pub text: String,
    /// Operation: rewrite, expand, shorten, translate, tone.
    pub operation: String,
    /// Target language for translate (e.g., "es", "fr", "de").
    #[serde(default)]
    pub language: Option<String>,
    /// Target tone for tone adjustment (e.g., "formal", "casual", "technical").
    #[serde(default)]
    pub tone: Option<String>,
}

/// Response body for AI assist.
#[derive(Debug, Serialize)]
pub struct AiAssistResponse {
    /// The AI-transformed text.
    pub result: String,
    /// Tokens used for this operation.
    pub tokens_used: u32,
}

/// Build the AI assist router.
pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/ai/assist", post(ai_assist_handler))
}

/// Handle AI text assist requests.
///
/// Makes a non-streaming chat completion request to the configured
/// provider and returns the full response synchronously.
async fn ai_assist_handler(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<AiAssistRequest>,
) -> Response {
    // CSRF check
    if require_csrf_header(&session, &headers).await.is_err() {
        return AppError::forbidden("Invalid or missing CSRF token").into_response();
    }

    // Auth check: load user from session
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let Some(uid) = user_id else {
        return AppError::unauthorized("Authentication required").into_response();
    };

    let user = match state.users().find_by_id(uid).await {
        Ok(Some(u)) if u.is_active() => u,
        _ => {
            return AppError::unauthorized("User not found or inactive").into_response();
        }
    };

    // Permission check
    if !user.is_admin {
        let has_base = state
            .permissions()
            .user_has_permission(&user, "use ai")
            .await
            .unwrap_or(false);
        let has_chat = state
            .permissions()
            .user_has_permission(&user, "use ai chat")
            .await
            .unwrap_or(false);

        if !has_base || !has_chat {
            return AppError::forbidden("Permission required: use ai chat").into_response();
        }
    }

    // Validate input
    if body.text.is_empty() {
        return AppError::bad_request("Text cannot be empty").into_response();
    }

    if body.text.len() > 10_000 {
        return AppError::bad_request("Text too long (max 10,000 characters)").into_response();
    }

    if !VALID_OPERATIONS.contains(&body.operation.as_str()) {
        return AppError::bad_request(format!(
            "Invalid operation '{}'. Valid: {}",
            body.operation,
            VALID_OPERATIONS.join(", ")
        ))
        .into_response();
    }

    // Resolve AI provider
    let ai_providers = state.ai_providers();

    let resolved = match ai_providers
        .resolve_provider(AiOperationType::Chat, None)
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => {
            return AppError::service_unavailable("AI", "No AI provider configured for chat")
                .into_response();
        }
        Err(e) => {
            return AppError::internal_ctx(e, "resolve AI provider").into_response();
        }
    };

    // Build and send the request
    let start = Instant::now();
    let user_prompt = build_operation_prompt(&body);

    let (url, request_body, auth_headers) = build_provider_request(
        &resolved,
        "You are a writing assistant. Respond with only the transformed text \
         — no explanations, no markdown formatting, no quotes around the text.",
        &user_prompt,
    );

    let mut req = ai_providers
        .http()
        .post(&url)
        .timeout(std::time::Duration::from_secs(60))
        .header("content-type", "application/json")
        .body(request_body);

    for (key, value) in &auth_headers {
        req = req.header(key.as_str(), value.as_str());
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "AI assist HTTP request failed");
            return AppError::service_unavailable("AI", "AI request failed").into_response();
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        tracing::error!(
            http_status = %status,
            body = %body_text.chars().take(200).collect::<String>(),
            "AI provider error"
        );
        return AppError::service_unavailable("AI", "AI provider returned an error")
            .into_response();
    }

    let latency_ms = start.elapsed().as_millis() as i64;
    let response_text = match response.text().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "failed to read AI response body");
            return AppError::service_unavailable("AI", "Failed to read AI response")
                .into_response();
        }
    };

    // Parse the provider response
    let (content, tokens_used) = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => parse_openai_response(&response_text),
        ProviderProtocol::Anthropic => parse_anthropic_response(&response_text),
    };

    tracing::info!(
        operation = %body.operation,
        model = %resolved.model,
        latency_ms,
        tokens = tokens_used,
        "AI assist completed"
    );

    // Record usage (best-effort)
    let entry = crate::services::ai_token_budget::UsageLogEntry {
        user_id: Some(uid),
        plugin_name: "trovato_ai".to_string(),
        provider_id: resolved.config.id.clone(),
        operation: "Chat".to_string(),
        model: resolved.model.clone(),
        prompt_tokens: 0,
        completion_tokens: tokens_used as i32,
        total_tokens: tokens_used as i32,
        latency_ms,
    };
    let _ = state.ai_budgets().record_usage(state.db(), entry).await;

    Json(AiAssistResponse {
        result: content.trim().to_string(),
        tokens_used,
    })
    .into_response()
}

/// Build the user-facing prompt for the requested operation.
fn build_operation_prompt(body: &AiAssistRequest) -> String {
    match body.operation.as_str() {
        "rewrite" => format!(
            "Rewrite the following text to improve clarity and flow while \
             preserving the meaning:\n\n{}",
            body.text
        ),
        "expand" => format!(
            "Expand the following text with more detail and supporting \
             information while maintaining the same tone:\n\n{}",
            body.text
        ),
        "shorten" => format!(
            "Shorten the following text to roughly half its length while \
             preserving the key points:\n\n{}",
            body.text
        ),
        "translate" => {
            let lang = body.language.as_deref().unwrap_or("English");
            format!("Translate the following text to {lang}:\n\n{}", body.text)
        }
        "tone" => {
            let tone = body.tone.as_deref().unwrap_or("professional");
            format!(
                "Rewrite the following text in a {tone} tone:\n\n{}",
                body.text
            )
        }
        _ => body.text.clone(),
    }
}

/// Build a non-streaming chat request for the given provider.
fn build_provider_request(
    resolved: &crate::services::ai_provider::ResolvedProvider,
    system_prompt: &str,
    user_prompt: &str,
) -> (String, String, Vec<(String, String)>) {
    let mut headers = Vec::new();

    match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => {
            let url = format!("{}/chat/completions", resolved.config.base_url);
            if let Some(ref key) = resolved.api_key {
                headers.push(("Authorization".to_string(), format!("Bearer {key}")));
            }
            let body = serde_json::json!({
                "model": resolved.model,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt}
                ],
                "max_tokens": 1000,
                "temperature": 0.7
            });
            (url, body.to_string(), headers)
        }
        ProviderProtocol::Anthropic => {
            let url = format!("{}/messages", resolved.config.base_url);
            if let Some(ref key) = resolved.api_key {
                headers.push(("x-api-key".to_string(), key.clone()));
            }
            headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            let body = serde_json::json!({
                "model": resolved.model,
                "system": system_prompt,
                "messages": [
                    {"role": "user", "content": user_prompt}
                ],
                "max_tokens": 1000,
                "temperature": 0.7
            });
            (url, body.to_string(), headers)
        }
    }
}

/// Parse an OpenAI-compatible chat completion response.
fn parse_openai_response(body: &str) -> (String, u32) {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return (String::new(), 0),
    };

    let content = json["choices"]
        .get(0)
        .and_then(|c| c["message"]["content"].as_str())
        .unwrap_or("")
        .to_string();

    let tokens = json["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32;

    (content, tokens)
}

/// Parse an Anthropic Messages API response.
fn parse_anthropic_response(body: &str) -> (String, u32) {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return (String::new(), 0),
    };

    let content = json["content"]
        .get(0)
        .and_then(|c| c["text"].as_str())
        .unwrap_or("")
        .to_string();

    let input_tokens = json["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = json["usage"]["output_tokens"].as_u64().unwrap_or(0);

    (content, (input_tokens + output_tokens) as u32)
}
