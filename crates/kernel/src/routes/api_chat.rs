//! Public API route for AI chat with SSE streaming.
//!
//! `POST /api/v1/chat` accepts a JSON body with a user message and returns
//! a Server-Sent Events stream of AI-generated tokens. Requires `use ai`
//! and `use ai chat` permissions.

use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use dashmap::DashMap;
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::routes::auth::SESSION_USER_ID;
use crate::routes::helpers::{JsonError, require_csrf_header};
use crate::services::ai_chat::{ChatRole, ChatStreamEvent, ChatTurn};
use crate::services::ai_token_budget::{BudgetAction, UsageLogEntry};
use crate::state::AppState;

// =============================================================================
// Rate limiter
// =============================================================================

/// Per-user rate limit state: `(count, window_start)`.
///
/// **Limitation:** This is an in-process, non-persistent rate limiter. It
/// resets on server restart and is not shared across multiple instances.
/// For single-instance deployments this is adequate; clustered deployments
/// should add a Redis-backed rate limiter.
static CHAT_RATE_LIMITS: std::sync::LazyLock<DashMap<String, (u32, Instant)>> =
    std::sync::LazyLock::new(DashMap::new);

/// Timestamp of the last eviction pass.
static LAST_EVICTION: std::sync::LazyLock<std::sync::Mutex<Instant>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Instant::now()));

