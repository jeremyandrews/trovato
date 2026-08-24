//! The AI Assistant's page and API (0.102).
//!
//! Six routes and one shape: a themed page at `/ai/assistant/{scope}` that a
//! person reads, and four API endpoints under `/api/v1/assistant/` that change
//! the conversation. Every one of them belongs to exactly one person — the
//! conversation's owner — and says 404 to anyone else, including an
//! administrator, because a conversation is somebody's working notes rather
//! than site content.
//!
//! # Without JavaScript
//!
//! Everything except sending a message works with JavaScript switched off. The
//! proposal cards are `<form method="post">` with a `_token` field, and so is
//! Start over; the kernel accepts a CSRF token from that field exactly as it
//! accepts one from the `X-CSRF-Token` header (the 0.101 surface a plugin-served
//! form needed). Sending a message is the exception because the reply is an SSE
//! stream, and a stream is what a turn is: the person watches a tool being
//! called and a proposal appearing while it happens.
//!
//! # What guards a turn
//!
//! In order, and in this order deliberately: ownership, then whether the
//! conversation still takes messages, then the per-person rate limit, then the
//! token budget. Rate limiting protects the server and runs before the budget
//! check, which protects the bill; a request refused for budget has still spent
//! a rate token, which is the same trade `routes::api_chat` makes.

use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use serde::Deserialize;
use tower_sessions::Session;
use trovato_sdk::types::{
    AssistantIdKind, AssistantToolCall, AssistantToolKind, AssistantToolMode,
};
use uuid::Uuid;

use crate::assistant::RegisteredScope;
use crate::error::AppError;
use crate::models::assistant::{
    Conversation, PROPOSAL_APPLIED, PROPOSAL_DISCARDED, PROPOSAL_FAILED, PROPOSAL_PROPOSED,
    Proposal, STATUS_OPEN, TranscriptEntry, now as epoch_now,
};
use crate::routes::helpers::{get_user_context, html_escape};
use crate::services::ai_assistant::{
    AssistantConfig, TurnRequest, dispatch_context, dispatch_tool, run_turn, truncate_snapshot,
};
use crate::services::ai_provider::AiOperationType;
use crate::services::ai_token_budget::BudgetAction;
use crate::state::AppState;
use crate::tap::UserContext;

/// Longest message a person may send, in characters.
const MAX_MESSAGE_CHARS: usize = 4096;

/// Longest `scope_id` a `String`-kind scope will accept, in bytes.
const MAX_SCOPE_ID_BYTES: usize = 128;

/// The request body cap. A message is at most 4096 characters; 16 KiB leaves
/// room for multi-byte text and JSON overhead and nothing more.
const MAX_BODY_BYTES: usize = 16 * 1024;

// =============================================================================
// Rate limiting
// =============================================================================

/// Per-person message counters: `(count, window_start)`.
///
/// In-process and non-persistent, the same shape and the same limitation as the
/// chatbot's: it resets on restart and is not shared across instances. Adequate
/// for a single instance; a clustered deployment wants a Redis-backed limiter
/// for both.
static ASSISTANT_RATE_LIMITS: std::sync::LazyLock<DashMap<String, (u32, Instant)>> =
    std::sync::LazyLock::new(DashMap::new);

/// When the counters were last swept.
static LAST_EVICTION: std::sync::LazyLock<std::sync::Mutex<Instant>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Instant::now()));

/// Count one message against a person's hourly limit.
fn check_rate_limit(user_key: &str, limit_per_hour: u32) -> bool {
    if limit_per_hour == 0 {
        return true;
    }
    let now = Instant::now();

    if let Ok(mut last) = LAST_EVICTION.lock()
        && now.duration_since(*last) > Duration::from_secs(60)
    {
        ASSISTANT_RATE_LIMITS.retain(|_, v| now.duration_since(v.1) < Duration::from_secs(3600));
        *last = now;
    }

    let mut entry = ASSISTANT_RATE_LIMITS
        .entry(user_key.to_string())
        .or_insert((0, now));

    if now.duration_since(entry.1) > Duration::from_secs(3600) {
        *entry = (1, now);
        return true;
    }
    if entry.0 >= limit_per_hour {
        return false;
    }
    entry.0 += 1;
    true
}

/// Clear every rate-limit counter.
///
/// `pub` but `#[doc(hidden)]` solely for the integration tests in the separate
/// `tests/` crate, which need to prove the limit both bites and resets. Not part
/// of the public API; nothing in production calls it.
#[doc(hidden)]
pub fn clear_assistant_rate_limits() {
    ASSISTANT_RATE_LIMITS.clear();
}

