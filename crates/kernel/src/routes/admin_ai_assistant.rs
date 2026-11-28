//! Admin page for the AI Assistant (0.102).
//!
//! One page, two halves. The top half is the site's settings: whether the
//! assistant is on at all, which provider and model it uses, and the limits that
//! bound what a conversation can cost. The bottom half is the scopes plugins
//! declared, each with a switch and a prompt override, plus the ones that were
//! **dropped** and why — a registry rejection is otherwise only visible in a
//! startup log nobody reads.
//!
//! The page is one form built by `FormService` (B83c). The scopes half is a
//! template rendered into a markup element of it, because its fields are
//! named per scope (`scope_enabled[name]`) and labelled with each scope's own
//! name.

use std::collections::HashMap;

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::models::User;
use crate::services::ai_assistant::{
    AssistantConfig, AssistantScopeConfig, DEFAULT_CORE_PROMPT, risk_name,
};
use crate::services::ai_provider::AiOperationType;
use crate::state::AppState;

use super::admin_form;
use super::admin_nav::AdminText;
use super::helpers::{
    parse_optional_temperature, render_admin_template, render_server_error, require_permission,
    user_context_for,
};

/// Session key for the flash message.
const FLASH_KEY: &str = "ai_assistant_flash";

/// The path, in one place, since the form posts back to it and the redirect
/// returns to it.
const PATH: &str = "/admin/system/ai-assistant";

/// The assistant settings form.
const ASSISTANT_FORM_ID: &str = "ai_assistant_config_form";

/// The submitted settings, split by hand.
///
/// Split by hand rather than `Form<T>` because the per-scope fields are named
/// `scope_enabled[name]` and `scope_prompt[name]`, and `serde_urlencoded` has no
/// notion of bracket nesting: it would hand back a key called literally
/// `scope_prompt[test_widget]` and no map at all. The set of scopes is whatever
/// the plugins declared, so it cannot be a struct field either. One small
/// splitter is cheaper than either workaround.
#[derive(Debug, Default)]
struct SubmittedConfig {
    fields: HashMap<String, String>,
    scope_enabled: HashMap<String, String>,
    scope_prompt: HashMap<String, String>,
}

impl SubmittedConfig {
    /// Split the submitted name and value pairs.
    fn from_pairs(pairs: &[(String, String)]) -> Self {
        let mut submitted = Self::default();
        for (key, value) in pairs {
            if let Some(name) = bracketed(key, "scope_enabled") {
                submitted.scope_enabled.insert(name, value.clone());
            } else if let Some(name) = bracketed(key, "scope_prompt") {
                submitted.scope_prompt.insert(name, value.clone());
            } else {
                submitted.fields.insert(key.clone(), value.clone());
            }
        }
        submitted
    }

