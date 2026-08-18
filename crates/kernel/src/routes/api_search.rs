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
///
/// Expansions are cached. `docs/design/search-architecture.md` specified a cache
/// and none was built, so every call — including the same query typed twice —
/// spent provider tokens.
async fn expand_query(State(state): State<AppState>, Json(body): Json<ExpandRequest>) -> Response {
    if body.query.trim().is_empty() {
        return AppError::bad_request("Query cannot be empty").into_response();
    }

    // Everything the prompt is built from, so a site rename or a provider change
    // cannot serve an expansion produced under the old one.
    let site_name = SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let site_slogan = SiteConfig::site_slogan(state.db())
        .await
        .unwrap_or_default();

    let ttl = state.runtime().search_expand_cache_ttl;
    let cache_key = expansion_cache_key(&body.query, &site_name, &site_slogan);

    if ttl > 0
        && let Some(cached) = state.cache().get(&cache_key).await
        && let Ok(terms) = serde_json::from_str::<Vec<String>>(&cached)
    {
        tracing::debug!(query = %body.query, "search expansion served from cache");
        return Json(ExpandResponse { terms }).into_response();
    }

    // Resolve AI provider
    let ai_providers = state.ai_providers();
    let Ok(Some(resolved)) = ai_providers
        .resolve_provider(AiOperationType::Chat, None)
        .await
    else {
        return AppError::service_unavailable("AI", "AI provider not configured").into_response();
    };

    // Build the expand prompt from the same values the cache key covers.
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

    // An empty list is a parse failure, not an answer worth remembering for a
    // month: cache only a real expansion.
    if ttl > 0 && !terms.is_empty() {
        match serde_json::to_string(&terms) {
            Ok(payload) => {
                state
                    .cache()
                    .set(&cache_key, &payload, ttl, &[EXPANSION_CACHE_TAG])
                    .await;
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize expansion for cache"),
        }
    }

    Json(ExpandResponse { terms }).into_response()
}

/// Cache tag for every stored query expansion, so the set can be dropped at once
/// when the prompt changes.
pub const EXPANSION_CACHE_TAG: &str = "search_expand";

/// Cache key for one query's expansion.
///
/// The query is normalized — trimmed, lowercased, internal whitespace collapsed —
/// so "  Rust   Async " and "rust async" are one entry rather than two. The site
/// name and slogan are part of the key because the prompt is built from them: a
/// renamed site must not be served expansions produced under its old name.
///
/// Hashed rather than interpolated: a query is arbitrary user text, and a cache
/// key is a Redis key.
///
/// Public because the key shape is the cache's contract: it is what an operator
/// inspecting or evicting entries needs, and what a test seeds.
pub fn expansion_cache_key(query: &str, site_name: &str, site_slogan: &str) -> String {
    use sha2::{Digest, Sha256};

    let normalized = normalize_query(query);
    let mut hasher = Sha256::new();
    // Length-prefixed so no two different triples can hash the same bytes.
    for part in [normalized.as_str(), site_name, site_slogan] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }

    format!("{EXPANSION_CACHE_TAG}:{:x}", hasher.finalize())
}

/// Reduce a query to the form the cache keys on.
pub fn normalize_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn normalization_folds_case_and_whitespace() {
        assert_eq!(normalize_query("  Rust   Async "), "rust async");
        assert_eq!(normalize_query("rust async"), "rust async");
        assert_eq!(normalize_query("RUST\tASYNC\n"), "rust async");
    }

    /// The point of normalizing: the same question asked two ways is one cache
    /// entry, so the second asker does not pay for another provider call.
    #[test]
    fn queries_that_differ_only_in_spacing_and_case_share_a_key() {
        let a = expansion_cache_key("  Rust   Async ", "Site", "Slogan");
        let b = expansion_cache_key("rust async", "Site", "Slogan");

        assert_eq!(a, b);
    }

    #[test]
    fn different_queries_get_different_keys() {
        let a = expansion_cache_key("rust async", "Site", "Slogan");
        let b = expansion_cache_key("rust threads", "Site", "Slogan");

        assert_ne!(a, b);
    }

    /// The prompt is built from the site name and slogan, so they belong in the
    /// key: a renamed site must not be served expansions produced under its old
    /// name.
    #[test]
    fn the_prompt_inputs_are_part_of_the_key() {
        let base = expansion_cache_key("rust async", "Site", "Slogan");

        assert_ne!(base, expansion_cache_key("rust async", "Other", "Slogan"));
        assert_ne!(base, expansion_cache_key("rust async", "Site", "Other"));
    }

    /// Length-prefixing the parts stops one triple from colliding with another
    /// that merely concatenates the same way.
    #[test]
    fn adjacent_parts_cannot_run_together_into_the_same_key() {
        assert_ne!(
            expansion_cache_key("ab", "c", "d"),
            expansion_cache_key("a", "bc", "d")
        );
        assert_ne!(
            expansion_cache_key("a", "b", "cd"),
            expansion_cache_key("a", "bc", "d")
        );
    }

    /// A key goes into Redis, so it must be a key: fixed length, tagged prefix,
    /// no user text.
    #[test]
    fn a_key_is_a_hash_and_not_the_query() {
        let key = expansion_cache_key("a query with spaces and 'quotes'", "Site", "");

        assert!(key.starts_with("search_expand:"));
        assert!(!key.contains(' '));
        assert!(!key.contains("query"));
        assert_eq!(key.len(), "search_expand:".len() + 64);
    }
}