// =============================================================================
// Shared resolution
// =============================================================================

/// Everything a request needs resolved before it can do anything.
struct Resolved {
    user: UserContext,
    config: AssistantConfig,
}

/// Resolve the caller and the configuration, or say why not.
///
/// An anonymous caller is sent to the login form — the bare one every other
/// kernel route uses, with no destination parameter, because the login form has
/// none to read.
async fn resolve(state: &AppState, session: &Session) -> Result<Resolved, Response> {
    let user = get_user_context(session, state).await;
    if !user.authenticated {
        return Err(Redirect::to("/user/login").into_response());
    }

    let config = match state.ai_assistant().load_config().await {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = %e, "failed to load the assistant configuration");
            return Err(page_error(
                state,
                session,
                StatusCode::INTERNAL_SERVER_ERROR,
                "The assistant is not available.",
            )
            .await);
        }
    };

    Ok(Resolved { user, config })
}

/// Whether this person may open this scope.
///
/// Three permissions, all of them, unless the caller is an administrator:
/// `use ai` is the site-wide AI gate, `use ai assistant` is this feature's, and
/// the scope's own permission is the plugin's.
fn may_open(user: &UserContext, scope: &RegisteredScope) -> bool {
    if user.is_admin() {
        return true;
    }
    user.has_permission("use ai")
        && user.has_permission("use ai assistant")
        && (scope.scope.permission.is_empty() || user.has_permission(&scope.scope.permission))
}

/// Render a themed error page.
async fn page_error(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    message: &str,
) -> Response {
    let mut context = tera::Context::new();
    crate::routes::helpers::inject_site_context(state, session, &mut context, "/ai/assistant")
        .await;
    let body = format!("<p class=\"assistant-error\">{}</p>", html_escape(message));
    let html = state
        .theme()
        .render_page("/ai/assistant", "Assistant", &body, &mut context)
        .unwrap_or_else(|_| format!("<!DOCTYPE html><html><body>{body}</body></html>"));
    (status, axum::response::Html(html)).into_response()
}

/// Check a `scope_id` against what the scope says one is.
///
/// Returns the id to store, or `Err` when the path does not describe anything
/// this scope could be opened on.
async fn validate_scope_id(
    state: &AppState,
    scope: &RegisteredScope,
    scope_id: Option<String>,
) -> Result<Option<String>, ()> {
    match scope.scope.id_kind {
        AssistantIdKind::None => match scope_id {
            // A site-wide scope has nothing to be opened *on*; an id in the path
            // is a URL that means nothing, not a URL with a spare segment.
            Some(_) => Err(()),
            None => Ok(None),
        },
        AssistantIdKind::String => match scope_id {
            Some(id) if !id.is_empty() && id.len() <= MAX_SCOPE_ID_BYTES => Ok(Some(id)),
            _ => Err(()),
        },
        AssistantIdKind::Item => {
            let Some(id) = scope_id else {
                return Err(());
            };
            let Ok(uuid) = Uuid::parse_str(&id) else {
                return Err(());
            };
            match state.items().load(uuid).await {
                Ok(Some(item)) if scope.scope.item_types.contains(&item.item_type) => Ok(Some(id)),
                _ => Err(()),
            }
        }
    }
}

/// Whether a conversation still takes messages.
///
/// Read-only is not closed: the transcript stays readable and Start over is the
/// way on. Four things make it so, and the caller is told which in the page.
fn read_only_reason(conversation: &Conversation, config: &AssistantConfig) -> Option<String> {
    if !conversation.is_open() {
        return Some("This conversation was replaced by a newer one.".to_string());
    }
    if conversation.message_count >= config.max_messages as i32 {
        return Some("This conversation has reached its message limit.".to_string());
    }
    if conversation.tokens_used >= config.max_tokens_per_conversation as i64 {
        return Some("This conversation has reached its token limit.".to_string());
    }
    let age_hours = (epoch_now() - conversation.created) / 3600;
    if config.conversation_ttl_hours > 0 && age_hours >= i64::from(config.conversation_ttl_hours) {
        return Some("This conversation is older than the site keeps them.".to_string());
    }
    None
}

/// Open the caller's conversation for this scope, creating it if there is none.
async fn open_conversation(
    state: &AppState,
    user: &UserContext,
    scope: &RegisteredScope,
    scope_id: Option<&str>,
    config: &AssistantConfig,
) -> anyhow::Result<Conversation> {
    if let Some(existing) =
        Conversation::find_open(state.db(), user.id, &scope.scope.name, scope_id).await?
    {
        return Ok(existing);
    }
    create_conversation(state, user, scope, scope_id, config).await
}

