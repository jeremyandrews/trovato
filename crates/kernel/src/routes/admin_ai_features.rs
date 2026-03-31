//! Per-AI-feature configuration admin routes.
//!
//! Provides GET/POST `/admin/config/ai/features` for enabling/disabling
//! individual AI operation types and configuring per-operation providers.

use axum::{
    Form, Router,
    extract::State,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::SiteConfig;
use crate::routes::helpers::{render_admin_template, render_server_error, require_admin};
use crate::state::AppState;

/// Site config key for AI feature toggles.
const AI_FEATURES_CONFIG_KEY: &str = "ai_features";

/// Per-operation AI feature configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiFeatureConfig {
    /// Whether this operation type is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Override provider for this operation (empty = use global default).
    #[serde(default)]
    pub provider: String,
    /// Override model for this operation (empty = use provider default).
    #[serde(default)]
    pub model: String,
}

fn default_true() -> bool {
    true
}

impl Default for AiFeatureConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: String::new(),
            model: String::new(),
        }
    }
}

/// All AI feature configurations keyed by operation type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiFeaturesConfig {
    #[serde(default)]
    pub chat: AiFeatureConfig,
    #[serde(default)]
    pub embedding: AiFeatureConfig,
    #[serde(default)]
    pub image_generation: AiFeatureConfig,
    #[serde(default)]
    pub speech_to_text: AiFeatureConfig,
    #[serde(default)]
    pub text_to_speech: AiFeatureConfig,
    #[serde(default)]
    pub moderation: AiFeatureConfig,
}

