//! Admin page for the AI Assistant (0.102).
//!
//! One page, two halves. The top half is the site's settings: whether the
//! assistant is on at all, which provider and model it uses, and the limits that
//! bound what a conversation can cost. The bottom half is the scopes plugins
//! declared, each with a switch and a prompt override, plus the ones that were
//! **dropped** and why — a registry rejection is otherwise only visible in a
//! startup log nobody reads.

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::services::ai_assistant::{
    AssistantConfig, AssistantScopeConfig, DEFAULT_CORE_PROMPT, risk_name,
};
use crate::services::ai_provider::AiOperationType;
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_error, render_server_error, require_csrf, require_permission,
};

/// Session key for the flash message.
const FLASH_KEY: &str = "ai_assistant_flash";

/// The path, in one place, since the form posts back to it and the redirect
/// returns to it.
const PATH: &str = "/admin/system/ai-assistant";

/// The submitted settings, parsed by hand.
///
/// Hand-parsed rather than `Form<T>` because the per-scope fields are named
/// `scope_enabled[name]` and `scope_prompt[name]`, and `serde_urlencoded` has no
/// notion of bracket nesting: it would hand back a key called literally
/// `scope_prompt[test_widget]` and no map at all. The set of scopes is whatever
/// the plugins declared, so it cannot be a struct field either. One small parser
/// is cheaper than either workaround.
#[derive(Debug, Default)]
struct SubmittedConfig {
    fields: HashMap<String, String>,
    scope_enabled: HashMap<String, String>,
    scope_prompt: HashMap<String, String>,
}

impl SubmittedConfig {
    /// Parse an `application/x-www-form-urlencoded` body.
    fn parse(body: &str) -> Self {
        let mut submitted = Self::default();
        for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
            let key = key.into_owned();
            let value = value.into_owned();
            if let Some(name) = bracketed(&key, "scope_enabled") {
                submitted.scope_enabled.insert(name, value);
            } else if let Some(name) = bracketed(&key, "scope_prompt") {
                submitted.scope_prompt.insert(name, value);
            } else {
                submitted.fields.insert(key, value);
            }
        }
        submitted
    }

    fn text(&self, key: &str) -> &str {
        self.fields.get(key).map(String::as_str).unwrap_or_default()
    }

    fn present(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }

    /// A number, falling back to the current configuration's value when the
    /// field is missing or unreadable. Every value is clamped on save anyway, so
    /// this only has to be sane, not validated.
    fn number<T: std::str::FromStr>(&self, key: &str, fallback: T) -> T {
        self.text(key).trim().parse().unwrap_or(fallback)
    }
}

/// The `name` in `prefix[name]`, if this key has that shape.
fn bracketed(key: &str, prefix: &str) -> Option<String> {
    let rest = key.strip_prefix(prefix)?.strip_prefix('[')?;
    let name = rest.strip_suffix(']')?;
    (!name.is_empty()).then(|| name.to_string())
}