/// Create a conversation, asking the plugin to describe what is being configured.
async fn create_conversation(
    state: &AppState,
    user: &UserContext,
    scope: &RegisteredScope,
    scope_id: Option<&str>,
    config: &AssistantConfig,
) -> anyhow::Result<Conversation> {
    let context = dispatch_context(state, user, &scope.plugin, &scope.scope.name, scope_id).await;
    let (title, snapshot, links) = match context {
        Some(context) => {
            let links =
                serde_json::to_value(&context.links).unwrap_or_else(|_| serde_json::json!([]));
            (
                context.title,
                truncate_snapshot(&context.snapshot, config.snapshot_max_bytes),
                links,
            )
        }
        // A plugin that cannot describe its own domain still gets a
        // conversation: the tools are what the model works with, and an empty
        // snapshot is honest about there being no overview.
        None => (
            scope.scope.label.clone(),
            String::new(),
            serde_json::json!([]),
        ),
    };

    let now = epoch_now();
    let conversation = Conversation {
        id: Uuid::now_v7(),
        user_id: user.id,
        plugin: scope.plugin.clone(),
        scope: scope.scope.name.clone(),
        scope_id: scope_id.map(str::to_string),
        title,
        status: STATUS_OPEN.to_string(),
        snapshot,
        links,
        transcript: serde_json::json!([]),
        message_count: 0,
        tokens_used: 0,
        created: now,
        changed: now,
    };
    Conversation::create(state.db(), &conversation).await?;
    Ok(conversation)
}

/// Load a conversation the caller owns, or produce the 404 that hides whether
/// somebody else's exists.
async fn owned_conversation(
    state: &AppState,
    user: &UserContext,
    id: Uuid,
) -> Result<Conversation, Response> {
    match Conversation::find_by_id(state.db(), id).await {
        Ok(Some(conversation)) if conversation.user_id == user.id => Ok(conversation),
        Ok(_) => Err(AppError::not_found("Conversation").into_response()),
        Err(e) => Err(AppError::internal_ctx(e, "load conversation").into_response()),
    }
}

// =============================================================================
// The page
// =============================================================================

async fn conversation_page_no_id(
    State(state): State<AppState>,
    session: Session,
    Path(scope_name): Path<String>,
) -> Response {
    conversation_page(state, session, scope_name, None).await
}

async fn conversation_page_with_id(
    State(state): State<AppState>,
    session: Session,
    Path((scope_name, scope_id)): Path<(String, String)>,
) -> Response {
    conversation_page(state, session, scope_name, Some(scope_id)).await
}

async fn conversation_page(
    state: AppState,
    session: Session,
    scope_name: String,
    scope_id: Option<String>,
) -> Response {
    let Resolved { user, config } = match resolve(&state, &session).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    // A disabled assistant serves nothing at all, so a site that turns it off
    // does not leave live conversation URLs behind.
    if !config.enabled {
        return page_error(
            &state,
            &session,
            StatusCode::NOT_FOUND,
            "The assistant is not enabled on this site.",
        )
        .await;
    }

    let Some(scope) = state.assistant_scopes().get(&scope_name).cloned() else {
        return page_error(
            &state,
            &session,
            StatusCode::NOT_FOUND,
            "No such assistant.",
        )
        .await;
    };
    if !config.scope_enabled(&scope_name) {
        return page_error(
            &state,
            &session,
            StatusCode::NOT_FOUND,
            "No such assistant.",
        )
        .await;
    }
    if !may_open(&user, &scope) {
        return page_error(
            &state,
            &session,
            StatusCode::FORBIDDEN,
            "You do not have permission to use this assistant.",
        )
        .await;
    }

    let Ok(scope_id) = validate_scope_id(&state, &scope, scope_id).await else {
        return page_error(
            &state,
            &session,
            StatusCode::NOT_FOUND,
            "That is not something this assistant configures.",
        )
        .await;
    };

    let conversation =
        match open_conversation(&state, &user, &scope, scope_id.as_deref(), &config).await {
            Ok(conversation) => conversation,
            Err(e) => {
                tracing::error!(error = %e, scope = %scope_name, "failed to open a conversation");
                return page_error(
                    &state,
                    &session,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The assistant could not be opened.",
                )
                .await;
            }
        };

    render_conversation(&state, &session, &scope, &conversation, &config).await
}

