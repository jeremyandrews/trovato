//! Scolta AI search endpoints.
//!
//! `POST /api/v1/search/expand` — AI query expansion (returns alternative terms).
//! `POST /api/v1/search/summarize`: AI summary of search results (JSON response).
//! `POST /api/v1/search/followup`: Follow-up conversation (JSON response).
//!
//! All endpoints ask for the `use ai` permission, and require the `trovato_ai`
//! plugin to be enabled and an AI provider configured for Chat operations.
//!
//! Each one first reads its stage switch from the search settings
//! ([`SearchSettings`]) and answers 404 when the stage is off, before any cache
//! or provider work, so switching a stage off stops the spending and not just
//! the stock page's use of it. The follow-up endpoint also enforces the turn cap
//! there.
//!
//! The browser never supplies the text the model reads. Summarize and
//! follow-up take result **URLs**, not excerpts: each URL is resolved to an
//! item on the Live stage, loaded, and run through
//! [`crate::content::ItemService::filter_page_for_view`] for the calling
//! viewer, so a URL naming an unpublished item, or a field the viewer may not
//! see, contributes nothing. What survives is cut to `ai_summary_max_chars`
//! before it reaches the prompt. A request body over 64 KiB is refused before
//! any of this runs.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::page_meta::text_of;
use crate::error::AppError;
use crate::models::{Item, SiteConfig, UrlAlias};
use crate::search::analytics::Counter;
use crate::search::{MAX_QUERY_LENGTH, SearchSettings, normalize_query, prompts};
use crate::services::ai_provider::{
    AiCallError, AiCaller, AiOperationType, AiProviderService, ChatRequest, ResolvedProvider,
};
use crate::state::AppState;
use crate::tap::UserContext;

/// The most a search endpoint's request body may be: a query, a handful of
/// result URLs or conversation turns, never the whole page content a browser
/// used to be able to stuff in.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Build the Scolta search API router.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    use axum::extract::DefaultBodyLimit;
    use axum::routing::post;

    let limit = || DefaultBodyLimit::max(MAX_BODY_BYTES);

    super::api::ApiRoutes::new("search")
        .route(
            axum::http::Method::POST,
            "/search/expand",
            post(expand_query).layer(limit()),
            doc("AI query expansion — returns alternative search terms"),
        )
        .route(
            axum::http::Method::POST,
            "/search/summarize",
            post(summarize).layer(limit()),
            doc("AI summary of search results, built server-side from result URLs"),
        )
        .route(
            axum::http::Method::POST,
            "/search/followup",
            post(followup).layer(limit()),
            doc("Follow-up conversation about search results"),
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
/// `results` names what the summary reads; the server resolves each URL to an
/// item and builds the text itself ([`context_for_results`]). `deny_unknown_fields`
/// so an old client still sending `context` or `excerpts`, the browser's own
/// text, gets a 400 rather than being silently ignored.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummarizeRequest {
    query: String,
    #[serde(default)]
    results: Vec<ResultRef>,
}

/// Request for follow-up conversation.
///
/// The only shape `scolta.js` sends: a conversation and, optionally, result
/// URLs for whatever this follow-up searched up. `deny_unknown_fields` so the
/// retired `query`/`history` shape is refused rather than silently accepted.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowupRequest {
    messages: Vec<Message>,
    #[serde(default)]
    results: Vec<ResultRef>,
}

/// A search result named by its URL, the only thing the browser tells the
/// server about a result. The server resolves it to an item and reads the
/// item's own stored text, never text carried in the request.
#[derive(Debug, Deserialize)]
struct ResultRef {
    url: String,
}

