//! Scolta AI search endpoints.
//!
//! `POST /api/v1/search/expand` — AI query expansion (returns alternative terms).
//! `POST /api/v1/search/summarize` — AI summary of search results (SSE stream).
//! `POST /api/v1/search/followup` — Follow-up conversation (SSE stream).
//!
//! All endpoints require the `trovato_ai` plugin to be enabled and an AI
//! provider configured for Chat operations.

use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::models::SiteConfig;
use crate::search::prompts;
use crate::services::ai_provider::{AiOperationType, ProviderProtocol};
use crate::state::AppState;

/// Build the Scolta search API router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/search/expand", post(expand_query))
        .route("/api/v1/search/summarize", post(summarize))
        .route("/api/v1/search/followup", post(followup))
}

// ============================================================================
// Request/response types
// ============================================================================

/// Request for query expansion.
#[derive(Debug, Deserialize)]
struct ExpandRequest {
    query: String,
}

/// Response for query expansion.
#[derive(Debug, Serialize)]
struct ExpandResponse {
    terms: Vec<String>,
}

/// Request for AI summary.
///
/// Accepts `excerpts` (structured) or `context` (plain text from scolta.js).
#[derive(Debug, Deserialize)]
struct SummarizeRequest {
    query: String,
    #[serde(default)]
    excerpts: Vec<Excerpt>,
    /// Plain text context from scolta.js (alternative to excerpts).
    #[serde(default)]
    context: Option<String>,
}

/// Request for follow-up conversation.
///
/// scolta.js sends `{ messages: [...] }` with conversation history.
#[derive(Debug, Deserialize)]
struct FollowupRequest {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    history: Vec<Message>,
    #[serde(default)]
    excerpts: Vec<Excerpt>,
    /// Conversation messages from scolta.js (alternative to query+history).
    #[serde(default)]
    messages: Vec<Message>,
}

/// A search result excerpt for AI context.
#[derive(Debug, Deserialize)]
struct Excerpt {
    title: String,
    url: String,
    text: String,
}

/// A conversation message.
#[derive(Debug, Deserialize)]
struct Message {
    role: String,
    content: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// Expand a search query into alternative terms via AI.
///
/// No CSRF required — this is a read-only search enhancement, not a
/// state-changing operation. scolta.js calls this from the client.
async fn expand_query(State(state): State<AppState>, Json(body): Json<ExpandRequest>) -> Response {
    if body.query.trim().is_empty() {
        return AppError::bad_request("Query cannot be empty").into_response();
    }

    // Resolve AI provider
    let ai_providers = state.ai_providers();
    let Ok(Some(resolved)) = ai_providers
        .resolve_provider(AiOperationType::Chat, None)
        .await
    else {
        return AppError::service_unavailable("AI", "AI provider not configured").into_response();
    };

    // Build the expand prompt
    let site_name = SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let site_slogan = SiteConfig::site_slogan(state.db())
        .await
        .unwrap_or_default();
    let system_prompt = prompts::resolve(prompts::EXPAND_QUERY, &site_name, &site_slogan);

    // Make the AI request
    let (url, request_body, auth_headers) =
        build_chat_request(&resolved, &system_prompt, &body.query);

    let response = match ai_providers
        .http()
        .post(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("content-type", "application/json")
        .body(request_body)
        .headers_from_vec(&auth_headers)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => {
            return AppError::service_unavailable("AI", "AI request failed").into_response();
        }
    };

    let response_text = response.text().await.unwrap_or_default();
    let (content, _tokens) = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => parse_openai_response(&response_text),
        ProviderProtocol::Anthropic => parse_anthropic_response(&response_text),
    };

    // Parse the expansion terms from the AI response
    let terms: Vec<String> = serde_json::from_str(&content)
        .or_else(|_| {
            // Try to extract JSON array from markdown-wrapped response
            let trimmed = content.trim();
            let json_str = if trimmed.contains("```") {
                trimmed
                    .split("```")
                    .nth(1)
                    .and_then(|s| s.strip_prefix("json"))
                    .unwrap_or(trimmed)
                    .trim()
            } else {
                trimmed
            };
            serde_json::from_str(json_str)
        })
        .unwrap_or_default();

    Json(ExpandResponse { terms }).into_response()
}

/// Summarize search results via AI with SSE streaming.
///
/// No CSRF required — read-only search enhancement.
async fn summarize(State(state): State<AppState>, Json(body): Json<SummarizeRequest>) -> Response {
    // Accept either structured excerpts or plain text context from scolta.js
    let context_text = if let Some(ref ctx) = body.context {
        ctx.clone()
    } else if !body.excerpts.is_empty() {
        build_excerpt_context(&body.excerpts)
    } else {
        return AppError::bad_request("Query and excerpts/context required").into_response();
    };

    if body.query.trim().is_empty() {
        return AppError::bad_request("Query cannot be empty").into_response();
    }

    let user_prompt = format!(
        "Search query: {}\n\nSearch result excerpts:\n{}",
        body.query, context_text
    );

    json_ai_response(&state, prompts::SUMMARIZE, &user_prompt).await
}

