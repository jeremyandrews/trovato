//! Admin routes for AI chat configuration.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::services::ai_chat::ChatConfig;
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_error, render_server_error, require_csrf, require_permission,
};

/// Session key for flash messages.
const FLASH_KEY: &str = "ai_chat_flash";

// =============================================================================
// Form data
// =============================================================================

/// Chat configuration form.
#[derive(Debug, Deserialize)]
struct ChatConfigForm {
    #[serde(rename = "_token")]
    token: String,
    system_prompt: String,
    rag_enabled: Option<String>,
    rag_max_results: u32,
    rag_min_score: f32,
    max_history_turns: u32,
    rate_limit_per_hour: u32,
    max_tokens: u32,
    temperature: f32,
}

// =============================================================================
// Handlers
// =============================================================================

/// Chat configuration page.
///
/// GET /admin/system/ai-chat
async fn chat_config_page(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }

    let config = match state.ai_chat().load_config().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load chat config");
            return render_server_error("Failed to load chat configuration.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("config", &config);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/system/ai-chat");
    if let Some(flash) = flash {
        context.insert("flash", &flash);
    }

    render_admin_template(&state, "admin/ai-chat.html", context).await
}

/// Save chat configuration.
///
/// POST /admin/system/ai-chat
async fn save_chat_config(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ChatConfigForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    if form.system_prompt.trim().is_empty() {
        return render_error("System prompt cannot be empty.");
    }
    // Limit system prompt to 10,000 chars to prevent session/DB bloat.
    if form.system_prompt.len() > 10_000 {
        return render_error("System prompt is too long (max 10,000 characters).");
    }
    // f32::clamp() does not reject NaN/infinity — check explicitly.
    if !form.temperature.is_finite() {
        return render_error("Temperature must be a number.");
    }
    if !form.rag_min_score.is_finite() {
        return render_error("Minimum relevance score must be a number.");
    }

    let config = ChatConfig {
        system_prompt: form.system_prompt,
        rag_enabled: form.rag_enabled.is_some(),
        rag_max_results: form.rag_max_results.clamp(1, 20),
        rag_min_score: form.rag_min_score.clamp(0.0, 1.0),
        max_history_turns: form.max_history_turns.clamp(0, 20),
        rate_limit_per_hour: form.rate_limit_per_hour.clamp(0, 1000),
        max_tokens: form.max_tokens.clamp(64, 16384),
        temperature: form.temperature.clamp(0.0, 2.0),
    };

    if let Err(e) = state.ai_chat().save_config(&config).await {
        tracing::error!(error = %e, "failed to save chat config");
        return render_server_error("Failed to save chat configuration.");
    }

    let _ = session.insert(FLASH_KEY, "Chat configuration saved.").await;
    Redirect::to("/admin/system/ai-chat").into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the AI chat admin routes.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/admin/system/ai-chat",
        get(chat_config_page).post(save_chat_config),
    )
}