/// A conversation message. `role` must be `user` or `assistant`
/// (case-insensitive); anything else is refused.
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
    let (_viewer, caller) = match ai_caller(&state, &session).await {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };

    if body.query.trim().is_empty() {
        return AppError::bad_request("Query cannot be empty").into_response();
    }
    if let Some(refusal) = query_too_long(&body.query) {
        return refusal;
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

/// Summarize search results via AI.
///
/// No CSRF required — read-only search enhancement.
async fn summarize(
    State(state): State<AppState>,
    session: Session,
    body: Result<Json<SummarizeRequest>, JsonRejection>,
) -> Response {
    let body = match parse_json(body) {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    if !settings.summarize {
        return stage_off("The search summary");
    }
    let (viewer, caller) = match ai_caller(&state, &session).await {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };

    if body.query.trim().is_empty() {
        return AppError::bad_request("Query cannot be empty").into_response();
    }
    if let Some(refusal) = query_too_long(&body.query) {
        return refusal;
    }

    let context_text = context_for_results(
        &state,
        &viewer,
        &body.results,
        settings.ai_summary_top_n as usize,
        settings.ai_summary_max_chars as usize,
    )
    .await;

    // Nothing the viewer may read survived resolution: answer with an empty
    // summary rather than spend a provider call on an empty prompt.
    if context_text.is_empty() {
        return Json(serde_json::json!({ "summary": "" })).into_response();
    }

    let user_prompt = format!(
        "Search query: {}\n\nSearch result excerpts:\n{}",
        body.query, context_text
    );

    json_ai_response(&state, &caller, prompts::SUMMARIZE, &user_prompt).await
}

/// Handle follow-up conversation via AI.
///
/// No CSRF required — read-only search enhancement.
async fn followup(
    State(state): State<AppState>,
    session: Session,
    body: Result<Json<FollowupRequest>, JsonRejection>,
) -> Response {
    let body = match parse_json(body) {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    if !settings.followup {
        return stage_off("Follow-up");
    }
    let (viewer, caller) = match ai_caller(&state, &session).await {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };

    for message in &body.messages {
        if !message.role.eq_ignore_ascii_case("user")
            && !message.role.eq_ignore_ascii_case("assistant")
        {
            return AppError::bad_request("Message role must be \"user\" or \"assistant\"")
                .into_response();
        }
    }
    // The newest question is the one thing here shaped like a search query;
    // the rest of the conversation is bounded by the per-message and
    // whole-conversation character caps below instead.
    if let Some(last) = body.messages.last()
        && last.role.eq_ignore_ascii_case("user")
        && let Some(refusal) = query_too_long(&last.content)
    {
        return refusal;
    }

    // The turn cap counts `user` turns only after every role has been
    // checked, so a role other than user/assistant cannot buy extra turns.
    let user_turns = body
        .messages
        .iter()
        .filter(|m| m.role.eq_ignore_ascii_case("user"))
        .count();
    // The opening question plus `max_followups` follow-ups.
    if user_turns > settings.max_followups as usize + 1 {
        return AppError::bad_request(format!(
            "A conversation may hold at most {} follow-up questions",
            settings.max_followups
        ))
        .into_response();
    }

    let max_chars = settings.ai_summary_max_chars as usize;
    let mut conversation = String::new();
    for message in &body.messages {
        conversation.push_str(&truncate_chars(&message.content, max_chars));
        conversation.push_str("\n\n");
    }
    // The whole conversation, capped: `max_followups` turns plus the opening
    // summary, each worth at most one message's share. Previous assistant
    // turns are still client-supplied text; this is what bounds them, not a
    // resolution against stored content.
    let conversation_budget = (settings.max_followups as usize + 1) * max_chars;
    let mut user_prompt = truncate_chars(&conversation, conversation_budget);

    let results_context = context_for_results(
        &state,
        &viewer,
        &body.results,
        settings.ai_summary_top_n as usize,
        max_chars,
    )
    .await;
    if !results_context.is_empty() {
        user_prompt.push_str("\nAdditional search results for this follow-up:\n");
        user_prompt.push_str(&results_context);
    }

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
        && let Some((question, previous_answer)) = last_exchange(&body.messages)
    {
        state.sentiment().submit(question, previous_answer);
    }

    response
}

// ============================================================================
// Helpers
// ============================================================================

/// Turn a failed `Json<T>` extraction into the same 400 shape every other
/// refusal on these routes answers with, rather than axum's default 422: an
/// old client sending a retired field (`context`, `excerpts`, the
/// `query`/`history` follow-up shape) gets a refusal in the family these
/// endpoints already use. A body over [`MAX_BODY_BYTES`] keeps its 413: that
/// refusal is about size, not shape, and belongs with the other oversize
/// refusals, not folded into this one.
#[allow(clippy::result_large_err)]
fn parse_json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    body.map(|Json(body)| body).map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            rejection.into_response()
        } else {
            AppError::bad_request(rejection.body_text()).into_response()
        }
    })
}

/// The answer for a stage the search settings switched off: 404, with a short
/// JSON error, before any cache or provider work.
fn stage_off(stage: &str) -> Response {
    crate::error::AppError::with_status(
        StatusCode::NOT_FOUND,
        format!("{stage} is switched off on this site"),
    )
    .into_response()
}

/// The refusal for a query longer than [`MAX_QUERY_LENGTH`], the same bound
/// `/api/v1/search` enforces: `None` when the query is within it.
fn query_too_long(query: &str) -> Option<Response> {
    if query.len() > MAX_QUERY_LENGTH {
        Some(
            AppError::bad_request(format!(
                "query exceeds maximum length of {MAX_QUERY_LENGTH} characters"
            ))
            .into_response(),
        )
    } else {
        None
    }
}

/// Cut `s` to at most `max_chars` **characters** (not bytes), so a multi-byte
/// character is never split.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// The visitor's newest message and the assistant answer before it.
fn last_exchange(messages: &[Message]) -> Option<(&str, &str)> {
    let (last, earlier) = messages.split_last()?;
    if !last.role.eq_ignore_ascii_case("user") {
        return None;
    }
    let previous = earlier
        .iter()
        .rev()
        .find(|m| m.role.eq_ignore_ascii_case("assistant"))?;
    Some((last.content.as_str(), previous.content.as_str()))
}

