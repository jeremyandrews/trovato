//! AI text assist API endpoint.
//!
//! `POST /api/v1/ai/assist` accepts a JSON body with text content and an
//! operation (rewrite, expand, shorten, translate, tone) and returns the
//! AI-transformed text. Used by the form AI Assist buttons injected by
//! the `trovato_ai` plugin's `tap_form_alter`.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::error::AppError;
use crate::routes::helpers::require_csrf_header;
use crate::services::ai_provider::{AiCaller, AiOperationType, ChatRequest};
use crate::state::AppState;

/// Supported AI assist operations.
const VALID_OPERATIONS: &[&str] = &["rewrite", "expand", "shorten", "translate", "tone"];

/// The writing assistant's instructions.
const WRITING_ASSISTANT_PROMPT: &str = "You are a writing assistant. Respond with only the \
    transformed text — no explanations, no markdown formatting, no quotes around the text.";

/// The most tokens one transformation may use.
const ASSIST_MAX_TOKENS: u32 = 1000;

/// What the usage log records the writing assistant's calls as.
const ASSIST_USAGE_SOURCE: &str = "trovato_ai";

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
pub fn api() -> super::api::ApiRoutes {
    super::api::ApiRoutes::new("ai").post(
        "/ai/assist",
        ai_assist_handler,
        super::api::doc("AI text transformation (rewrite, expand, shorten, translate, tone)"),
    )
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

    // Auth check: the signed-in, active user, or the JSON 401.
    let user = match crate::routes::helpers::require_login_json(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal.into_response(),
    };
    let uid = user.id;

    // Permission check. `user_has_permission` is the effective answer and
    // already returns true for a site administrator, so the hand-written
    // `if !user.is_admin` wrapper that used to sit here was a second copy of
    // the same bypass (BL-33). One bypass, in one place.
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

    let request = ChatRequest::system_and_user(
        WRITING_ASSISTANT_PROMPT,
        &build_operation_prompt(&body),
        ASSIST_MAX_TOKENS,
    );
    let caller = AiCaller::user(uid, ASSIST_USAGE_SOURCE);
    let completion = match ai_providers.chat_once(&resolved, &caller, &request).await {
        Ok(completion) => completion,
        Err(e) => {
            tracing::warn!(error = %e, operation = %body.operation, "AI assist failed");
            return AppError::service_unavailable("AI", e.user_message()).into_response();
        }
    };
    let (prompt_tokens, completion_tokens, total_tokens) = completion.usage;

    tracing::info!(
        operation = %body.operation,
        model = %completion.model,
        latency_ms = completion.latency_ms,
        tokens = total_tokens,
        "AI assist completed"
    );

    // Record usage (best-effort), with a cost estimate when the model is priced
    // (P11c / D-44).
    let cost_estimate = state
        .ai_budgets()
        .estimate_call_cost(
            &resolved.model,
            &completion.model,
            i64::from(prompt_tokens),
            i64::from(completion_tokens),
        )
        .await;
    let entry = crate::services::ai_token_budget::UsageLogEntry {
        user_id: Some(uid),
        plugin_name: ASSIST_USAGE_SOURCE.to_string(),
        provider_id: resolved.config.id.clone(),
        operation: "Chat".to_string(),
        model: completion.model.clone(),
        prompt_tokens: prompt_tokens.min(i32::MAX as u32) as i32,
        completion_tokens: completion_tokens.min(i32::MAX as u32) as i32,
        total_tokens: total_tokens.min(i32::MAX as u32) as i32,
        latency_ms: completion.latency_ms.min(i64::MAX as u64) as i64,
        cost_estimate,
    };
    if let Err(e) = state.ai_budgets().record_usage(state.db(), entry).await {
        tracing::warn!(error = %e, "failed to record AI assist usage");
    }

    Json(AiAssistResponse {
        result: completion.text.unwrap_or_default().trim().to_string(),
        tokens_used: total_tokens,
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

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::services::ai_provider::{AiProviderConfig, ProviderProtocol, ResolvedProvider};

    fn resolved(protocol: ProviderProtocol) -> ResolvedProvider {
        ResolvedProvider {
            config: AiProviderConfig {
                id: "p".to_string(),
                label: "P".to_string(),
                protocol,
                base_url: "https://provider.example/v1".to_string(),
                api_key_env: String::new(),
                models: Vec::new(),
                rate_limit_rpm: 0,
                enabled: true,
            },
            api_key: Some("sk-test".to_string()),
            model: "claude-test".to_string(),
        }
    }

    /// Nothing configures a temperature for the writing assistant, so none is
    /// sent: a hard coded 0.7 made every current Claude model answer 400.
    #[test]
    fn the_writing_assistant_sends_no_temperature() {
        for protocol in [
            ProviderProtocol::Anthropic,
            ProviderProtocol::OpenAiCompatible,
        ] {
            let request = ChatRequest::system_and_user("sys", "user", ASSIST_MAX_TOKENS);
            let (_, body, _) =
                crate::services::ai_tools::build_chat_request(&resolved(protocol), &request, false);
            assert!(body.get("temperature").is_none(), "{protocol:?}: {body}");
        }
    }
}
