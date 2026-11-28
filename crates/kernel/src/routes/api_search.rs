//! Scolta AI search endpoints.
//!
//! `POST /api/v1/search/expand` — AI query expansion (returns alternative terms).
//! `POST /api/v1/search/summarize` — AI summary of search results (plain JSON).
//! `POST /api/v1/search/followup` — Follow-up conversation (plain JSON).
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
//! **The server builds its own prompt.** Neither `summarize` nor `followup`
//! takes prompt text from the browser: both take `results: [{ "url": ... }]`,
//! resolve each URL to an item ([`resolve_result_url`]) and run it through
//! [`crate::content::item_service::ItemService::filter_page_for_view`] for the
//! calling viewer, so a caller gets context built only from what they may see
//! — not from an item that is unpublished, on a stage they cannot reach, or
//! carrying a field their role cannot view. Every request is bounded: a 64 KiB
//! body limit on all three routes, [`crate::search::MAX_QUERY_LENGTH`] on the
//! query, [`SearchSettings::ai_summary_top_n`] results, each item's text cut to
//! [`SearchSettings::ai_summary_max_chars`], and the turn cap on follow-up.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::ItemService;
use crate::content::page_meta::{text_of, truncate_on_word};
use crate::error::AppError;
use crate::models::{Item, SiteConfig, UrlAlias};
use crate::search::analytics::Counter;
use crate::search::{MAX_QUERY_LENGTH, SearchSettings, normalize_query, prompts};
use crate::services::ai_provider::{
    AiCallError, AiCaller, AiOperationType, AiProviderService, ChatRequest, ResolvedProvider,
};
use crate::state::AppState;
use crate::tap::UserContext;

/// Build the Scolta search API router.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    use axum::extract::DefaultBodyLimit;
    use axum::http::Method;
    use axum::routing::post;

    /// The three endpoints take a query and a short list of result URLs; this
    /// is a generous ceiling for that, not a target. Nothing a legitimate
    /// client sends is anywhere close to it; it exists so a request body is
    /// never the unbounded thing a prompt is built from.
    const MAX_BODY_BYTES: usize = 64 * 1024;

    super::api::ApiRoutes::new("search")
        .route(
            Method::POST,
            "/search/expand",
            post(expand_query).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
            doc("AI query expansion — returns alternative search terms"),
        )
        .route(
            Method::POST,
            "/search/summarize",
            post(summarize).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
            doc("AI summary of search results, built from the results' own content"),
        )
        .route(
            Method::POST,
            "/search/followup",
            post(followup).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
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
/// No prompt text travels in this body: `results` names the search results the
/// summary is about, by URL, and the server resolves and reads them itself
/// ([`build_results_context`]). `deny_unknown_fields` so an old client sending
/// `context` or `excerpts` — free text the server once trusted — gets a 400
/// instead of being silently ignored.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummarizeRequest {
    query: String,
    #[serde(default)]
    results: Vec<ResultRef>,
}

/// Request for follow-up conversation.
///
/// `messages` is the conversation so far, oldest first; `results` is this
/// turn's search results, by URL, the same as [`SummarizeRequest::results`].
/// This is the one shape `scolta.js` sends; there is no second accepted shape.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowupRequest {
    messages: Vec<Message>,
    #[serde(default)]
    results: Vec<ResultRef>,
}

/// One search result, named only by the URL the server resolves it from.
///
/// Public so an integration test can build a [`build_results_context`] call
/// directly, the way it builds the request body for the real endpoint.
#[derive(Debug, Deserialize)]
pub struct ResultRef {
    pub url: String,
}