/// Merge each proposal entry with the proposal's live status, so the template
/// does not have to look one list up in the other.
///
/// A proposal card's transcript entry is what the model asked for, and never
/// changes; its status is what the person did about it, and does. The page needs
/// both in one place — a card that still says "Apply" after the change was
/// applied is the one thing this page must never do.
fn timeline(conversation: &Conversation, proposals: &[Proposal]) -> Vec<serde_json::Value> {
    conversation
        .entries()
        .iter()
        .map(|entry| {
            let mut value = serde_json::to_value(entry).unwrap_or(serde_json::Value::Null);
            let Some(object) = value.as_object_mut() else {
                return value;
            };
            if let Some(id) = object
                .get("proposal_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                && let Some(proposal) = proposals.iter().find(|p| p.id.to_string() == id)
            {
                object.insert("status".to_string(), serde_json::json!(proposal.status));
                object.insert("result".to_string(), serde_json::json!(proposal.result));
            }
            value
        })
        .collect()
}

/// Render the conversation into the site's page template.
///
/// Two steps, the way an item page and a plugin-served page both do it: the
/// inner template first, then `render_page`, so a theme can override the outer
/// page as `page--ai--assistant.html`.
async fn render_conversation(
    state: &AppState,
    session: &Session,
    scope: &RegisteredScope,
    conversation: &Conversation,
    config: &AssistantConfig,
) -> Response {
    let proposals = Proposal::for_conversation(state.db(), conversation.id)
        .await
        .unwrap_or_default();
    let csrf_token = crate::form::csrf::generate_csrf_token(session).await;
    let read_only = read_only_reason(conversation, config);

    let mut inner = tera::Context::new();
    inner.insert("entries", &timeline(conversation, &proposals));
    inner.insert("conversation_id", &conversation.id.to_string());
    inner.insert("title", &conversation.title);
    inner.insert("scope", &scope.scope.name);
    inner.insert("scope_label", &scope.scope.label);
    inner.insert("scope_description", &scope.scope.description);
    inner.insert("links", &conversation.link_list());
    inner.insert("suggestions", &scope.scope.suggestions);
    inner.insert("proposals", &proposals);
    inner.insert("csrf_token", &csrf_token);
    inner.insert("read_only", &read_only.is_some());
    inner.insert("read_only_reason", &read_only.unwrap_or_default());
    inner.insert("message_count", &conversation.message_count);
    inner.insert("max_messages", &config.max_messages);
    inner.insert("max_message_chars", &MAX_MESSAGE_CHARS);
    inner.insert("tokens_used", &conversation.tokens_used);
    inner.insert("max_tokens", &config.max_tokens_per_conversation);

    let body = match state
        .theme()
        .tera()
        .render("assistant/conversation.html", &inner)
    {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to render the assistant conversation");
            return page_error(
                state,
                session,
                StatusCode::INTERNAL_SERVER_ERROR,
                "The assistant could not be rendered.",
            )
            .await;
        }
    };

    let mut context = tera::Context::new();
    crate::routes::helpers::inject_site_context(state, session, &mut context, "/ai/assistant")
        .await;
    let html = state
        .theme()
        .render_page("/ai/assistant", &conversation.title, &body, &mut context)
        .unwrap_or_else(|_| format!("<!DOCTYPE html><html><body>{body}</body></html>"));

    axum::response::Html(html).into_response()
}

// =============================================================================
// GET the conversation as JSON
// =============================================================================

async fn conversation_json(
    State(state): State<AppState>,
    session: Session,
    Path(conversation_id): Path<Uuid>,
) -> Response {
    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return AppError::unauthorized("Authentication required").into_response();
    }
    let conversation = match owned_conversation(&state, &user, conversation_id).await {
        Ok(conversation) => conversation,
        Err(response) => return response,
    };
    let config = state.ai_assistant().load_config().await.unwrap_or_default();
    let proposals = Proposal::for_conversation(state.db(), conversation.id)
        .await
        .unwrap_or_default();
    let read_only = read_only_reason(&conversation, &config);

    Json(serde_json::json!({
        "id": conversation.id,
        "scope": conversation.scope,
        "scope_id": conversation.scope_id,
        "title": conversation.title,
        "status": conversation.status,
        "links": conversation.link_list(),
        "transcript": conversation.entries(),
        "proposals": proposals,
        "read_only": read_only.is_some(),
        "read_only_reason": read_only,
        "limits": {
            "max_messages": config.max_messages,
            "max_tokens_per_conversation": config.max_tokens_per_conversation,
            "max_message_chars": MAX_MESSAGE_CHARS,
        },
        "message_count": conversation.message_count,
        "tokens_used": conversation.tokens_used,
    }))
    .into_response()
}