    /// Parse an `application/x-www-form-urlencoded` body.
    #[cfg(test)]
    fn parse(body: &str) -> Self {
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        Self::from_pairs(&pairs)
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

// =============================================================================
// The form
// =============================================================================

/// A number field of the form, with its HTML bounds.
fn number(title: &str, value: impl ToString, min: &str, max: &str) -> FormElement {
    FormElement::input("number")
        .title(title)
        .default_value(value.to_string())
        .attribute("min", min)
        .attribute("max", max)
}

/// The scopes half of the page (`admin/ai-assistant-scopes.html`) for
/// `config`, in the page's language. `None` (logged) when the template fails
/// to render.
fn scopes_html(state: &AppState, text: &AdminText, config: &AssistantConfig) -> Option<String> {
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

    let mut context = tera::Context::new();
    context.insert("scopes", &scopes);
    context.insert("rejections", &rejections);
    text.scope(|| {
        state
            .theme()
            .tera()
            .render("admin/ai-assistant-scopes.html", &context)
    })
    .map_err(|e| tracing::error!(error = %e, "failed to render the assistant scopes"))
    .ok()
}

/// The settings form for `config`, its temperature box holding
/// `temperature` (what was posted when the form is shown again), and the
/// scopes half already rendered.
async fn assistant_form(
    state: &AppState,
    text: &AdminText,
    config: &AssistantConfig,
    temperature: String,
    scopes: String,
) -> crate::form::Form {
    // Enabled providers only: offering a disabled one would let a site pick
    // something that cannot answer.
    let providers: Vec<(String, String)> = state
        .ai_providers()
        .list_providers()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|provider| provider.enabled)
        .map(|provider| {
            let label = format!("{} ({})", provider.label, provider.protocol);
            (provider.id, label)
        })
        .collect();

    let model = FormElement::fieldset()
        .title("Model")
        .child(
            "provider_id",
            FormElement::select(providers)
                .title("Provider")
                .description("Only enabled providers are listed.")
                .placeholder(format!("Site default for {}", AiOperationType::Chat))
                .default_value(config.provider_id.clone().unwrap_or_default())
                .suffix(format!(
                    r#"<p><a href="/admin/system/ai-providers">{}</a></p>"#,
                    text.html("Manage providers", &[])
                ))
                .weight(0),
        )
        .child(
            "model",
            FormElement::textfield()
                .title("Model")
                .description(
                    "Leave blank to use whatever the provider is configured to use for chat.",
                )
                .default_value(config.model.clone().unwrap_or_default())
                .attribute("spellcheck", "false")
                .weight(1),
        )
        .child(
            "temperature",
            number("Temperature", temperature, "0", "2")
                .description(
                    "Leave blank to send none and let the model use its own default. Current \
                     Anthropic models (newer than Claude Sonnet 4.6) refuse a request that sets \
                     one. Clamped to 0.0–2.0.",
                )
                .attribute("step", "0.1")
                .weight(2),
        )
        .child(
            "max_response_tokens",
            number(
                "Max response tokens",
                config.max_response_tokens,
                "64",
                "16384",
            )
            .weight(3),
        )
        .weight(1);

    let limits = FormElement::fieldset_collapsible(true)
        .title("Limits")
        .child(
            "turn_timeout_secs",
            number(
                "Model call timeout (seconds)",
                config.turn_timeout_secs,
                "5",
                "150",
            )
            .description("One message gets three times this in total, however many calls it takes.")
            .weight(0),
        )
        .child(
            "max_tool_calls_per_message",
            number(
                "Tool calls per message",
                config.max_tool_calls_per_message,
                "1",
                "32",
            )
            .weight(1),
        )
        .child(
            "max_messages",
            number("Messages per conversation", config.max_messages, "4", "400")
                .description(
                    "At the limit the conversation goes read-only. Start over opens a fresh one.",
                )
                .weight(2),
        )
        .child(
            "max_tokens_per_conversation",
            number(
                "Tokens per conversation",
                config.max_tokens_per_conversation,
                "1000",
                "2000000",
            )
            .weight(3),
        )
        .child(
            "max_history_exchanges",
            number(
                "History exchanges per request",
                config.max_history_exchanges,
                "1",
                "100",
            )
            .description("Older exchanges are dropped from the request; the transcript keeps them.")
            .weight(4),
        )
        .child(
            "snapshot_max_bytes",
            number(
                "Snapshot size (bytes)",
                config.snapshot_max_bytes,
                "1024",
                "32768",
            )
            .weight(5),
        )
        .child(
            "tool_result_max_bytes",
            number(
                "Tool result size (bytes)",
                config.tool_result_max_bytes,
                "1024",
                "32768",
            )
            .weight(6),
        )
        .child(
            "rate_limit_per_hour",
            number(
                "Messages per person per hour",
                config.rate_limit_per_hour,
                "0",
                "1000",
            )
            .description("0 is unlimited.")
            .weight(7),
        )
        .child(
            "conversation_ttl_hours",
            number(
                "Conversation lifetime (hours)",
                config.conversation_ttl_hours,
                "0",
                "8760",
            )
            .description("After this a conversation is read-only. 0 never expires one.")
            .weight(8),
        )
        .weight(2);

    let core = FormElement::fieldset_collapsible(true)
        .title("Core prompt")
        .child(
            "core_prompt",
            FormElement::textarea(14)
                .title("Sent before every scope's own prompt")
                .description(
                    "This is what tells the model it must propose rather than act. Change the \
                     wording freely; changing the intent will let a model claim it made changes \
                     it did not make.",
                )
                .default_value(config.core_prompt.clone()),
        )
        .weight(3);

    crate::form::Form::new(ASSISTANT_FORM_ID)
        .action(PATH)
        .element(
            "enabled",
            FormElement::checkbox()
                .title("Enable the assistant")
                .description("Off by default. While off, every assistant page is a 404.")
                .default_value(config.enabled)
                .weight(0),
        )
        .element("model_settings", model)
        .element("limits", limits)
        .element("core", core)
        .element("scopes", FormElement::markup(scopes).weight(4))
        .element(
            "effect",
            super::admin_ai_chat::effect(
                text,
                "which model the assistant calls and how far one conversation may go, for every \
                 conversation started or continued after you save. Each change the assistant \
                 wants still waits as a proposal for someone to apply.",
            )
            .weight(9),
        )
        .element(
            "op",
            FormElement::submit("Save configuration")
                .suffix(format!(
                    r#"<button type="submit" name="reset_core_prompt" value="1" class="button button--secondary">{}</button>"#,
                    text.html("Reset core prompt", &[])
                ))
                .weight(10),
        )
}

// =============================================================================
// Handlers
// =============================================================================

/// Render the page with the form for `config`, its temperature box holding
/// `temperature`, and `errors`.
async fn render_page(
    state: &AppState,
    session: &Session,
    user: &User,
    config: &AssistantConfig,
    temperature: String,
    errors: &[ValidationError],
) -> Response {
    let viewer = user_context_for(state, user).await;
    let text = AdminText::for_page(state, &viewer, PATH).await;
    let Some(scopes) = scopes_html(state, &text, config) else {
        return render_server_error("The form could not be displayed.");
    };
    let form = assistant_form(state, &text, config, temperature, scopes).await;
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("form", &form_html);
    context.insert("path", PATH);
    if let Some(flash) = flash {
        context.insert("flash", &flash);
    }

    render_admin_template(state, &viewer, "admin/ai-assistant.html", context).await
}

/// GET the page.
async fn assistant_config_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let config = match state.ai_assistant().load_config().await {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = %e, "failed to load the assistant configuration");
            return render_server_error("Failed to load the assistant configuration.");
        }
    };
    let temperature = config
        .temperature
        .map(|t| t.to_string())
        .unwrap_or_default();
    render_page(&state, &session, &user, &config, temperature, &[]).await
}

/// POST the page.
async fn save_assistant_config(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let submitted = SubmittedConfig::from_pairs(&posted.0);

    let existing = state.ai_assistant().load_config().await.unwrap_or_default();

    let mut errors = Vec::new();
    // Blank is unset, not "keep the old value": clearing the box is how a site
    // stops sending a temperature to a model that refuses one.
    let temperature = match parse_optional_temperature(submitted.text("temperature")) {
        Ok(temperature) => temperature,
        Err(message) => {
            errors.push(ValidationError::field("temperature", message));
            None
        }
    };
    let core_prompt = submitted.text("core_prompt").to_string();
    if core_prompt.len() > 20_000 {
        errors.push(ValidationError::field(
            "core_prompt",
            "The core prompt is too long (max 20,000 characters).",
        ));
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

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        ASSISTANT_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            let temperature = submitted.text("temperature").to_string();
            return render_page(&state, &session, &user, &config, temperature, &errors).await;
        }
        Err(Err(response)) => return response,
    };

    if let Err(e) = state.ai_assistant().save_config(&config).await {
        tracing::error!(error = %e, "failed to save the assistant configuration");
        return render_server_error("Failed to save the assistant configuration.");
    }
    admin_form::submitted(&state, &user, &form_state, &posted).await;

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