/// Handle follow-up conversation via AI with SSE streaming.
///
/// No CSRF required — read-only search enhancement.
async fn followup(State(state): State<AppState>, Json(body): Json<FollowupRequest>) -> Response {
    // Build conversation context from either messages (scolta.js) or query+history
    let user_prompt = if !body.messages.is_empty() {
        body.messages
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    } else if let Some(ref query) = body.query {
        let mut prompt = String::new();
        for msg in &body.history {
            prompt.push_str(&format!("{}: {}\n\n", msg.role, msg.content));
        }
        prompt.push_str(&format!("User: {query}\n"));
        if !body.excerpts.is_empty() {
            let context = build_excerpt_context(&body.excerpts);
            prompt.push_str(&format!(
                "\nAdditional search results for this follow-up:\n{context}",
            ));
        }
        prompt
    } else {
        return AppError::bad_request("Query or messages required").into_response();
    };

    json_ai_response_with_key(&state, prompts::FOLLOW_UP, &user_prompt, "response").await
}

// ============================================================================
// Helpers
// ============================================================================

/// Build excerpt context string from a list of excerpts.
fn build_excerpt_context(excerpts: &[Excerpt]) -> String {
    excerpts
        .iter()
        .enumerate()
        .map(|(i, e)| format!("Result {} - {} ({})\n{}\n", i + 1, e.title, e.url, e.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Return an AI response as JSON with `{ summary: "..." }`.
async fn json_ai_response(state: &AppState, prompt_template: &str, user_prompt: &str) -> Response {
    json_ai_response_with_key(state, prompt_template, user_prompt, "summary").await
}

/// Return an AI response as JSON with a custom key.
async fn json_ai_response_with_key(
    state: &AppState,
    prompt_template: &str,
    user_prompt: &str,
    key: &str,
) -> Response {
    let ai_providers = state.ai_providers();
    let Ok(Some(resolved)) = ai_providers
        .resolve_provider(AiOperationType::Chat, None)
        .await
    else {
        return AppError::service_unavailable("AI", "AI provider not configured").into_response();
    };

    let site_name = SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let site_slogan = SiteConfig::site_slogan(state.db())
        .await
        .unwrap_or_default();
    let system_prompt = prompts::resolve(prompt_template, &site_name, &site_slogan);

    let (url, request_body, auth_headers) =
        build_chat_request(&resolved, &system_prompt, user_prompt);

    let response = match ai_providers
        .http()
        .post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .header("content-type", "application/json")
        .body(request_body)
        .headers_from_vec(&auth_headers)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => {
            return AppError::service_unavailable("AI", "AI request failed").into_response();
        }
    };

    let response_text = response.text().await.unwrap_or_default();
    let (content, _tokens) = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => parse_openai_response(&response_text),
        ProviderProtocol::Anthropic => parse_anthropic_response(&response_text),
    };

    let mut result = serde_json::Map::new();
    result.insert(key.to_string(), serde_json::Value::String(content));
    Json(serde_json::Value::Object(result)).into_response()
}

/// Stream an AI response as SSE events (kept for future use).
#[allow(dead_code)]
async fn stream_ai_response(
    state: &AppState,
    prompt_template: &str,
    user_prompt: &str,
) -> Response {
    let ai_providers = state.ai_providers();
    let Ok(Some(resolved)) = ai_providers
        .resolve_provider(AiOperationType::Chat, None)
        .await
    else {
        return AppError::service_unavailable("AI", "AI provider not configured").into_response();
    };

    let site_name = SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let site_slogan = SiteConfig::site_slogan(state.db())
        .await
        .unwrap_or_default();
    let system_prompt = prompts::resolve(prompt_template, &site_name, &site_slogan);

    // Non-streaming request for simplicity (scolta.js handles the display)
    let (url, request_body, auth_headers) =
        build_chat_request(&resolved, &system_prompt, user_prompt);

    let response = match ai_providers
        .http()
        .post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .header("content-type", "application/json")
        .body(request_body)
        .headers_from_vec(&auth_headers)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => {
            return AppError::service_unavailable("AI", "AI request failed").into_response();
        }
    };

    let response_text = response.text().await.unwrap_or_default();
    let (content, _tokens) = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => parse_openai_response(&response_text),
        ProviderProtocol::Anthropic => parse_anthropic_response(&response_text),
    };

    // Return as SSE event (single chunk for non-streaming providers)
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(
            Event::default()
                .event("message")
                .data(serde_json::json!({"summary": content}).to_string()),
        );
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Build a non-streaming chat request for the given provider.
fn build_chat_request(
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
                "max_tokens": 500,
                "temperature": 0.3
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
                "max_tokens": 500,
                "temperature": 0.3
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
    let input = json["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output = json["usage"]["output_tokens"].as_u64().unwrap_or(0);
    (content, (input + output) as u32)
}

/// Extension trait to add headers from a Vec to a reqwest RequestBuilder.
trait RequestBuilderExt {
    fn headers_from_vec(self, headers: &[(String, String)]) -> Self;
}

impl RequestBuilderExt for reqwest::RequestBuilder {
    fn headers_from_vec(mut self, headers: &[(String, String)]) -> Self {
        for (key, value) in headers {
            self = self.header(key.as_str(), value.as_str());
        }
        self
    }
}
