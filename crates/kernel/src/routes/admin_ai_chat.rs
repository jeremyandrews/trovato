//! Admin routes for AI chat configuration.
//!
//! The configuration form goes through `FormService` (B83c).

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::models::User;
use crate::services::ai_chat::ChatConfig;
use crate::state::AppState;

use super::admin_form;
use super::admin_nav::AdminText;
use super::helpers::{
    parse_optional_temperature, render_admin_template, render_server_error, require_permission,
    user_context_for,
};

/// Session key for flash messages.
const FLASH_KEY: &str = "ai_chat_flash";

/// The page, where the form posts and the save returns.
const PATH: &str = "/admin/system/ai-chat";

/// The chat configuration form.
const CHAT_FORM_ID: &str = "ai_chat_config_form";

/// The line under an admin settings form that says what saving it changes,
/// in the page's language: `what` is the English rest of the sentence after
/// "What this changes:". The AI settings forms share it.
pub(super) fn effect(text: &AdminText, what: &str) -> FormElement {
    FormElement::markup(format!(
        r#"<p class="form__effect"><strong class="form__effect-label">{}</strong> {}</p>"#,
        text.html("What this changes:", &[]),
        text.html(what, &[])
    ))
}

/// A number field of the form, with its HTML bounds.
fn number(title: &str, value: String, min: &str, max: &str) -> FormElement {
    FormElement::input("number")
        .title(title)
        .default_value(value)
        .attribute("min", min)
        .attribute("max", max)
}

/// What the chat form shows: the stored configuration or what was posted,
/// each value as the field holds it.
struct ChatValues {
    system_prompt: String,
    rag_enabled: bool,
    rag_max_results: String,
    rag_min_score: String,
    max_history_turns: String,
    rate_limit_per_hour: String,
    max_tokens: String,
    temperature: String,
}

impl ChatValues {
    /// The values of the stored configuration.
    fn of(config: &ChatConfig) -> Self {
        Self {
            system_prompt: config.system_prompt.clone(),
            rag_enabled: config.rag_enabled,
            rag_max_results: config.rag_max_results.to_string(),
            rag_min_score: config.rag_min_score.to_string(),
            max_history_turns: config.max_history_turns.to_string(),
            rate_limit_per_hour: config.rate_limit_per_hour.to_string(),
            max_tokens: config.max_tokens.to_string(),
            temperature: config
                .temperature
                .map(|t| t.to_string())
                .unwrap_or_default(),
        }
    }

    /// The values a submission posted.
    fn posted(posted: &PostedForm) -> Self {
        let text = |name: &str| posted.get(name).unwrap_or_default().to_string();
        Self {
            system_prompt: text("system_prompt"),
            rag_enabled: posted.contains("rag_enabled"),
            rag_max_results: text("rag_max_results"),
            rag_min_score: text("rag_min_score"),
            max_history_turns: text("max_history_turns"),
            rate_limit_per_hour: text("rate_limit_per_hour"),
            max_tokens: text("max_tokens"),
            temperature: text("temperature"),
        }
    }
}

/// The chat configuration form.
fn chat_form(text: &AdminText, values: ChatValues) -> crate::form::Form {
    crate::form::Form::new(CHAT_FORM_ID)
        .action(PATH)
        .element(
            "system_prompt",
            FormElement::textarea(6)
                .title("System prompt")
                .description(
                    "Use {site_name} as a placeholder for the site name. This prompt is sent to \
                     the AI provider as context before user messages.",
                )
                .default_value(values.system_prompt)
                .weight(0),
        )
        .element(
            "rag",
            FormElement::fieldset()
                .title("RAG (Retrieval-Augmented Generation)")
                .child(
                    "rag_enabled",
                    FormElement::checkbox()
                        .title("Enable RAG context injection")
                        .description(
                            "When enabled, relevant site content is included as context for the \
                             AI to reference.",
                        )
                        .default_value(values.rag_enabled)
                        .weight(0),
                )
                .child(
                    "rag_max_results",
                    number("Max search results", values.rag_max_results, "1", "20").weight(1),
                )
                .child(
                    "rag_min_score",
                    number("Minimum relevance score", values.rag_min_score, "0", "1")
                        .description(
                            "Results below this score are excluded (0.0 = include all, 1.0 = \
                             only exact matches).",
                        )
                        .attribute("step", "0.01")
                        .weight(2),
                )
                .weight(1),
        )
        .element(
            "model",
            FormElement::fieldset()
                .title("AI Model")
                .child(
                    "max_tokens",
                    number("Max tokens", values.max_tokens, "64", "16384")
                        .description(
                            "Maximum number of tokens the AI can generate per response \
                             (64–16384).",
                        )
                        .weight(0),
                )
                .child(
                    "temperature",
                    number("Temperature", values.temperature, "0", "2")
                        .description(
                            "Controls randomness. Lower values (0.0) are more deterministic, \
                             higher values (2.0) are more creative. Leave blank to send none and \
                             let the model use its own default; current Anthropic models (newer \
                             than Claude Sonnet 4.6) refuse a request that sets one.",
                        )
                        .attribute("step", "0.1")
                        .weight(1),
                )
                .weight(2),
        )
        .element(
            "conversation",
            FormElement::fieldset()
                .title("Conversation")
                .child(
                    "max_history_turns",
                    number("Max history turns", values.max_history_turns, "0", "20").description(
                        "Number of previous conversation turns to include (0 = no history).",
                    ),
                )
                .weight(3),
        )
        .element(
            "rate_limiting",
            FormElement::fieldset()
                .title("Rate Limiting")
                .child(
                    "rate_limit_per_hour",
                    number(
                        "Requests per hour per user",
                        values.rate_limit_per_hour,
                        "0",
                        "1000",
                    )
                    .description("Maximum chat requests per user per hour (0 = unlimited)."),
                )
                .weight(4),
        )
        .element(
            "effect",
            effect(
                text,
                "what the site's AI chat tells the provider, how much site content it adds as \
                 context, and how often each person may ask, for every chat message from now on.",
            )
            .weight(9),
        )
        .element("op", FormElement::submit("Save configuration").weight(10))
}