/// Resolve at most `top_n` result URLs to items the viewer may see, and build
/// their text context: each surviving item's title and the text of the
/// fields the viewer may read, cut to `max_chars`. A URL that does not
/// resolve, or resolves to an item the viewer cannot see at all or has
/// nothing left to show after field filtering, contributes nothing, dropped
/// silently rather than refused, since a stale or guessed URL is routine (a
/// result the browser saw a moment ago may since have been unpublished).
async fn context_for_results(
    state: &AppState,
    viewer: &UserContext,
    results: &[ResultRef],
    top_n: usize,
    max_chars: usize,
) -> String {
    let mut candidates = Vec::new();
    for result in results.iter().take(top_n) {
        if let Some(item) = resolve_item_for_url(state, &result.url).await {
            candidates.push(item);
        }
    }
    let page_size = candidates.len();
    let visible = state
        .items()
        .filter_page_for_view(candidates, viewer, "view", page_size)
        .await;

    visible
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let text = item_text(item, max_chars)?;
            Some(format!("[{}] {}\n{}", i + 1, item.title, text))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Resolve a result URL to the item it names: `/item/<uuid>` directly, else
/// through the kernel's URL alias lookup on the Live stage, the same
/// resolution a browser's request for that path would get.
async fn resolve_item_for_url(state: &AppState, url: &str) -> Option<Item> {
    let path = request_path(url);
    if path.is_empty() {
        return None;
    }
    let id = if let Some(rest) = path.strip_prefix("/item/") {
        Uuid::parse_str(rest).ok()?
    } else {
        let found = UrlAlias::resolve_path(
            state.db(),
            &[path],
            crate::models::stage::LIVE_STAGE_ID,
            "en",
        )
        .await
        .ok()??;
        found
            .alias
            .source
            .strip_prefix("/item/")
            .and_then(|s| Uuid::parse_str(s).ok())?
    };
    state.items().load(id).await.ok().flatten()
}

/// The path a URL names: everything from the first `/` after a `scheme://`
/// host, or the whole string when it carries no scheme, with any query string
/// or fragment dropped.
fn request_path(url: &str) -> &str {
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |i| &rest[i..]),
        None => url,
    };
    path.split(['?', '#']).next().unwrap_or("")
}

/// The text an item contributes to a summary or follow-up prompt: its title
/// and the text of every field still present on it, tags stripped, cut to
/// `max_chars` characters. `None` when nothing is left to show: an item with
/// no visible text is dropped rather than contributing an empty entry.
///
/// Called after [`crate::content::ItemService::filter_page_for_view`], so
/// `item.fields` already holds only the fields this viewer may see; reading
/// every field present is reading exactly what the viewer may read.
fn item_text(item: &Item, max_chars: usize) -> Option<String> {
    let mut text = item.title.clone();
    if let Some(fields) = item.fields.as_object() {
        for value in fields.values() {
            collect_field_text(value, &mut text);
        }
    }
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(truncate_chars(text, max_chars))
    }
}

/// Append one field value's plain text to `out`, space-separated: a string
/// value, a structured `{value, summary}` field, or an array of either,
/// recursively.
fn collect_field_text(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(s) => {
            out.push(' ');
            out.push_str(&text_of(s));
        }
        serde_json::Value::Object(obj) => {
            for key in ["value", "summary"] {
                if let Some(serde_json::Value::String(s)) = obj.get(key) {
                    out.push(' ');
                    out.push_str(&text_of(s));
                }
            }
        }
        serde_json::Value::Array(values) => {
            for v in values {
                collect_field_text(v, out);
            }
        }
        _ => {}
    }
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
/// not make one. The viewer is handed back too: summarize and follow-up use
/// it again, to decide which items and fields the prompt may be built from.
///
/// The three endpoints ask for `use ai`, as every other AI route does. A
/// default install grants it to every role that may search (migration
/// `20261130000001_grant_use_ai_to_searchers`), so an ordinary visitor still
/// gets expansion, summaries and follow-ups. A signed-in person's calls count
/// against their token budget; an anonymous visitor's against the site-level
/// anonymous cap.
async fn ai_caller(
    state: &AppState,
    session: &Session,
) -> Result<(UserContext, AiCaller), Response> {
    let viewer = crate::routes::item::get_user_context(session, state).await;
    if !viewer.can(USE_AI_PERMISSION) {
        return Err(super::helpers::refusal_for(
            &viewer,
            format!("Permission required: {USE_AI_PERMISSION}"),
        )
        .into_response());
    }
    let caller = if viewer.authenticated {
        AiCaller::user(viewer.id, SEARCH_USAGE_SOURCE)
    } else {
        AiCaller::anonymous(SEARCH_USAGE_SOURCE)
    };
    Ok((viewer, caller))
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