impl AiFeaturesConfig {
    /// Load from site config, returning defaults if not yet configured.
    pub async fn load(pool: &sqlx::PgPool) -> Self {
        match SiteConfig::get(pool, AI_FEATURES_CONFIG_KEY).await {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// Check if a specific operation type is enabled.
    pub fn is_enabled(&self, operation: &str) -> bool {
        match operation {
            "chat" => self.chat.enabled,
            "embedding" => self.embedding.enabled,
            "image_generation" => self.image_generation.enabled,
            "speech_to_text" => self.speech_to_text.enabled,
            "text_to_speech" => self.text_to_speech.enabled,
            "moderation" => self.moderation.enabled,
            _ => false,
        }
    }

    /// Get the provider override for an operation (empty string = use global).
    pub fn provider_for(&self, operation: &str) -> &str {
        match operation {
            "chat" => &self.chat.provider,
            "embedding" => &self.embedding.provider,
            "image_generation" => &self.image_generation.provider,
            "speech_to_text" => &self.speech_to_text.provider,
            "text_to_speech" => &self.text_to_speech.provider,
            "moderation" => &self.moderation.provider,
            _ => "",
        }
    }

    /// Get the model override for an operation (empty string = use provider default).
    pub fn model_for(&self, operation: &str) -> &str {
        match operation {
            "chat" => &self.chat.model,
            "embedding" => &self.embedding.model,
            "image_generation" => &self.image_generation.model,
            "speech_to_text" => &self.speech_to_text.model,
            "text_to_speech" => &self.text_to_speech.model,
            "moderation" => &self.moderation.model,
            _ => "",
        }
    }
}

/// Router for AI feature configuration.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/config/ai/features", get(show_form))
        .route("/admin/config/ai/features", post(save_form))
}

/// Form data for AI feature configuration.
#[derive(Debug, Deserialize)]
struct AiFeaturesForm {
    _token: String,
    chat_enabled: Option<String>,
    chat_provider: Option<String>,
    chat_model: Option<String>,
    embedding_enabled: Option<String>,
    embedding_provider: Option<String>,
    embedding_model: Option<String>,
    image_generation_enabled: Option<String>,
    image_generation_provider: Option<String>,
    image_generation_model: Option<String>,
    speech_to_text_enabled: Option<String>,
    speech_to_text_provider: Option<String>,
    speech_to_text_model: Option<String>,
    text_to_speech_enabled: Option<String>,
    text_to_speech_provider: Option<String>,
    text_to_speech_model: Option<String>,
    moderation_enabled: Option<String>,
    moderation_provider: Option<String>,
    moderation_model: Option<String>,
}

/// Display the AI features configuration form.
async fn show_form(State(state): State<AppState>, session: Session) -> Response {
    let _admin = match require_admin(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let config = AiFeaturesConfig::load(state.db()).await;
    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("config", &config);
    context.insert("csrf_token", &csrf_token);
    context.insert("page_title", "AI Feature Configuration");

    render_admin_template(&state, "admin/ai-features.html", context)
        .await
        .into_response()
}

/// Save the AI features configuration.
async fn save_form(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<AiFeaturesForm>,
) -> Response {
    let _admin = match require_admin(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    if let Err(resp) = crate::routes::helpers::require_csrf(&session, &form._token).await {
        return resp;
    }

    let config = AiFeaturesConfig {
        chat: AiFeatureConfig {
            enabled: form.chat_enabled.is_some(),
            provider: form.chat_provider.unwrap_or_default(),
            model: form.chat_model.unwrap_or_default(),
        },
        embedding: AiFeatureConfig {
            enabled: form.embedding_enabled.is_some(),
            provider: form.embedding_provider.unwrap_or_default(),
            model: form.embedding_model.unwrap_or_default(),
        },
        image_generation: AiFeatureConfig {
            enabled: form.image_generation_enabled.is_some(),
            provider: form.image_generation_provider.unwrap_or_default(),
            model: form.image_generation_model.unwrap_or_default(),
        },
        speech_to_text: AiFeatureConfig {
            enabled: form.speech_to_text_enabled.is_some(),
            provider: form.speech_to_text_provider.unwrap_or_default(),
            model: form.speech_to_text_model.unwrap_or_default(),
        },
        text_to_speech: AiFeatureConfig {
            enabled: form.text_to_speech_enabled.is_some(),
            provider: form.text_to_speech_provider.unwrap_or_default(),
            model: form.text_to_speech_model.unwrap_or_default(),
        },
        moderation: AiFeatureConfig {
            enabled: form.moderation_enabled.is_some(),
            provider: form.moderation_provider.unwrap_or_default(),
            model: form.moderation_model.unwrap_or_default(),
        },
    };

    let value = match serde_json::to_value(&config) {
        Ok(v) => v,
        Err(e) => return render_server_error(&format!("Failed to serialize config: {e}")),
    };

    if let Err(e) = SiteConfig::set(state.db(), AI_FEATURES_CONFIG_KEY, value).await {
        return render_server_error(&format!("Failed to save config: {e}"));
    }

    Redirect::to("/admin/config/ai/features").into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn default_config_all_enabled() {
        let config = AiFeaturesConfig::default();
        assert!(config.chat.enabled);
        assert!(config.embedding.enabled);
        assert!(config.image_generation.enabled);
        assert!(config.speech_to_text.enabled);
        assert!(config.text_to_speech.enabled);
        assert!(config.moderation.enabled);
    }

    #[test]
    fn is_enabled_checks_correct_field() {
        let mut config = AiFeaturesConfig::default();
        config.image_generation.enabled = false;

        assert!(config.is_enabled("chat"));
        assert!(!config.is_enabled("image_generation"));
        assert!(!config.is_enabled("unknown_operation"));
    }

    #[test]
    fn provider_override_empty_means_global() {
        let config = AiFeaturesConfig::default();
        assert!(config.provider_for("chat").is_empty());
        assert!(config.model_for("chat").is_empty());
    }

    #[test]
    fn serde_round_trip() {
        let mut config = AiFeaturesConfig::default();
        config.chat.provider = "anthropic".to_string();
        config.chat.model = "claude-sonnet-4-20250514".to_string();
        config.image_generation.enabled = false;

        let json = serde_json::to_value(&config).unwrap();
        let parsed: AiFeaturesConfig = serde_json::from_value(json).unwrap();

        assert_eq!(parsed.chat.provider, "anthropic");
        assert_eq!(parsed.chat.model, "claude-sonnet-4-20250514");
        assert!(!parsed.image_generation.enabled);
        assert!(parsed.embedding.enabled);
    }
}