/// Check and increment the rate counter for a user.
///
/// Returns `true` if the request is allowed, `false` if rate limited.
fn check_chat_rate_limit(user_key: &str, limit_per_hour: u32) -> bool {
    if limit_per_hour == 0 {
        return true;
    }

    let now = Instant::now();

    // Evict stale entries every 60 seconds (avoids unbounded growth without
    // the old approach of only evicting above 100 entries).
    if let Ok(mut last) = LAST_EVICTION.lock()
        && now.duration_since(*last) > Duration::from_secs(60)
    {
        CHAT_RATE_LIMITS.retain(|_, v| now.duration_since(v.1) < Duration::from_secs(3600));
        *last = now;
    }

    let mut entry = CHAT_RATE_LIMITS
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

/// Clear all rate limit state.
///
/// This is `pub` (but `#[doc(hidden)]`) solely for integration tests in the
/// separate `tests/` crate. It has no side effects beyond resetting the
/// in-memory counter. Not part of the public API — do not call from
/// production code.
#[doc(hidden)]
pub fn clear_chat_rate_limits() {
    CHAT_RATE_LIMITS.clear();
}

// =============================================================================
// Request types
// =============================================================================

/// Chat request body.
#[derive(Debug, Deserialize)]
pub struct ChatInput {
    /// User's chat message.
    pub message: String,
}

/// Session key for conversation history.
const CHAT_HISTORY_KEY: &str = "chat_history";

/// Maximum length for assistant messages stored in session history.
const MAX_ASSISTANT_MESSAGE_LEN: usize = 32_768;

// =============================================================================
// Handler
// =============================================================================

/// POST /api/v1/chat — streaming chat endpoint.
///
/// Returns an SSE stream of token events. Requires authentication with
/// `use ai` and `use ai chat` permissions.
async fn chat_handler(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(input): Json<ChatInput>,
) -> Response {
    // CSRF check
    if let Err((status, json)) = require_csrf_header(&session, &headers).await {
        return (status, json).into_response();
    }

    // Auth check: load user from session
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let Some(uid) = user_id else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(JsonError {
                error: "Authentication required".to_string(),
            }),
        )
            .into_response();
    };

    let user = match state.users().find_by_id(uid).await {
        Ok(Some(u)) if u.is_active() => u,
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(JsonError {
                    error: "Authentication required".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Permission check: use ai + use ai chat
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
            return (
                StatusCode::FORBIDDEN,
                Json(JsonError {
                    error: "Permission required: use ai chat".to_string(),
                }),
            )
                .into_response();
        }
    }

    // Validate input
    let message = input.message.trim().to_string();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(JsonError {
                error: "Message cannot be empty".to_string(),
            }),
        )
            .into_response();
    }
    if message.len() > 4096 {
        return (
            StatusCode::BAD_REQUEST,
            Json(JsonError {
                error: "Message too long (max 4096 characters)".to_string(),
            }),
        )
            .into_response();
    }

    // Load config
    let config = match state.ai_chat().load_config().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load chat config");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Failed to load chat configuration".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Rate limit check — placed after auth/validation so failed auth doesn't
    // consume rate tokens, but before provider resolution and budget check to
    // protect server resources from request floods. Rate tokens are consumed
    // even if the budget check later denies the request; this is intentional
    // since rate limiting protects server resources, not billing.
    let rate_key = uid.to_string();
    if !check_chat_rate_limit(&rate_key, config.rate_limit_per_hour) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(JsonError {
                error: "Rate limit exceeded. Please try again later.".to_string(),
            }),
        )
            .into_response();
    }

    // Resolve provider BEFORE streaming so we can check budget
    let resolved = match state.ai_chat().resolve_chat_provider().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to resolve chat provider");
            return (
                StatusCode::BAD_GATEWAY,
                Json(JsonError {
                    error: "Failed to connect to AI provider".to_string(),
                }),
            )
                .into_response();
        }
    };
    let provider_id = resolved.config.id.clone();

    // Budget enforcement (before streaming starts)
    match state
        .ai_budgets()
        .check_budget(state.db(), uid, &provider_id)
        .await
    {
        Ok(result) if !result.allowed => match result.action {
            BudgetAction::Deny | BudgetAction::Queue => {
                tracing::warn!(
                    user = %uid,
                    provider = %provider_id,
                    used = result.used,
                    limit = result.limit,
                    "AI chat token budget exceeded"
                );
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(JsonError {
                        error: "Token budget exceeded. Please try again later.".to_string(),
                    }),
                )
                    .into_response();
            }
            BudgetAction::Warn => {
                tracing::warn!(
                    user = %uid,
                    provider = %provider_id,
                    used = result.used,
                    limit = result.limit,
                    "AI chat token budget exceeded (warn mode, allowing)"
                );
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "failed to check AI chat budget, allowing request");
        }
        _ => {}
    }

    // Load conversation history from session
    let history: Vec<ChatTurn> = session
        .get(CHAT_HISTORY_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    // Trim history to max turns (each turn = user + assistant = 2 entries)
    let max_entries = (config.max_history_turns as usize) * 2;
    let history = if history.len() > max_entries {
        history[history.len() - max_entries..].to_vec()
    } else {
        history
    };

    // Build system prompt with site_name substitution
    let site_name = crate::models::SiteConfig::get(state.db(), "site_name")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "Trovato".to_string());
    let system_prompt = config.system_prompt.replace("{site_name}", &site_name);

    // RAG context
    let rag_context = state
        .ai_chat()
        .search_for_context(&message, &config, Some(uid))
        .await;

    // Build messages
    let messages = state
        .ai_chat()
        .build_messages(&system_prompt, &rag_context, &history, &message);

    // Save user message to session history BEFORE stream starts
    let mut updated_history = history.clone();
    updated_history.push(ChatTurn {
        role: ChatRole::User,
        content: message.clone(),
        timestamp: chrono::Utc::now().timestamp(),
    });
    if let Err(e) = session.insert(CHAT_HISTORY_KEY, &updated_history).await {
        tracing::warn!(error = %e, "failed to save user message to session history");
    }

    // Execute streaming request using the same resolved provider used for budget check (no TOCTOU).
    let (stream, meta) = match state
        .ai_chat()
        .execute_streaming(messages, &config, &resolved)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to start chat stream");
            return (
                StatusCode::BAD_GATEWAY,
                Json(JsonError {
                    error: "Failed to connect to AI provider".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Wrap the provider stream in our normalized SSE events.
    //
    // Client-disconnect detection: Axum drops the SSE response body when the
    // client disconnects, which cascades through async_stream → provider byte
    // stream → reqwest response. The TCP connection to the provider is closed,
    // stopping further token generation. No explicit abort logic is needed.
    let db = state.ai_chat().db().clone();
    let ai_budgets = state.ai_budgets().clone();
    let request_start = Instant::now();
    let stream_provider_id = meta.provider_id;
    let model = meta.model;
    let session_for_stream = session.clone();
    let max_history_turns = config.max_history_turns;

    let sse_stream = async_stream::stream! {
        use tokio_stream::StreamExt;
        let mut pinned = stream;
        let mut accumulated_text = String::new();

        while let Some(event) = pinned.as_mut().next().await {
            match event {
                ChatStreamEvent::Token(text) => {
                    // Accumulate for server-side history save, avoiding the
                    // race condition in client-side save_history (F7).
                    if accumulated_text.len() < MAX_ASSISTANT_MESSAGE_LEN {
                        accumulated_text.push_str(&text);
                    }
                    let data = serde_json::json!({"type": "token", "text": text});
                    // Infallible: serde_json::Value to string cannot fail.
                    let json_str = serde_json::to_string(&data).unwrap_or_default();
                    yield Ok::<_, Infallible>(Event::default().data(json_str));
                }
                ChatStreamEvent::Done { prompt_tokens, completion_tokens, total_tokens } => {
                    // Save assistant message to session history server-side.
                    let mut msg = std::mem::take(&mut accumulated_text);
                    if !msg.is_empty() {
                        if msg.len() > MAX_ASSISTANT_MESSAGE_LEN {
                            let mut end = MAX_ASSISTANT_MESSAGE_LEN;
                            while end > 0 && !msg.is_char_boundary(end) {
                                end -= 1;
                            }
                            msg.truncate(end);
                        }
                        let mut history: Vec<ChatTurn> = session_for_stream
                            .get(CHAT_HISTORY_KEY)
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        history.push(ChatTurn {
                            role: ChatRole::Assistant,
                            content: msg,
                            timestamp: chrono::Utc::now().timestamp(),
                        });
                        let max_entries = (max_history_turns as usize) * 2;
                        if history.len() > max_entries {
                            history = history[history.len() - max_entries..].to_vec();
                        }
                        if let Err(e) = session_for_stream.insert(CHAT_HISTORY_KEY, &history).await {
                            tracing::warn!(error = %e, "failed to save assistant message to session");
                        }
                    }

                    let latency_ms = request_start.elapsed().as_millis() as i64;

                    // Record usage
                    let entry = UsageLogEntry {
                        user_id: Some(uid),
                        plugin_name: "kernel_chat".to_string(),
                        provider_id: stream_provider_id.clone(),
                        operation: "Chat".to_string(),
                        model: model.clone(),
                        prompt_tokens: prompt_tokens.min(i32::MAX as u32) as i32,
                        completion_tokens: completion_tokens.min(i32::MAX as u32) as i32,
                        total_tokens: total_tokens.min(i32::MAX as u32) as i32,
                        latency_ms,
                    };
                    if let Err(e) = ai_budgets.record_usage(&db, entry).await {
                        tracing::warn!(error = %e, "failed to record chat usage");
                    }

                    // F6: Do NOT include assistant_message in the done event — the
                    // client JS already accumulates tokens in its own variable.
                    let data = serde_json::json!({
                        "type": "done",
                        "usage": {
                            "prompt_tokens": prompt_tokens,
                            "completion_tokens": completion_tokens,
                            "total_tokens": total_tokens,
                        },
                    });
                    // Infallible: serde_json::Value to string cannot fail.
                    let json_str = serde_json::to_string(&data).unwrap_or_default();
                    yield Ok::<_, Infallible>(Event::default().data(json_str));
                }
                ChatStreamEvent::Error(msg) => {
                    // Log the full provider error server-side but send a generic
                    // message to the client to avoid leaking provider details.
                    tracing::warn!(error = %msg, "AI chat stream error");
                    let data = serde_json::json!({
                        "type": "error",
                        "message": "An error occurred while generating the response."
                    });
                    // Infallible: serde_json::Value to string cannot fail.
                    let json_str = serde_json::to_string(&data).unwrap_or_default();
                    yield Ok::<_, Infallible>(Event::default().data(json_str));
                }
            }
        }
    };

    Sse::new(sse_stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the chat API router.
pub fn router() -> Router<AppState> {
    use axum::extract::DefaultBodyLimit;

    // The chat endpoint receives a JSON message (max 4096 chars) plus JSON
    // overhead. Cap at 8 KiB to prevent abuse while allowing legitimate payloads.
    let chat_route = post(chat_handler).layer(DefaultBodyLimit::max(8 * 1024));

    Router::new().route("/api/v1/chat", chat_route)
}
