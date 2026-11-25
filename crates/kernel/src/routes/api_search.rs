//! Scolta AI search endpoints.
//!
//! `POST /api/v1/search/expand` — AI query expansion (returns alternative terms).
//! `POST /api/v1/search/summarize` — AI summary of search results (SSE stream).
//! `POST /api/v1/search/followup` — Follow-up conversation (SSE stream).
//!
//! All endpoints ask for the `use ai` permission, and require the `trovato_ai`
//! plugin to be enabled and an AI provider configured for Chat operations.
//!
//! Each one first reads its stage switch from the search settings
//! ([`SearchSettings`]) and answers 404 when the stage is off, before any cache
//! or provider work, so switching a stage off stops the spending and not just
//! the stock page's use of it. The follow-up endpoint also enforces the turn cap
//! there.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::error::AppError;
use crate::models::SiteConfig;
use crate::search::analytics::Counter;
use crate::search::{SearchSettings, normalize_query, prompts};
use crate::services::ai_provider::{
    AiCallError, AiCaller, AiOperationType, AiProviderService, ChatRequest, ResolvedProvider,
};
use crate::state::AppState;

/// Build the Scolta search API router.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("search")
        .post(
            "/search/expand",
            expand_query,
            doc("AI query expansion — returns alternative search terms"),
        )
        .post(
            "/search/summarize",
            summarize,
            doc("AI summary of search results (SSE stream)").stream(),
        )
        .post(
            "/search/followup",
            followup,
            doc("Follow-up conversation about search results (SSE stream)").stream(),
        )
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
async fn expand_query(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<ExpandRequest>,
) -> Response {
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    if !settings.expand {
        return stage_off("Query expansion");
    }
    let caller = match ai_caller(&state, &session).await {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };

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
        state
            .search()
            .analytics()
            .count(&settings, Counter::ExpansionHit);
        return Json(ExpandResponse { terms }).into_response();
    }
    state
        .search()
        .analytics()
        .count(&settings, Counter::ExpansionMiss);

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
    let content = match complete(
        ai_providers,
        &resolved,
        &caller,
        &system_prompt,
        &body.query,
    )
    .await
    {
        Ok(content) => content,
        Err(refusal) => return refusal,
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

/// Summarize search results via AI with SSE streaming.
///
/// No CSRF required — read-only search enhancement.
async fn summarize(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<SummarizeRequest>,
) -> Response {
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    if !settings.summarize {
        return stage_off("The search summary");
    }
    let caller = match ai_caller(&state, &session).await {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };

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

    json_ai_response(&state, &caller, prompts::SUMMARIZE, &user_prompt).await
}

/// Handle follow-up conversation via AI with SSE streaming.
///
/// No CSRF required — read-only search enhancement.
async fn followup(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<FollowupRequest>,
) -> Response {
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    if !settings.followup {
        return stage_off("Follow-up");
    }
    let caller = match ai_caller(&state, &session).await {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    // The opening question plus `max_followups` follow-ups.
    if user_turns(&body) > settings.max_followups as usize + 1 {
        return AppError::bad_request(format!(
            "A conversation may hold at most {} follow-up questions",
            settings.max_followups
        ))
        .into_response();
    }

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

    let response = json_ai_response_with_key(
        &state,
        &caller,
        prompts::FOLLOW_UP,
        &user_prompt,
        "response",
    )
    .await;

    // Classify the follow-up off the request path: queued, never awaited.
    if response.status().is_success()
        && settings.sentiment_enabled
        && settings.analytics_enabled
        && let Some((question, previous_answer)) = last_exchange(&body)
    {
        state.sentiment().submit(question, previous_answer);
    }

    response
}

// ============================================================================
// Helpers
// ============================================================================

/// The answer for a stage the search settings switched off: 404, with a short
/// JSON error, before any cache or provider work.
fn stage_off(stage: &str) -> Response {
    crate::error::AppError::with_status(
        StatusCode::NOT_FOUND,
        format!("{stage} is switched off on this site"),
    )
    .into_response()
}

/// User turns in a follow-up request, counted on whichever shape it came in:
/// `messages` as sent, or `history` plus the new `query`.
fn user_turns(body: &FollowupRequest) -> usize {
    let is_user = |m: &&Message| m.role.eq_ignore_ascii_case("user");
    if body.messages.is_empty() {
        body.history.iter().filter(is_user).count() + usize::from(body.query.is_some())
    } else {
        body.messages.iter().filter(is_user).count()
    }
}

/// The visitor's newest message and the assistant answer before it, from
/// whichever shape the request came in. The search context `scolta.js` appends
/// to a follow-up is cut off: it is search results, not the visitor's words.
fn last_exchange(body: &FollowupRequest) -> Option<(&str, &str)> {
    let (question, earlier): (&str, &[Message]) = if body.messages.is_empty() {
        (body.query.as_deref()?, &body.history)
    } else {
        let (last, earlier) = body.messages.split_last()?;
        if !last.role.eq_ignore_ascii_case("user") {
            return None;
        }
        (&last.content, earlier)
    };
    let previous = earlier
        .iter()
        .rev()
        .find(|m| m.role.eq_ignore_ascii_case("assistant"))?;
    let question = question
        .split_once("\n\nAdditional search results for this follow-up:")
        .map_or(question, |(own, _)| own);
    Some((question, previous.content.as_str()))
}

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
async fn json_ai_response(
    state: &AppState,
    caller: &AiCaller,
    prompt_template: &str,
    user_prompt: &str,
) -> Response {
    json_ai_response_with_key(state, caller, prompt_template, user_prompt, "summary").await
}

/// Return an AI response as JSON with a custom key.
async fn json_ai_response_with_key(
    state: &AppState,
    caller: &AiCaller,
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

    let content = match complete(ai_providers, &resolved, caller, &system_prompt, user_prompt).await
    {
        Ok(content) => content,
        Err(refusal) => return refusal,
    };

    let mut result = serde_json::Map::new();
    result.insert(key.to_string(), serde_json::Value::String(content));
    Json(serde_json::Value::Object(result)).into_response()
}

/// The most tokens a search answer may use.
const SEARCH_MAX_TOKENS: u32 = 500;

/// What the usage log records the search endpoints' calls as.
pub(crate) const SEARCH_USAGE_SOURCE: &str = "kernel_search";

/// Who an AI search call is made for, or the refusal for a visitor who may
/// not make one.
///
/// The three endpoints ask for `use ai`, as every other AI route does. A
/// default install grants it to every role that may search (migration
/// `20261130000001_grant_use_ai_to_searchers`), so an ordinary visitor still
/// gets expansion, summaries and follow-ups. A signed-in person's calls count
/// against their token budget; an anonymous visitor's against the site-level
/// anonymous cap.
async fn ai_caller(state: &AppState, session: &Session) -> Result<AiCaller, Response> {
    let viewer = crate::routes::item::get_user_context(session, state).await;
    if !viewer.can(USE_AI_PERMISSION) {
        return Err(super::helpers::refusal_for(
            &viewer,
            format!("Permission required: {USE_AI_PERMISSION}"),
        )
        .into_response());
    }
    Ok(if viewer.authenticated {
        AiCaller::user(viewer.id, SEARCH_USAGE_SOURCE)
    } else {
        AiCaller::anonymous(SEARCH_USAGE_SOURCE)
    })
}

/// The permission the AI search endpoints ask for.
const USE_AI_PERMISSION: &str = "use ai";

/// Send one search prompt through the send path and return the answer's text,
/// or the response the endpoint answers with instead.
///
/// For the three endpoints. A spent budget is a 429; any other failure is
/// logged by the send path, the provider's own words included, and is a
/// generic 503.
async fn complete(
    ai_providers: &AiProviderService,
    resolved: &ResolvedProvider,
    caller: &AiCaller,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, Response> {
    send_prompt(ai_providers, resolved, caller, system_prompt, user_prompt)
        .await
        .map_err(|e| match e {
            AiCallError::BudgetExceeded => AppError::RateLimited {
                retry_after_secs: 3600,
                category: "token_budget".to_string(),
            }
            .into_response(),
            _ => AppError::service_unavailable("AI", e.user_message()).into_response(),
        })
}

/// Send one search prompt through the kernel's one send path, which checks the
/// base URL and the caller's budget, applies the configured timeout and the
/// chat breaker, and records the call's usage. For the endpoints and the
/// sentiment classifier.
pub(crate) async fn send_prompt(
    ai_providers: &AiProviderService,
    resolved: &ResolvedProvider,
    caller: &AiCaller,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, AiCallError> {
    let request = ChatRequest::system_and_user(system_prompt, user_prompt, SEARCH_MAX_TOKENS);
    match ai_providers.chat_once(resolved, caller, &request).await {
        Ok(completion) => Ok(completion.text.unwrap_or_default()),
        Err(e) => {
            tracing::warn!(error = %e, source = %caller.source, "AI search request failed");
            Err(e)
        }
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::services::ai_provider::ProviderProtocol;

    fn resolved(protocol: ProviderProtocol) -> crate::services::ai_provider::ResolvedProvider {
        crate::services::ai_provider::ResolvedProvider {
            config: crate::services::ai_provider::AiProviderConfig {
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

    /// Nothing configures a temperature for query expansion or summaries, so
    /// none is sent: a hard coded 0.3 made every current Claude model answer 400.
    #[test]
    fn search_ai_requests_send_no_temperature() {
        for protocol in [
            ProviderProtocol::Anthropic,
            ProviderProtocol::OpenAiCompatible,
        ] {
            let request = ChatRequest::system_and_user("sys", "user", SEARCH_MAX_TOKENS);
            let (_, body, _) =
                crate::services::ai_tools::build_chat_request(&resolved(protocol), &request, false);
            assert!(body.get("temperature").is_none(), "{protocol:?}: {body}");
        }
    }

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