// =============================================================================
// POST a message
// =============================================================================

/// A message from the person.
#[derive(Debug, Deserialize)]
pub struct MessageInput {
    /// What they typed.
    pub message: String,
}

async fn post_message(
    State(state): State<AppState>,
    session: Session,
    Path(conversation_id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<MessageInput>,
) -> Response {
    if crate::routes::helpers::require_csrf_header(&session, &headers)
        .await
        .is_err()
    {
        return AppError::forbidden("Invalid or missing CSRF token").into_response();
    }

    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return AppError::unauthorized("Authentication required").into_response();
    }

    let config = match state.ai_assistant().load_config().await {
        Ok(config) => config,
        Err(e) => return AppError::internal_ctx(e, "load assistant configuration").into_response(),
    };
    if !config.enabled {
        return AppError::not_found("Assistant").into_response();
    }

    let conversation = match owned_conversation(&state, &user, conversation_id).await {
        Ok(conversation) => conversation,
        Err(response) => return response,
    };

    let Some(scope) = state.assistant_scopes().get(&conversation.scope).cloned() else {
        return AppError::not_found("Assistant scope").into_response();
    };
    if !config.scope_enabled(&conversation.scope) || !may_open(&user, &scope) {
        return AppError::forbidden("Permission required: use ai assistant").into_response();
    }

    let message = input.message.trim().to_string();
    if message.is_empty() {
        return AppError::bad_request("Message cannot be empty").into_response();
    }
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return AppError::bad_request(format!(
            "Message too long (max {MAX_MESSAGE_CHARS} characters)"
        ))
        .into_response();
    }

    if let Some(reason) = read_only_reason(&conversation, &config) {
        return (StatusCode::CONFLICT, AppError::conflict(reason))
            .1
            .into_response();
    }

    if !check_rate_limit(&user.id.to_string(), config.rate_limit_per_hour) {
        return (AppError::RateLimited {
            retry_after_secs: 3600,
            category: "assistant".to_string(),
        })
        .into_response();
    }

    // Resolve the provider before the budget check, so the budget that is
    // checked and the provider that is billed cannot disagree.
    let resolved = match state
        .ai_providers()
        .resolve_provider(AiOperationType::Chat, config.provider_id.as_deref())
        .await
    {
        Ok(Some(mut resolved)) => {
            if let Some(model) = config.model.clone().filter(|m| !m.is_empty()) {
                resolved.model = model;
            }
            resolved
        }
        Ok(None) => {
            return AppError::service_unavailable("AI", "No chat provider is configured")
                .into_response();
        }
        Err(e) => {
            return AppError::service_unavailable("AI", format!("Failed to connect: {e}"))
                .into_response();
        }
    };

    let provider_id = resolved.config.id.clone();
    match state
        .ai_budgets()
        .check_budget(state.db(), user.id, &provider_id)
        .await
    {
        Ok(result) if !result.allowed => match result.action {
            BudgetAction::Deny | BudgetAction::Queue => {
                tracing::warn!(
                    user = %user.id,
                    provider = %provider_id,
                    used = result.used,
                    limit = result.limit,
                    "assistant token budget exceeded"
                );
                return (AppError::RateLimited {
                    retry_after_secs: 3600,
                    category: "token_budget".to_string(),
                })
                .into_response();
            }
            BudgetAction::Warn => {
                tracing::warn!(
                    user = %user.id,
                    provider = %provider_id,
                    "assistant token budget exceeded (warn mode, allowing)"
                );
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "failed to check the assistant budget, allowing request");
        }
        _ => {}
    }

    // The person's message goes in before the turn runs, so a crash during the
    // turn leaves a transcript that says what was asked.
    let mut entries = conversation.entries();
    entries.push(TranscriptEntry::User {
        text: message,
        ts: epoch_now(),
    });
    let message_count = conversation.message_count.saturating_add(1);
    if let Err(e) = Conversation::save_transcript(
        state.db(),
        conversation.id,
        &entries,
        message_count,
        conversation.tokens_used,
    )
    .await
    {
        return AppError::internal_ctx(e, "save the message").into_response();
    }

    let conversation = Conversation {
        transcript: serde_json::to_value(&entries).unwrap_or_else(|_| serde_json::json!([])),
        message_count,
        ..conversation
    };

    let stream = run_turn(
        state.clone(),
        TurnRequest {
            conversation,
            scope,
            config,
            resolved,
            user,
        },
    );

    let sse = async_stream::stream! {
        use tokio_stream::StreamExt;
        let mut pinned = Box::pin(stream);
        while let Some(payload) = pinned.next().await {
            let json = serde_json::to_string(&payload).unwrap_or_default();
            yield Ok::<_, Infallible>(Event::default().data(json));
        }
    };

    Sse::new(sse)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