/// What the chat form posted, checked: the configuration to save, or the
/// problems with it.
fn read_chat(posted: &PostedForm) -> Result<ChatConfig, Vec<ValidationError>> {
    let mut errors = Vec::new();
    let whole = |name: &str, errors: &mut Vec<ValidationError>| {
        let parsed = posted.get(name).unwrap_or_default().trim().parse::<u32>();
        if parsed.is_err() {
            errors.push(ValidationError::field(name, "Enter a whole number."));
        }
        parsed.unwrap_or_default()
    };

    let system_prompt = posted.get("system_prompt").unwrap_or_default().to_string();
    if system_prompt.trim().is_empty() {
        errors.push(ValidationError::field(
            "system_prompt",
            "System prompt cannot be empty.",
        ));
    } else if system_prompt.len() > 10_000 {
        // Limit system prompt to 10,000 chars to prevent session/DB bloat.
        errors.push(ValidationError::field(
            "system_prompt",
            "System prompt is too long (max 10,000 characters).",
        ));
    }
    let rag_max_results = whole("rag_max_results", &mut errors);
    let rag_min_score = match posted
        .get("rag_min_score")
        .unwrap_or_default()
        .trim()
        .parse::<f32>()
    {
        // f32::clamp() does not reject NaN/infinity — check explicitly.
        Ok(score) if score.is_finite() => score,
        _ => {
            errors.push(ValidationError::field(
                "rag_min_score",
                "Minimum relevance score must be a number.",
            ));
            0.0
        }
    };
    let max_history_turns = whole("max_history_turns", &mut errors);
    let rate_limit_per_hour = whole("rate_limit_per_hour", &mut errors);
    let max_tokens = whole("max_tokens", &mut errors);
    let temperature =
        match parse_optional_temperature(posted.get("temperature").unwrap_or_default()) {
            Ok(temperature) => temperature,
            Err(message) => {
                errors.push(ValidationError::field("temperature", message));
                None
            }
        };
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ChatConfig {
        system_prompt,
        rag_enabled: posted.contains("rag_enabled"),
        rag_max_results: rag_max_results.clamp(1, 20),
        rag_min_score: rag_min_score.clamp(0.0, 1.0),
        max_history_turns: max_history_turns.clamp(0, 20),
        rate_limit_per_hour: rate_limit_per_hour.clamp(0, 1000),
        max_tokens: max_tokens.clamp(64, 16384),
        temperature,
    })
}

// =============================================================================
// Handlers
// =============================================================================

/// Render the page with the chat form holding `values` and `errors`.
async fn render_page(
    state: &AppState,
    session: &Session,
    user: &User,
    values: ChatValues,
    errors: &[ValidationError],
) -> Response {
    let viewer = user_context_for(state, user).await;
    let text = AdminText::for_page(state, &viewer, PATH).await;
    let form_html =
        match admin_form::render(state, session, user, chat_form(&text, values), errors).await {
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

    render_admin_template(state, &viewer, "admin/ai-chat.html", context).await
}

/// Chat configuration page.
///
/// GET /admin/system/ai-chat
async fn chat_config_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let config = match state.ai_chat().load_config().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load chat config");
            return render_server_error("Failed to load chat configuration.");
        }
    };

    render_page(&state, &session, &user, ChatValues::of(&config), &[]).await
}

/// Save chat configuration.
///
/// POST /admin/system/ai-chat
async fn save_chat_config(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let parsed = read_chat(&posted);
    let errors = parsed.as_ref().err().cloned().unwrap_or_default();
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, CHAT_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_page(
                    &state,
                    &session,
                    &user,
                    ChatValues::posted(&posted),
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };
    let Ok(config) = parsed else {
        return super::helpers::render_not_found();
    };

    if let Err(e) = state.ai_chat().save_config(&config).await {
        tracing::error!(error = %e, "failed to save chat config");
        return render_server_error("Failed to save chat configuration.");
    }
    admin_form::submitted(&state, &user, &form_state, &posted).await;

    let _ = session.insert(FLASH_KEY, "Chat configuration saved.").await;
    Redirect::to(PATH).into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the AI chat admin routes.
pub fn router() -> Router<AppState> {
    Router::new().route(PATH, get(chat_config_page).post(save_chat_config))
}