/// GET the page.
async fn assistant_config_page(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }

    let config = match state.ai_assistant().load_config().await {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = %e, "failed to load the assistant configuration");
            return render_server_error("Failed to load the assistant configuration.");
        }
    };

    // Enabled providers only: offering a disabled one would let a site pick
    // something that cannot answer.
    let providers: Vec<serde_json::Value> = state
        .ai_providers()
        .list_providers()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|provider| provider.enabled)
        .map(|provider| {
            serde_json::json!({
                "id": provider.id,
                "label": provider.label,
                "protocol": provider.protocol.to_string(),
            })
        })
        .collect();

    let scopes: Vec<serde_json::Value> = state
        .assistant_scopes()
        .scopes()
        .map(|registered| {
            let name = registered.scope.name.clone();
            let override_prompt = config
                .scopes
                .get(&name)
                .and_then(|s| s.prompt_override.clone())
                .unwrap_or_default();
            serde_json::json!({
                "name": name,
                "plugin": registered.plugin,
                "label": registered.scope.label,
                "description": registered.scope.description,
                "permission": registered.scope.permission,
                "tool_count": registered.scope.tools.len(),
                "write_tool_count": registered.write_tool_count(),
                "stock_prompt": registered.scope.prompt,
                "enabled": config.scope_enabled(&registered.scope.name),
                "prompt_override": override_prompt,
                "tools": registered
                    .scope
                    .tools
                    .iter()
                    .map(|tool| serde_json::json!({
                        "name": tool.name,
                        "kind": if tool.kind == trovato_sdk::types::AssistantToolKind::Write {
                            "write"
                        } else {
                            "read"
                        },
                        "risk": risk_name(tool.risk),
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    let rejections: Vec<serde_json::Value> = state
        .assistant_scopes()
        .rejections()
        .iter()
        .map(|rejection| {
            serde_json::json!({
                "plugin": rejection.plugin,
                "scope": rejection.scope,
                "reason": rejection.reason,
            })
        })
        .collect();

    let csrf_token = generate_csrf_token(&session).await;
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("config", &config);
    context.insert("providers", &providers);
    context.insert("scopes", &scopes);
    context.insert("rejections", &rejections);
    context.insert("chat_operation", &AiOperationType::Chat.to_string());
    context.insert("csrf_token", &csrf_token);
    context.insert("path", PATH);
    if let Some(flash) = flash {
        context.insert("flash", &flash);
    }

    render_admin_template(&state, "admin/ai-assistant.html", context).await
}

/// POST the page.
async fn save_assistant_config(
    State(state): State<AppState>,
    session: Session,
    body: axum::body::Bytes,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }
    let Ok(body) = std::str::from_utf8(&body) else {
        return render_error("The form could not be read.");
    };
    let submitted = SubmittedConfig::parse(body);

    if let Err(response) = require_csrf(&session, submitted.text("_token")).await {
        return response;
    }

    let existing = state.ai_assistant().load_config().await.unwrap_or_default();

    let temperature: f32 = submitted.number("temperature", existing.temperature);
    // f32::clamp does not reject NaN, so this is checked rather than clamped.
    if !temperature.is_finite() {
        return render_error("Temperature must be a number.");
    }
    let core_prompt = submitted.text("core_prompt").to_string();
    if core_prompt.len() > 20_000 {
        return render_error("The core prompt is too long (max 20,000 characters).");
    }

    // Every registered scope gets an entry, because an unchecked checkbox sends
    // nothing at all: without walking the registry, unchecking a scope would be
    // indistinguishable from not having a form field for it.
    let mut scopes: HashMap<String, AssistantScopeConfig> = HashMap::new();
    for registered in state.assistant_scopes().scopes() {
        let name = &registered.scope.name;
        let prompt_override = submitted
            .scope_prompt
            .get(name)
            .map(|prompt| prompt.trim())
            .filter(|prompt| !prompt.is_empty())
            .map(str::to_string);
        scopes.insert(
            name.clone(),
            AssistantScopeConfig {
                enabled: submitted.scope_enabled.contains_key(name),
                prompt_override,
            },
        );
    }
    // Keep settings for a scope whose plugin is currently disabled: it will be
    // back, and silently discarding its prompt override would be a surprise.
    for (name, scope) in existing.scopes.clone() {
        scopes.entry(name).or_insert(scope);
    }

    let resetting = submitted.present("reset_core_prompt");
    let core_prompt = if resetting {
        DEFAULT_CORE_PROMPT.to_string()
    } else {
        core_prompt
    };
    let provider_id = submitted.text("provider_id").trim().to_string();
    let model = submitted.text("model").trim().to_string();

    let config = AssistantConfig {
        enabled: submitted.present("enabled"),
        provider_id: (!provider_id.is_empty()).then_some(provider_id),
        model: (!model.is_empty()).then_some(model),
        temperature,
        turn_timeout_secs: submitted.number("turn_timeout_secs", existing.turn_timeout_secs),
        max_tool_calls_per_message: submitted.number(
            "max_tool_calls_per_message",
            existing.max_tool_calls_per_message,
        ),
        max_messages: submitted.number("max_messages", existing.max_messages),
        max_tokens_per_conversation: submitted.number(
            "max_tokens_per_conversation",
            existing.max_tokens_per_conversation,
        ),
        max_history_exchanges: submitted
            .number("max_history_exchanges", existing.max_history_exchanges),
        max_response_tokens: submitted.number("max_response_tokens", existing.max_response_tokens),
        snapshot_max_bytes: submitted.number("snapshot_max_bytes", existing.snapshot_max_bytes),
        tool_result_max_bytes: submitted
            .number("tool_result_max_bytes", existing.tool_result_max_bytes),
        rate_limit_per_hour: submitted.number("rate_limit_per_hour", existing.rate_limit_per_hour),
        conversation_ttl_hours: submitted
            .number("conversation_ttl_hours", existing.conversation_ttl_hours),
        core_prompt,
        scopes,
    };

    if let Err(e) = state.ai_assistant().save_config(&config).await {
        tracing::error!(error = %e, "failed to save the assistant configuration");
        return render_server_error("Failed to save the assistant configuration.");
    }

    let message = if resetting {
        "Core prompt reset to the stock text."
    } else {
        "Assistant configuration saved."
    };
    let _ = session.insert(FLASH_KEY, message).await;
    Redirect::to(PATH).into_response()
}

/// Build the assistant admin route.
pub fn router() -> Router<AppState> {
    Router::new().route(PATH, get(assistant_config_page).post(save_assistant_config))
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_keys_are_split_into_a_prefix_and_a_name() {
        assert_eq!(
            bracketed("scope_prompt[test_widget]", "scope_prompt"),
            Some("test_widget".to_string())
        );
        assert_eq!(bracketed("scope_prompt[]", "scope_prompt"), None);
        assert_eq!(bracketed("scope_prompt", "scope_prompt"), None);
        assert_eq!(bracketed("core_prompt", "scope_prompt"), None);
        // A key that starts the same way but is not bracketed stays a field.
        assert_eq!(bracketed("scope_prompts[x]", "scope_prompt"), None);
    }

    #[test]
    fn the_form_body_splits_into_fields_and_per_scope_maps() {
        // This is the shape `serde_urlencoded` cannot produce, which is why the
        // parser exists: bracket nesting, and a scope set nobody can name in a
        // struct.
        let body = "_token=abc&enabled=1&core_prompt=Hello+there\
                    &scope_enabled%5Btest_widget%5D=1\
                    &scope_prompt%5Btest_widget%5D=Only+say+no.\
                    &scope_prompt%5Bother%5D=";
        let submitted = SubmittedConfig::parse(body);

        assert_eq!(submitted.text("_token"), "abc");
        assert!(submitted.present("enabled"));
        assert!(!submitted.present("reset_core_prompt"));
        assert_eq!(submitted.text("core_prompt"), "Hello there");
        assert_eq!(
            submitted
                .scope_prompt
                .get("test_widget")
                .map(String::as_str),
            Some("Only say no.")
        );
        assert_eq!(
            submitted.scope_prompt.get("other").map(String::as_str),
            Some("")
        );
        assert!(submitted.scope_enabled.contains_key("test_widget"));
        assert!(!submitted.scope_enabled.contains_key("other"));
    }

    #[test]
    fn a_missing_or_unreadable_number_falls_back_to_what_is_configured() {
        let submitted = SubmittedConfig::parse("max_messages=not+a+number");
        assert_eq!(submitted.number("max_messages", 40_u32), 40);
        assert_eq!(submitted.number("absent", 12_u32), 12);
        assert_eq!(submitted.number("max_messages", 7_u32), 7);

        let submitted = SubmittedConfig::parse("max_messages=99");
        assert_eq!(submitted.number("max_messages", 40_u32), 99);
    }
}