// =============================================================================
// Proposals
// =============================================================================

/// The form body of a no-JavaScript proposal action.
#[derive(Debug, Deserialize, Default)]
pub struct TokenForm {
    /// The CSRF token, from a hidden `_token` input.
    #[serde(rename = "_token", default)]
    pub token: String,
}

/// Whether this request carried a valid CSRF token, from either place.
///
/// A plain HTML `<form>` cannot set a header, and a proposal card is a plain
/// HTML form on purpose, so the field has to be accepted alongside the header.
async fn csrf_ok(session: &Session, headers: &HeaderMap, body: &str) -> bool {
    crate::routes::helpers::require_csrf_header_or_field(session, headers, body)
        .await
        .is_ok()
}

/// Whether the caller wants JSON back rather than a redirect.
fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| !value.starts_with("application/x-www-form-urlencoded"))
        .unwrap_or(true)
}

/// Where a no-JavaScript action goes back to.
fn back_to(conversation: &Conversation) -> String {
    match conversation.scope_id.as_deref() {
        Some(id) => format!("/ai/assistant/{}/{}", conversation.scope, id),
        None => format!("/ai/assistant/{}", conversation.scope),
    }
}

async fn apply_proposal(
    State(state): State<AppState>,
    session: Session,
    Path((conversation_id, proposal_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    resolve_proposal(
        state,
        session,
        conversation_id,
        proposal_id,
        headers,
        body,
        true,
    )
    .await
}

async fn discard_proposal(
    State(state): State<AppState>,
    session: Session,
    Path((conversation_id, proposal_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    resolve_proposal(
        state,
        session,
        conversation_id,
        proposal_id,
        headers,
        body,
        false,
    )
    .await
}

/// Apply or discard one proposal.
///
/// Neither runs a model turn. The note this appends is what carries the outcome
/// to the model, on the next message the person sends — which is also why
/// discarding is worth recording rather than silently dropping: the model has to
/// learn that its suggestion was refused, or it will make it again.
#[allow(clippy::too_many_arguments)]
async fn resolve_proposal(
    state: AppState,
    session: Session,
    conversation_id: Uuid,
    proposal_id: Uuid,
    headers: HeaderMap,
    body: axum::body::Bytes,
    apply: bool,
) -> Response {
    let body_str = std::str::from_utf8(&body).unwrap_or("");
    if !csrf_ok(&session, &headers, body_str).await {
        return AppError::forbidden("Invalid or missing CSRF token").into_response();
    }

    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return AppError::unauthorized("Authentication required").into_response();
    }
    let conversation = match owned_conversation(&state, &user, conversation_id).await {
        Ok(conversation) => conversation,
        Err(response) => return response,
    };

    let proposal = match Proposal::find_by_id(state.db(), proposal_id).await {
        Ok(Some(proposal))
            if proposal.conversation_id == conversation_id && proposal.user_id == user.id =>
        {
            proposal
        }
        // Somebody else's proposal is not a 403: a person who does not own it
        // has no business learning that it exists.
        Ok(_) => return AppError::not_found("Proposal").into_response(),
        Err(e) => return AppError::internal_ctx(e, "load proposal").into_response(),
    };

    if proposal.status != PROPOSAL_PROPOSED {
        return AppError::conflict(format!("This proposal was already {}.", proposal.status))
            .into_response();
    }

    let Some(scope) = state.assistant_scopes().get(&conversation.scope).cloned() else {
        return AppError::not_found("Assistant scope").into_response();
    };

    let (status, result_text, note) = if apply {
        execute_proposal(&state, &user, &scope, &conversation, &proposal).await
    } else {
        (
            PROPOSAL_DISCARDED.to_string(),
            None,
            format!("Discarded: {}", proposal.description),
        )
    };

    // The predicate inside `resolve` is the concurrency control: a second apply
    // finds the row already moved and is told so rather than executing twice.
    match Proposal::resolve(
        state.db(),
        proposal.id,
        &status,
        result_text.as_deref(),
        user.id,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            return AppError::conflict("This proposal was already resolved.").into_response();
        }
        Err(e) => return AppError::internal_ctx(e, "resolve proposal").into_response(),
    }

    let mut entries = conversation.entries();
    entries.push(TranscriptEntry::Note {
        text: note.clone(),
        ts: epoch_now(),
    });
    if let Err(e) = Conversation::save_transcript(
        state.db(),
        conversation.id,
        &entries,
        conversation.message_count,
        conversation.tokens_used,
    )
    .await
    {
        tracing::warn!(error = %e, "failed to note a proposal outcome in the transcript");
    }

    if wants_json(&headers) {
        let refreshed = Proposal::find_by_id(state.db(), proposal.id)
            .await
            .ok()
            .flatten();
        return Json(serde_json::json!({
            "proposal": refreshed,
            "note": note,
        }))
        .into_response();
    }
    Redirect::to(&back_to(&conversation)).into_response()
}

/// Carry a proposal out, and say what to record.
///
/// This is the **only** `Execute` dispatch of a write tool anywhere in the
/// kernel. A model cannot reach it; a person clicking Apply is what does.
async fn execute_proposal(
    state: &AppState,
    user: &UserContext,
    scope: &RegisteredScope,
    conversation: &Conversation,
    proposal: &Proposal,
) -> (String, Option<String>, String) {
    // The tool must still exist and must still be a write: a plugin that dropped
    // or repurposed a tool since the proposal was made must not have an old
    // proposal executed against the new meaning.
    let declared = scope.tool(&proposal.tool);
    let Some(declared) = declared.filter(|tool| tool.kind == AssistantToolKind::Write) else {
        let message = "This tool is no longer available.".to_string();
        return (
            PROPOSAL_FAILED.to_string(),
            Some(message.clone()),
            format!("Failed: {}. {message}", proposal.description),
        );
    };
    let _ = declared;

    let call = AssistantToolCall::new(
        conversation.scope.clone(),
        conversation.scope_id.clone(),
        proposal.tool.clone(),
        proposal.arguments.clone(),
        AssistantToolMode::Execute,
        user.id.to_string(),
    )
    .proposal(proposal.id.to_string());

    let result = dispatch_tool(state, user, &scope.plugin, &call).await;
    if result.ok {
        let summary = result.summary.clone().unwrap_or_else(|| {
            crate::services::ai_tools::truncate_on_char_boundary(&result.content, 500)
        });
        (
            PROPOSAL_APPLIED.to_string(),
            Some(summary.clone()),
            format!("Applied: {}. {summary}", proposal.description),
        )
    } else {
        let message = result.summary.clone().unwrap_or_else(|| {
            crate::services::ai_tools::truncate_on_char_boundary(&result.content, 500)
        });
        (
            PROPOSAL_FAILED.to_string(),
            Some(message.clone()),
            format!("Failed: {}. {message}", proposal.description),
        )
    }
}

// =============================================================================
// Start over
// =============================================================================

async fn reset_conversation(
    State(state): State<AppState>,
    session: Session,
    Path(conversation_id): Path<Uuid>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let body_str = std::str::from_utf8(&body).unwrap_or("");
    if !csrf_ok(&session, &headers, body_str).await {
        return AppError::forbidden("Invalid or missing CSRF token").into_response();
    }

    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return AppError::unauthorized("Authentication required").into_response();
    }
    let conversation = match owned_conversation(&state, &user, conversation_id).await {
        Ok(conversation) => conversation,
        Err(response) => return response,
    };
    let config = match state.ai_assistant().load_config().await {
        Ok(config) => config,
        Err(e) => return AppError::internal_ctx(e, "load assistant configuration").into_response(),
    };
    let Some(scope) = state.assistant_scopes().get(&conversation.scope).cloned() else {
        return AppError::not_found("Assistant scope").into_response();
    };

    // A proposal whose conversation is gone can never be applied, so leaving it
    // `proposed` would be a lie the page would keep showing.
    if let Err(e) = Proposal::discard_open(state.db(), conversation.id, user.id).await {
        tracing::warn!(error = %e, "failed to discard open proposals on reset");
    }
    if conversation.is_open()
        && let Err(e) = Conversation::close(state.db(), conversation.id).await
    {
        return AppError::internal_ctx(e, "close conversation").into_response();
    }

    // A fresh snapshot, not a copy: the point of starting over is usually that
    // the thing has changed.
    let fresh = match create_conversation(
        &state,
        &user,
        &scope,
        conversation.scope_id.as_deref(),
        &config,
    )
    .await
    {
        Ok(fresh) => fresh,
        Err(e) => return AppError::internal_ctx(e, "create conversation").into_response(),
    };

    if wants_json(&headers) {
        return Json(serde_json::json!({
            "id": fresh.id,
            "title": fresh.title,
            "snapshot": fresh.snapshot,
        }))
        .into_response();
    }
    Redirect::to(&back_to(&fresh)).into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the assistant's page and API routes.
pub fn router() -> Router<AppState> {
    let message_route = post(post_message).layer(DefaultBodyLimit::max(MAX_BODY_BYTES));

    Router::new()
        .route("/ai/assistant/{scope}", get(conversation_page_no_id))
        .route(
            "/ai/assistant/{scope}/{scope_id}",
            get(conversation_page_with_id),
        )
        .route(
            "/api/v1/assistant/{conversation_id}",
            get(conversation_json),
        )
        .route("/api/v1/assistant/{conversation_id}/message", message_route)
        .route(
            "/api/v1/assistant/{conversation_id}/proposals/{proposal_id}/apply",
            post(apply_proposal),
        )
        .route(
            "/api/v1/assistant/{conversation_id}/proposals/{proposal_id}/discard",
            post(discard_proposal),
        )
        .route(
            "/api/v1/assistant/{conversation_id}/reset",
            post(reset_conversation),
        )
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn conversation(message_count: i32, tokens_used: i64, created: i64) -> Conversation {
        Conversation {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            plugin: "p".into(),
            scope: "s".into(),
            scope_id: Some("7".into()),
            title: "t".into(),
            status: STATUS_OPEN.into(),
            snapshot: String::new(),
            links: serde_json::json!([]),
            transcript: serde_json::json!([]),
            message_count,
            tokens_used,
            created,
            changed: created,
        }
    }

    #[test]
    fn a_conversation_goes_read_only_for_four_separate_reasons() {
        let config = AssistantConfig {
            max_messages: 4,
            max_tokens_per_conversation: 1_000,
            conversation_ttl_hours: 24,
            ..AssistantConfig::default()
        };
        let now = epoch_now();

        assert!(read_only_reason(&conversation(1, 10, now), &config).is_none());

        let full = read_only_reason(&conversation(4, 10, now), &config).unwrap();
        assert!(full.contains("message limit"), "{full}");

        let spent = read_only_reason(&conversation(1, 1_000, now), &config).unwrap();
        assert!(spent.contains("token limit"), "{spent}");

        let old = read_only_reason(&conversation(1, 10, now - 25 * 3600), &config).unwrap();
        assert!(old.contains("older"), "{old}");

        let closed = Conversation {
            status: "closed".into(),
            ..conversation(1, 10, now)
        };
        let replaced = read_only_reason(&closed, &config).unwrap();
        assert!(replaced.contains("replaced"), "{replaced}");
    }

    #[test]
    fn a_zero_ttl_never_ages_a_conversation_out() {
        let config = AssistantConfig {
            conversation_ttl_hours: 0,
            ..AssistantConfig::default()
        };
        let ancient = conversation(1, 10, 0);
        assert!(read_only_reason(&ancient, &config).is_none());
    }

    #[test]
    fn the_form_path_redirects_and_everything_else_gets_json() {
        let mut form = HeaderMap::new();
        form.insert(
            axum::http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        assert!(!wants_json(&form));

        let mut json = HeaderMap::new();
        json.insert(
            axum::http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        assert!(wants_json(&json));

        // No content type at all: a fetch with an empty body and a CSRF header.
        assert!(wants_json(&HeaderMap::new()));
    }

    #[test]
    fn the_way_back_names_the_scope_and_its_id() {
        assert_eq!(back_to(&conversation(0, 0, 0)), "/ai/assistant/s/7");
        let site_wide = Conversation {
            scope_id: None,
            ..conversation(0, 0, 0)
        };
        assert_eq!(back_to(&site_wide), "/ai/assistant/s");
    }

    #[test]
    fn the_rate_limit_bites_and_zero_means_unlimited() {
        clear_assistant_rate_limits();
        assert!(check_rate_limit("someone", 2));
        assert!(check_rate_limit("someone", 2));
        assert!(!check_rate_limit("someone", 2));
        // Another person has their own counter.
        assert!(check_rate_limit("somebody-else", 2));
        // Zero is unlimited, however many have gone before.
        assert!(check_rate_limit("someone", 0));
        clear_assistant_rate_limits();
        assert!(check_rate_limit("someone", 2));
    }
}