/// A conversation message. `role` must be `user` or `assistant`
/// (case-insensitively); anything else is refused rather than forwarded.
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
        Ok(v) => v,
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
/// No CSRF required — read-only search enhancement. Takes result URLs, not
/// text: see the module documentation.
async fn summarize(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<SummarizeRequest>,
) -> Response {
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    if !settings.summarize {
        return stage_off("The search summary");
    }
    let (viewer, caller) = match ai_caller(&state, &session).await {
        Ok(v) => v,
        Err(refusal) => return refusal,
    };

    if body.query.trim().is_empty() {
        return AppError::bad_request("Query cannot be empty").into_response();
    }
    if let Some(refusal) = query_too_long(&body.query) {
        return refusal;
    }

    let context_text =
        build_results_context(state.db(), state.items(), &viewer, &settings, &body.results).await;
    if context_text.is_empty() {
        // Nothing the caller named resolved to anything they may see: answer
        // with an empty summary rather than ask the provider to summarize
        // nothing.
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
/// No CSRF required — read-only search enhancement. Takes result URLs, not
/// text, exactly as `summarize` does: see the module documentation.
async fn followup(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<FollowupRequest>,
) -> Response {
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    if !settings.followup {
        return stage_off("Follow-up");
    }
    let (viewer, caller) = match ai_caller(&state, &session).await {
        Ok(v) => v,
        Err(refusal) => return refusal,
    };

    // A role outside the two the prompt is built from is refused outright,
    // rather than let through uncounted: that is how an unbounded number of
    // turns under a role like "human" used to slip past the cap below.
    if let Some(bad) = body
        .messages
        .iter()
        .find(|m| !is_user_role(&m.role) && !is_assistant_role(&m.role))
    {
        return AppError::bad_request(format!(
            "Message role must be \"user\" or \"assistant\", got \"{}\"",
            bad.role
        ))
        .into_response();
    }

    // The opening question plus `max_followups` follow-ups.
    let user_turns = body
        .messages
        .iter()
        .filter(|m| is_user_role(&m.role))
        .count();
    if user_turns > settings.max_followups as usize + 1 {
        return AppError::bad_request(format!(
            "A conversation may hold at most {} follow-up questions",
            settings.max_followups
        ))
        .into_response();
    }

    // The newest turn is the follow-up question; the same length bound
    // `/api/v1/search`, `expand` and `summarize` hold a query to applies here.
    if let Some(last) = body.messages.last()
        && is_user_role(&last.role)
        && let Some(refusal) = query_too_long(&last.content)
    {
        return refusal;
    }

    let max_chars = settings.ai_summary_max_chars as usize;
    let mut user_prompt = body
        .messages
        .iter()
        .map(|m| {
            let role = if is_user_role(&m.role) {
                "user"
            } else {
                "assistant"
            };
            format!("{role}: {}", truncate_on_word(&m.content, max_chars))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let results_context =
        build_results_context(state.db(), state.items(), &viewer, &settings, &body.results).await;
    if !results_context.is_empty() {
        user_prompt.push_str(&format!(
            "\n\nAdditional search results for this follow-up:\n{results_context}",
        ));
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

/// Whether a message role names the visitor's turn, case-insensitively.
fn is_user_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("user")
}

/// Whether a message role names the assistant's turn, case-insensitively.
fn is_assistant_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("assistant")
}

/// The visitor's newest message and the assistant answer before it.
///
/// The search context a follow-up carries travels in `results`, never inside
/// a message's `content`, so every message here is the visitor's or the
/// assistant's own words.
fn last_exchange(body: &FollowupRequest) -> Option<(&str, &str)> {
    let (last, earlier) = body.messages.split_last()?;
    if !is_user_role(&last.role) {
        return None;
    }
    let previous = earlier.iter().rev().find(|m| is_assistant_role(&m.role))?;
    Some((last.content.as_str(), previous.content.as_str()))
}

/// The URL a `ResultRef` names, resolved to the item it points at, or `None`
/// when it names nothing the kernel can find.
///
/// `scolta.js` sends Pagefind's `meta.url`, which is either `/item/<uuid>`
/// directly or an alias `/item/<uuid>` resolves to on the Live stage — never
/// an item id by itself. Parsing `/item/<uuid>` is tried first, so an alias
/// lookup is not spent on the common case; an alias that does not exist, or
/// resolves to something other than `/item/<uuid>` (a plugin-owned source),
/// answers `None`.
async fn resolve_result_url(pool: &sqlx::PgPool, url: &str) -> Option<Uuid> {
    if let Some(id) = url
        .strip_prefix("/item/")
        .and_then(|id| Uuid::parse_str(id).ok())
    {
        return Some(id);
    }
    let alias = UrlAlias::find_by_alias(pool, url).await.ok().flatten()?;
    alias
        .source
        .strip_prefix("/item/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

/// Every string a field's JSON value carries, structured (`{"value": ...}`,
/// `{"summary": ...}`) or plain, recursively through arrays.
fn collect_field_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Object(obj) => {
            for key in ["value", "summary"] {
                if let Some(serde_json::Value::String(s)) = obj.get(key) {
                    out.push(s.clone());
                }
            }
        }
        serde_json::Value::Array(values) => {
            for v in values {
                collect_field_strings(v, out);
            }
        }
        _ => {}
    }
}

/// One item's AI context: its title and the text of the fields the viewer may
/// see — `item.fields` has already been through
/// [`crate::content::item_service::ItemService::filter_page_for_view`], so a
/// field the viewer may not view is not here to read — tags stripped, cut to
/// `max_chars`.
fn item_context_text(item: &Item, max_chars: usize) -> String {
    let mut parts = vec![item.title.clone()];
    if let Some(obj) = item.fields.as_object() {
        for value in obj.values() {
            collect_field_strings(value, &mut parts);
        }
    }
    let text = text_of(&parts.join(" "));
    truncate_on_word(&text, max_chars)
}

/// Resolve a request's result URLs to items the viewer may see, and build the
/// AI context from them: title and visible-field text, one item per surviving
/// URL, in the order the request named them.
///
/// At most `ai_summary_top_n` URLs are looked at; each item's text is cut to
/// `ai_summary_max_chars`. A URL that does not resolve, or resolves to an item
/// the viewer may not view at all, is dropped silently — the caller gets
/// context built from whatever is left, including none of it.
///
/// Takes `pool` and `items` rather than the whole [`AppState`], the way
/// [`crate::services::ai_chat::AiChatService::search_for_context`] takes its
/// `ItemService`: a test can hand this a service whose dispatcher loads
/// `trovato_field_access_ref`, to assert a denied field never reaches a
/// prompt, without a real field-access plugin wired into the whole app. `pub`
/// for exactly that: `search_ai_lockdown_test` calls it directly.
pub async fn build_results_context(
    pool: &sqlx::PgPool,
    items: &ItemService,
    viewer: &UserContext,
    settings: &SearchSettings,
    results: &[ResultRef],
) -> String {
    let top_n = settings.ai_summary_top_n as usize;
    let max_chars = settings.ai_summary_max_chars as usize;

    let mut candidates = Vec::new();
    for result in results.iter().take(top_n) {
        if let Some(id) = resolve_result_url(pool, &result.url).await
            && let Ok(Some(item)) = items.load(id).await
        {
            candidates.push(item);
        }
    }

    let visible = items
        .filter_page_for_view(candidates, viewer, "view", top_n)
        .await;

    visible
        .iter()
        .enumerate()
        .map(|(i, item)| format!("[{}] {}", i + 1, item_context_text(item, max_chars)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The refusal for a query longer than the kernel accepts anywhere, or `None`
/// when it is within bounds. The same bound `/api/v1/search` enforces
/// (`routes/search.rs`), applied here so a search prompt cannot be grown past
/// it by the one field the browser still supplies free text in.
fn query_too_long(query: &str) -> Option<Response> {
    (query.len() > MAX_QUERY_LENGTH).then(|| {
        AppError::bad_request(format!(
            "query exceeds maximum length of {MAX_QUERY_LENGTH} characters"
        ))
        .into_response()
    })
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
