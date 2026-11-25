//! AI provider registry for managing LLM and AI service configurations.
//!
//! Stores provider configurations in `site_config` (JSONB). API keys are
//! referenced by environment variable name only — the actual key value is
//! resolved at runtime via `std::env::var` and never persisted in the database
//! or exposed to templates.
//!
//! ## Security
//!
//! - API key env var names are validated against an allowlist pattern and a
//!   denylist of known-sensitive process variables.
//! - Base URLs are validated for scheme (http/https only) and blocked from
//!   targeting private/link-local network ranges (SSRF prevention).
//! - CRUD operations on `site_config` are serialized via a `tokio::sync::Mutex`
//!   to prevent lost updates from concurrent read-modify-write cycles.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::circuit_breaker::{BreakerConfig, CircuitBreaker};
use crate::models::SiteConfig;
use crate::net_policy::is_disallowed_ip;
use crate::services::ai_chat::ChatStreamEvent;
use crate::services::ai_token_budget::{AiTokenBudgetService, BudgetAction, UsageLogEntry};
use crate::services::ai_tools::{self, ChatCompletion, ChatMessage, ToolSpec};

// =============================================================================
// Data types
// =============================================================================

/// The kind of AI operation a provider can serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiOperationType {
    /// Conversational / completion.
    Chat,
    /// Text embedding.
    Embedding,
    /// Image generation.
    ImageGeneration,
    /// Speech-to-text transcription.
    SpeechToText,
    /// Text-to-speech synthesis.
    TextToSpeech,
    /// Content moderation.
    Moderation,
}

impl AiOperationType {
    /// All known operation types.
    pub const ALL: &'static [Self] = &[
        Self::Chat,
        Self::Embedding,
        Self::ImageGeneration,
        Self::SpeechToText,
        Self::TextToSpeech,
        Self::Moderation,
    ];

    /// Stable snake_case key for this operation, matching the serde
    /// representation. Used to key per-operation timeout / breaker-window config
    /// (P11c / D-43) without a serde round-trip.
    pub fn config_key(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
            Self::ImageGeneration => "image_generation",
            Self::SpeechToText => "speech_to_text",
            Self::TextToSpeech => "text_to_speech",
            Self::Moderation => "moderation",
        }
    }
}

impl fmt::Display for AiOperationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chat => write!(f, "Chat"),
            Self::Embedding => write!(f, "Embedding"),
            Self::ImageGeneration => write!(f, "Image Generation"),
            Self::SpeechToText => write!(f, "Speech to Text"),
            Self::TextToSpeech => write!(f, "Text to Speech"),
            Self::Moderation => write!(f, "Moderation"),
        }
    }
}

/// Wire protocol spoken by the provider endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    /// OpenAI-compatible REST API (covers OpenAI, Azure, Ollama, vLLM).
    OpenAiCompatible,
    /// Anthropic Messages API.
    Anthropic,
}

impl fmt::Display for ProviderProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenAiCompatible => write!(f, "OpenAI Compatible"),
            Self::Anthropic => write!(f, "Anthropic"),
        }
    }
}

/// A model bound to a specific operation type within a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationModel {
    /// Which operation this model serves.
    pub operation: AiOperationType,
    /// Model identifier (e.g. "gpt-4o", "claude-sonnet-4-20250514").
    pub model: String,
}

/// Persisted provider configuration (stored as JSONB in `site_config`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
    /// Unique identifier (UUID).
    pub id: String,
    /// Human-readable label (e.g. "OpenAI Production").
    pub label: String,
    /// Wire protocol.
    pub protocol: ProviderProtocol,
    /// Base URL for the API (e.g. `https://api.openai.com/v1`).
    pub base_url: String,
    /// Name of the environment variable that holds the API key.
    pub api_key_env: String,
    /// Models available on this provider, keyed by operation type.
    pub models: Vec<OperationModel>,
    /// Rate limit in requests per minute (0 = unlimited).
    pub rate_limit_rpm: u32,
    /// Whether this provider is active.
    pub enabled: bool,
}

/// Default provider assignments: operation type → provider ID.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiDefaults {
    /// Map from operation type to the provider ID that serves it.
    #[serde(flatten)]
    pub defaults: HashMap<AiOperationType, String>,
}

/// A provider config with its API key resolved at runtime.
///
/// This type is intentionally **not** `Serialize` — the resolved key must
/// never be persisted or sent across the WASM boundary.
pub struct ResolvedProvider {
    /// The full provider configuration.
    pub config: AiProviderConfig,
    /// The resolved API key (from the environment variable), if set.
    pub api_key: Option<String>,
    /// The model identifier for the requested operation.
    pub model: String,
}

/// The result of an embedding request: the vector plus the model that
/// produced it.
///
/// The model is carried alongside the vector so that callers that store and
/// later search embeddings use the identical model string — `item_embeddings`
/// similarity search filters by model, so a mismatch silently yields no
/// results. Returning the model from the single resolution point (`embed`)
/// makes write-time and read-time agree by construction.
#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    /// The embedding vector.
    pub vector: Vec<f32>,
    /// The model identifier that was requested and that keys the stored
    /// vector.
    pub model: String,
    /// The model the provider reports having served (an alias may resolve to
    /// a dated name); the requested model when the response names none.
    pub served_model: String,
    /// Input tokens the provider reported.
    pub prompt_tokens: u32,
    /// Total tokens the provider reported (the input tokens when it reported
    /// no total).
    pub total_tokens: u32,
    /// Round-trip latency in milliseconds.
    pub latency_ms: u64,
    /// The estimated cost the send path recorded for the call; `None` for an
    /// unpriced model.
    pub cost_estimate: Option<f64>,
}

/// Who an AI call is made for.
///
/// Selects the timeout plane (request or background) and, from the principal,
/// the budget the call is held to and the `ai_usage_log` row it writes.
#[derive(Debug, Clone)]
pub struct AiCaller {
    /// What the usage log records as the caller (`ai_usage_log.plugin_name`):
    /// a plugin's name, or a `kernel_*` name for the kernel's own features.
    pub source: String,
    /// Whose call it is.
    pub principal: AiPrincipal,
    /// Whether the call runs off the request path, which selects the
    /// background timeout.
    pub background: bool,
}

/// Whose call an AI call is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiPrincipal {
    /// A signed-in person: held to their per-user or per-role budget.
    User(uuid::Uuid),
    /// An anonymous visitor, or work done on one's behalf.
    Anonymous,
    /// A plugin's background call (cron, queue worker): held to the plugin's
    /// background token and currency budgets.
    Plugin,
    /// The kernel's own housekeeping, such as indexing an item's embedding.
    Kernel,
}

impl AiCaller {
    /// A call made for a person; the nil id (the anonymous user) is anonymous.
    pub fn user(user_id: uuid::Uuid, source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            principal: if user_id.is_nil() {
                AiPrincipal::Anonymous
            } else {
                AiPrincipal::User(user_id)
            },
            background: false,
        }
    }

    /// A call made for an anonymous visitor.
    pub fn anonymous(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            principal: AiPrincipal::Anonymous,
            background: false,
        }
    }

    /// A plugin's background call.
    pub fn plugin_background(plugin: impl Into<String>) -> Self {
        Self {
            source: plugin.into(),
            principal: AiPrincipal::Plugin,
            background: true,
        }
    }

    /// The kernel's own work, on or off the request path.
    pub fn kernel(source: impl Into<String>, background: bool) -> Self {
        Self {
            source: source.into(),
            principal: AiPrincipal::Kernel,
            background,
        }
    }

    /// The same caller, off the request path.
    pub fn in_background(mut self) -> Self {
        self.background = true;
        self
    }

    /// The user id the usage log records: the person's, the nil id for an
    /// anonymous visitor (so the site-level anonymous cap can meter it), and
    /// none for a plugin's background call or the kernel's own work.
    pub fn user_id(&self) -> Option<uuid::Uuid> {
        match self.principal {
            AiPrincipal::User(id) => Some(id),
            AiPrincipal::Anonymous => Some(uuid::Uuid::nil()),
            AiPrincipal::Plugin | AiPrincipal::Kernel => None,
        }
    }
}

/// One chat completion request, in the protocol-neutral shape the send path
/// turns into either provider's wire format.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    /// The conversation.
    pub messages: Vec<ChatMessage>,
    /// Tools offered to the model; empty sends no tools at all.
    pub tools: Vec<ToolSpec>,
    /// Most tokens to generate. `None` omits it for an OpenAI-compatible
    /// provider; Anthropic requires one and gets 1024.
    pub max_tokens: Option<u32>,
    /// Sent only when set: Anthropic models newer than `claude-sonnet-4-6`
    /// answer 400 to a request that carries `temperature` at all.
    pub temperature: Option<f32>,
    /// Nucleus sampling, sent only when set.
    pub top_p: Option<f32>,
    /// Stop sequences, sent only when there are any.
    pub stop: Vec<String>,
    /// A timeout the calling feature configures for itself (the AI
    /// Assistant's per-call timeout). `None` takes the `ai_timeouts` value.
    pub timeout: Option<Duration>,
}

impl ChatRequest {
    /// A system prompt and one user message.
    pub fn system_and_user(system: &str, user: &str, max_tokens: u32) -> Self {
        Self {
            messages: vec![
                ChatMessage::System(system.to_string()),
                ChatMessage::User(user.to_string()),
            ],
            max_tokens: Some(max_tokens),
            ..Self::default()
        }
    }
}

/// A started chat stream.
pub type ChatEventStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = ChatStreamEvent> + Send>>;

/// Why an AI call failed, at the granularity a caller needs to say something
/// useful. The `Display` text never carries the provider's response body:
/// that goes to the log, and [`Self::user_message`] is what a person sees.
#[derive(Debug, thiserror::Error)]
pub enum AiCallError {
    /// The provider's base URL targets an address the kernel refuses.
    #[error("the AI provider's base URL is refused")]
    BaseUrlRefused,
    /// The provider rejected our credentials (401/403).
    #[error("the AI provider rejected the site's credentials")]
    Unauthorized,
    /// The provider rate-limited us (429).
    #[error("the AI provider is rate limiting this site")]
    RateLimited,
    /// Any other non-2xx status.
    #[error("the AI provider returned HTTP {status}")]
    Provider {
        /// The HTTP status the provider returned.
        status: u16,
        /// A truncated copy of the body, for the log only.
        detail: String,
    },
    /// The call did not complete within the timeout.
    #[error("the AI provider did not answer within the timeout")]
    Timeout,
    /// The circuit breaker for this operation is open.
    #[error("the AI provider circuit breaker is open")]
    BreakerOpen,
    /// The transport failed.
    #[error("the request to the AI provider failed: {0}")]
    Transport(String),
    /// The response was not the shape we parse.
    #[error("the AI provider's response could not be read: {0}")]
    Malformed(String),
    /// The provider cannot serve this operation.
    #[error("{0}")]
    Unsupported(String),
    /// The caller's token or currency budget for this period is spent.
    #[error("the AI budget for this period is spent")]
    BudgetExceeded,
}

impl AiCallError {
    /// A short, provider-agnostic sentence for the person. The detail belongs
    /// in the log, never here.
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::Unauthorized => "The model provider refused the request.",
            Self::RateLimited => "The model provider is busy. Try again shortly.",
            Self::Timeout => "The model took too long to answer.",
            Self::BreakerOpen | Self::BaseUrlRefused | Self::Unsupported(_) => {
                "The model provider is unavailable right now."
            }
            Self::Provider { .. } | Self::Transport(_) | Self::Malformed(_) => {
                "Something went wrong talking to the model provider."
            }
            Self::BudgetExceeded => "The AI usage limit for this period has been reached.",
        }
    }
}

/// Longest slice of a provider's error body kept in an [`AiCallError`].
const ERROR_DETAIL_MAX: usize = 500;

/// Send a provider request inside the telemetry span, naming the provider and
/// model on it.
async fn send_traced(
    builder: reqwest::RequestBuilder,
    resolved: &ResolvedProvider,
) -> reqwest::Result<reqwest::Response> {
    let provider = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => "openai_compatible",
        ProviderProtocol::Anthropic => "anthropic",
    };
    crate::telemetry::send_with(builder, |span| {
        span.record("gen_ai.provider.name", provider);
        span.record("gen_ai.request.model", resolved.model.as_str());
    })
    .await
}

/// A transport error, with a timeout told apart.
fn transport_error(e: reqwest::Error) -> AiCallError {
    if e.is_timeout() {
        AiCallError::Timeout
    } else {
        AiCallError::Transport(e.to_string())
    }
}

/// Pass a 2xx response through; log anything else with the provider's own
/// explanation and turn it into an [`AiCallError`].
async fn check_status(
    response: reqwest::Response,
    operation: &str,
    resolved: &ResolvedProvider,
) -> Result<reqwest::Response, AiCallError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    log_provider_failure(operation, resolved, status.as_u16(), &body);
    Err(match status.as_u16() {
        401 | 403 => AiCallError::Unauthorized,
        429 => AiCallError::RateLimited,
        other => AiCallError::Provider {
            status: other,
            detail: provider_error_excerpt(&body, resolved.api_key.as_deref())
                .chars()
                .take(ERROR_DETAIL_MAX)
                .collect(),
        },
    })
}

/// A `usage` count from an OpenAI-compatible response, zero when absent.
fn usage_u32(json: &serde_json::Value, key: &str) -> u32 {
    json["usage"][key]
        .as_u64()
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

/// Write one `ai_usage_log` row for a completed call and return its cost
/// estimate. A failed write is logged, never silently dropped, and never fails
/// the call: the answer has already been paid for.
#[allow(clippy::too_many_arguments)]
async fn record_call(
    budgets: &AiTokenBudgetService,
    db: &PgPool,
    caller: &AiCaller,
    provider_id: &str,
    requested_model: &str,
    op: AiOperationType,
    served_model: &str,
    (prompt_tokens, completion_tokens, total_tokens): (u32, u32, u32),
    latency_ms: u64,
) -> Option<f64> {
    let clamp = |n: u32| n.min(i32::MAX as u32) as i32;
    let cost_estimate = budgets
        .estimate_call_cost(
            requested_model,
            served_model,
            i64::from(prompt_tokens),
            i64::from(completion_tokens),
        )
        .await;
    let entry = UsageLogEntry {
        user_id: caller.user_id(),
        plugin_name: caller.source.clone(),
        provider_id: provider_id.to_string(),
        operation: op.to_string(),
        model: served_model.to_string(),
        prompt_tokens: clamp(prompt_tokens),
        completion_tokens: clamp(completion_tokens),
        total_tokens: clamp(total_tokens),
        latency_ms: latency_ms.min(i64::MAX as u64) as i64,
        cost_estimate,
    };
    if let Err(e) = budgets.record_usage(db, entry).await {
        tracing::warn!(
            error = %e,
            source = %caller.source,
            provider = %provider_id,
            "failed to record AI usage"
        );
    }
    cost_estimate
}

/// One budget check's answer: `(allowed, action on limit, used, limit)`.
type BudgetVerdict = (bool, BudgetAction, f64, f64);

/// Exact loopback base URLs a test has allowed past the send-time check.
static TEST_LOOPBACK_PROVIDERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// Allow one loopback provider URL past the send-time base URL check.
///
/// `pub` but `#[doc(hidden)]` solely for tests, in this crate and in
/// `tests/`, that drive the real send path against a local stand-in provider
/// (the scripted provider calls it for every server it starts). Nothing in
/// production calls it, no configuration reaches it, and it accepts only an
/// `http://127.0.0.1:<port>` URL, exactly: every other private address, and
/// every other loopback URL, is still refused.
///
/// # Panics
///
/// Panics if `base_url` is not `http://127.0.0.1:<port>`.
#[doc(hidden)]
pub fn allow_loopback_provider_for_tests(base_url: &str) {
    let parsed = url::Url::parse(base_url).ok();
    let is_loopback_literal = parsed.as_ref().is_some_and(|u| {
        u.scheme() == "http"
            && u.host() == Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST))
            && u.port().is_some()
    });
    assert!(
        is_loopback_literal,
        "only an http://127.0.0.1:<port> URL may be allowed, got {base_url}"
    );
    TEST_LOOPBACK_PROVIDERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(base_url.trim_end_matches('/').to_string());
}

fn loopback_allowed_for_tests(base_url: &str) -> bool {
    TEST_LOOPBACK_PROVIDERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(base_url.trim_end_matches('/'))
}

/// Result of a connection test against a provider endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionTestResult {
    /// Whether the connection test succeeded.
    pub success: bool,
    /// Human-readable result message.
    pub message: String,
    /// Round-trip latency in milliseconds.
    pub latency_ms: u64,
}

// Site config keys
const CONFIG_KEY_PROVIDERS: &str = "ai_providers";
const CONFIG_KEY_DEFAULTS: &str = "ai_defaults";
const CONFIG_KEY_TIMEOUTS: &str = "ai_timeouts";

/// Default outbound timeout for request-scoped (web/user) AI calls, seconds.
/// Effectively unchanged from the former fixed 10s client timeout (P11c / D-43).
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 10;
/// Default outbound timeout for background AI calls, seconds (P11c / D-43).
/// Larger than request-scoped to accommodate legitimately slow analyze-class
/// work, and comfortably under the 150s background epoch budget.
const DEFAULT_BACKGROUND_TIMEOUT_SECS: u64 = 90;
/// Hard ceiling on any resolved provider timeout, seconds: the 150s background
/// epoch budget. A `site_config` value can never push the HTTP timeout past the
/// CPU deadline that would kill the plugin anyway (P11c / D-43).
const MAX_TIMEOUT_SECS: u64 = 150;
/// Default circuit-breaker failure window (seconds) for a per-operation breaker
/// when the config specifies no override — matches the pre-P11c breaker window.
const DEFAULT_BREAKER_WINDOW_SECS: u64 = 120;
/// Upper bound on a configured breaker window (seconds). Unlike a request
/// timeout, a failure-counting window may legitimately exceed the epoch budget,
/// but is still bounded to keep the value sane.
const MAX_BREAKER_WINDOW_SECS: u64 = 3600;

fn default_request_timeout_secs() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECS
}
fn default_background_timeout_secs() -> u64 {
    DEFAULT_BACKGROUND_TIMEOUT_SECS
}

/// Configurable AI provider timeouts (P11c / D-43), persisted in `site_config`
/// under `ai_timeouts` and read per request.
///
/// Replaces the former single hard-coded 10s client timeout with **per-operation
/// and per-provider** timeouts. Two independent cuts bound an AI call: the
/// **epoch deadline** bounds plugin CPU (150s background), and this **provider
/// timeout** bounds the outbound HTTP call — both apply and the tighter wins.
/// Every resolved timeout is clamped to `[1, MAX_TIMEOUT_SECS]` so a config value
/// can never exceed the background epoch budget.
///
/// Because the values are read from `site_config` on each call, changes take
/// effect on the next request with no rebuild or restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiTimeoutConfig {
    /// Default timeout for request-scoped (web/user) operations, seconds.
    #[serde(default = "default_request_timeout_secs")]
    pub request_default_secs: u64,
    /// Default timeout for background (cron / queue-worker principal) operations,
    /// seconds.
    #[serde(default = "default_background_timeout_secs")]
    pub background_default_secs: u64,
    /// Per-operation timeout overrides keyed by the snake_case operation name
    /// (`"chat"`, `"embedding"`, …). Overrides the request/background default.
    #[serde(default)]
    pub per_operation_secs: HashMap<String, u64>,
    /// Per-provider timeout overrides keyed by provider id. The most specific
    /// override — wins over a per-operation value.
    #[serde(default)]
    pub per_provider_secs: HashMap<String, u64>,
    /// Per-operation circuit-breaker failure-window overrides, seconds, so a
    /// legitimately slow operation class carries a wider window than fast ops and
    /// does not trip a breaker sized for fast calls.
    #[serde(default)]
    pub breaker_window_secs: HashMap<String, u64>,
}

impl Default for AiTimeoutConfig {
    fn default() -> Self {
        Self {
            request_default_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            background_default_secs: DEFAULT_BACKGROUND_TIMEOUT_SECS,
            per_operation_secs: HashMap::new(),
            per_provider_secs: HashMap::new(),
            breaker_window_secs: HashMap::new(),
        }
    }
}

impl AiTimeoutConfig {
    /// Resolve the outbound HTTP timeout for a call.
    ///
    /// Precedence (most specific wins): per-provider → per-operation →
    /// (background|request) default. The result is clamped to
    /// `[1, MAX_TIMEOUT_SECS]`.
    pub fn resolve_timeout(
        &self,
        operation: &str,
        provider_id: &str,
        is_background: bool,
    ) -> Duration {
        let mut secs = if is_background {
            self.background_default_secs
        } else {
            self.request_default_secs
        };
        if let Some(&v) = self.per_operation_secs.get(operation) {
            secs = v;
        }
        if let Some(&v) = self.per_provider_secs.get(provider_id) {
            secs = v;
        }
        Duration::from_secs(secs.clamp(1, MAX_TIMEOUT_SECS))
    }

    /// Resolve the circuit-breaker failure window for an operation class, clamped
    /// to `[1, MAX_BREAKER_WINDOW_SECS]`.
    pub fn resolve_breaker_window(&self, operation: &str) -> Duration {
        let secs = self
            .breaker_window_secs
            .get(operation)
            .copied()
            .unwrap_or(DEFAULT_BREAKER_WINDOW_SECS);
        Duration::from_secs(secs.clamp(1, MAX_BREAKER_WINDOW_SECS))
    }
}

/// Maximum characters of text sent to an embeddings endpoint in a single
/// request.
///
/// A crude proxy for the provider's token ceiling — `text-embedding-3-*` caps
/// at 8,191 tokens, and ~24,000 chars (≈ ~6k tokens at ~4 chars/token) stays
/// safely under it for typical prose. Over-budget input is truncated with a
/// warning rather than rejected, so an oversized item is still indexed on its
/// leading content instead of being silently dropped (the embedding provider
/// would otherwise reject the whole request, leaving the item unsearchable),
/// and unbounded outbound payloads — a cost/DoS surface — are bounded.
///
/// A single conservative constant keeps 1.0 simple; it can be promoted to
/// per-provider config later if deployments use models with very different
/// limits.
pub(crate) const EMBEDDING_INPUT_MAX_CHARS: usize = 24_000;

/// Environment variable names that must never be used as API key references.
///
/// These contain sensitive process configuration (database credentials,
/// session secrets, etc.) that could be exfiltrated via SSRF if sent as
/// auth headers to an attacker-controlled `base_url`.
const DENIED_ENV_VARS: &[&str] = &[
    "DATABASE_URL",
    "REDIS_URL",
    "JWT_SECRET",
    "SMTP_PASSWORD",
    "SMTP_USERNAME",
    "SMTP_HOST",
    "SECRET_KEY",
    "SESSION_SECRET",
    "COOKIE_SECRET",
    "ARGON2_SECRET",
    "ENCRYPTION_KEY",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

// =============================================================================
// Validation
// =============================================================================

/// Validate that an environment variable name is safe for use as an API key
/// reference.
///
/// Rules:
/// - Must match `^[A-Z][A-Z0-9_]*$` (uppercase letters, digits, underscores).
/// - Must not be in the denylist of sensitive system variables.
/// - Empty string is allowed (means "no API key").
pub fn validate_env_var_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Ok(());
    }

    // Format check: uppercase letters, digits, underscores only
    let mut chars = name.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_uppercase() {
        return Err(
            "Environment variable name must start with an uppercase letter (A-Z).".to_string(),
        );
    }
    if !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return Err(
            "Environment variable name must contain only uppercase letters, digits, and underscores."
                .to_string(),
        );
    }

    // Denylist check
    let upper = name.to_ascii_uppercase();
    if DENIED_ENV_VARS
        .iter()
        .any(|d| d.eq_ignore_ascii_case(&upper))
    {
        return Err(format!(
            "'{name}' is a reserved system variable and cannot be used as an API key reference."
        ));
    }

    Ok(())
}

/// Validate that a base URL is safe for outbound requests (SSRF prevention).
///
/// Rules:
/// - Must parse as a valid URL.
/// - Scheme must be `http` or `https`.
/// - Host must not resolve to a private, loopback, or link-local IP range.
pub fn validate_base_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("URL scheme must be http or https, got '{other}'.")),
    }

    // Classify the host `url` already parsed. Re-parsing `host_str()` cannot
    // see an IPv6 literal, because that form keeps its brackets.
    let Some(host) = parsed.host() else {
        return Err("URL must include a host.".to_string());
    };

    let blocked_address =
        "URL must not target private, loopback, or link-local addresses.".to_string();

    match host {
        url::Host::Ipv4(v4) => {
            if is_disallowed_ip(IpAddr::V4(v4)) {
                return Err(blocked_address);
            }
        }
        url::Host::Ipv6(v6) => {
            if is_disallowed_ip(IpAddr::V6(v6)) {
                return Err(blocked_address);
            }
        }
        url::Host::Domain(name) => {
            // Block well-known local hostnames. This list is deliberately
            // narrower than the plugin fence's: a `.internal` or `.local` name
            // passes this string check. The address behind the name is
            // checked when the request connects: the provider client is built
            // from `hardened_outbound_builder()`, whose resolver refuses the
            // connection if the name resolves to any address `net_policy`
            // refuses (private, loopback, link-local and the other reserved
            // ranges).
            if name == "localhost" || name.ends_with(".localhost") {
                return Err(blocked_address);
            }

            // Check for common metadata endpoint hostnames
            if name == "metadata.google.internal" || name == "metadata.google.com" {
                return Err("URL must not target cloud metadata services.".to_string());
            }
        }
    }

    Ok(())
}

// =============================================================================
// Service
// =============================================================================

/// Service for managing AI provider configurations.
pub struct AiProviderService {
    db: PgPool,
    http: reqwest::Client,
    /// Serializes read-modify-write operations on `site_config` rows to
    /// prevent lost updates from concurrent admin requests.
    write_lock: Mutex<()>,
    /// Circuit breaker for the connection-test / connectivity-probe path. Its
    /// state is what the metrics endpoint reports (`circuit_breaker()`).
    circuit_breaker: CircuitBreaker,
    /// Per-operation circuit breakers for the outbound AI request path
    /// (P11c / D-43). Keyed by operation `config_key`, each lazily created with
    /// that operation's configured failure window, so a legitimately slow
    /// analyze-class call does not trip a breaker sized for fast decide calls.
    generate_breakers: std::sync::Mutex<HashMap<String, Arc<CircuitBreaker>>>,
    /// Budget checks and usage recording for every send.
    budgets: AiTokenBudgetService,
}

impl AiProviderService {
    /// Create a new AI provider service.
    ///
    /// # Panics
    ///
    /// Panics if the SSRF-hardened HTTP client fails to build (TLS backend
    /// init). This is fail-closed: AI requests must never silently go out on
    /// an unhardened client.
    pub fn new(db: PgPool) -> Self {
        // The client-level timeout is the request-scoped default and the backstop
        // for any path that does not set a per-request timeout (embedding,
        // connection test). The AI request path overrides it per request with the
        // per-operation / per-provider timeout resolved from `ai_timeouts`
        // (P11c / D-43); a `RequestBuilder::timeout` overrides this default.
        //
        // Built from the shared SSRF-hardened builder, so every AI send path
        // (chat, streaming chat, tool calling, embeddings, the connection test
        // and the plugin `ai-request` call, all of which take this client from
        // `http()`) gets the resolve-time address check and per-hop redirect
        // revalidation that plugin HTTP has. Fail-closed: a build failure must
        // never fall back to an unhardened default client.
        #[allow(clippy::expect_used)]
        let http = crate::host::http::hardened_outbound_builder()
            .timeout(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
            .build()
            .expect("SSRF-hardened AI provider HTTP client must build");
        Self {
            http,
            write_lock: Mutex::new(()),
            circuit_breaker: CircuitBreaker::new(
                "ai_provider",
                BreakerConfig {
                    failure_threshold: 3,
                    recovery_timeout: Duration::from_secs(60),
                    failure_window: Duration::from_secs(DEFAULT_BREAKER_WINDOW_SECS),
                },
            ),
            generate_breakers: std::sync::Mutex::new(HashMap::new()),
            budgets: AiTokenBudgetService::new(db.clone()),
            db,
        }
    }

    /// Get the connection-test circuit breaker for monitoring.
    pub fn circuit_breaker(&self) -> &CircuitBreaker {
        &self.circuit_breaker
    }

    /// Get (creating on first use) the per-operation circuit breaker for the
    /// outbound AI request path (P11c / D-43).
    ///
    /// The breaker is created once per operation `config_key` with the supplied
    /// failure `window`; the threshold (3) and recovery (60s) match the
    /// connection breaker. Because creation is lazy and one-time, a later config
    /// change to an operation's window applies to breakers created afterward (and
    /// on restart) — existing breakers keep their window, which is acceptable for
    /// a failure-counting window.
    pub fn breaker_for_operation(&self, op_key: &str, window: Duration) -> Arc<CircuitBreaker> {
        let mut map = self
            .generate_breakers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Arc::clone(map.entry(op_key.to_string()).or_insert_with(|| {
            Arc::new(CircuitBreaker::new(
                format!("ai_provider:{op_key}"),
                BreakerConfig {
                    failure_threshold: 3,
                    recovery_timeout: Duration::from_secs(60),
                    failure_window: window,
                },
            ))
        }))
    }

    /// The state of every per-operation breaker, by operation key, for the
    /// metrics gauge. An operation whose breaker has not been created yet has
    /// made no call on this server, so it reads `closed`.
    pub fn operation_breaker_states(&self) -> Vec<(&'static str, &'static str)> {
        let map = self
            .generate_breakers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        AiOperationType::ALL
            .iter()
            .map(|op| {
                let key = op.config_key();
                let state = map.get(key).map_or("closed", |b| b.state_name());
                (key, state)
            })
            .collect()
    }

    /// Read the AI timeout configuration (P11c / D-43) from `site_config`,
    /// falling back to defaults when unset. Read per request, so config changes
    /// take effect without a rebuild.
    pub async fn get_timeout_config(&self) -> Result<AiTimeoutConfig> {
        let value = SiteConfig::get(&self.db, CONFIG_KEY_TIMEOUTS)
            .await
            .context("failed to read ai_timeouts config")?;
        match value {
            Some(v) => serde_json::from_value(v).context("failed to parse ai_timeouts config"),
            None => Ok(AiTimeoutConfig::default()),
        }
    }

    /// Persist the AI timeout configuration (P11c / D-43) to `site_config`.
    pub async fn save_timeout_config(&self, config: &AiTimeoutConfig) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let value = serde_json::to_value(config).context("failed to serialize ai_timeouts")?;
        SiteConfig::set(&self.db, CONFIG_KEY_TIMEOUTS, value)
            .await
            .context("failed to save ai_timeouts config")
    }

    /// Get the shared HTTP client for making outbound requests.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    // -------------------------------------------------------------------------
    // CRUD
    // -------------------------------------------------------------------------

    /// List all configured providers.
    pub async fn list_providers(&self) -> Result<Vec<AiProviderConfig>> {
        let value = SiteConfig::get(&self.db, CONFIG_KEY_PROVIDERS)
            .await
            .context("failed to read ai_providers config")?;
        match value {
            Some(v) => serde_json::from_value(v).context("failed to parse ai_providers config"),
            None => Ok(Vec::new()),
        }
    }

    /// Get a single provider by ID.
    pub async fn get_provider(&self, id: &str) -> Result<Option<AiProviderConfig>> {
        let providers = self.list_providers().await?;
        Ok(providers.into_iter().find(|p| p.id == id))
    }

    /// Create or update a provider (upsert by `config.id`).
    ///
    /// Holds a write lock for the duration to prevent concurrent
    /// read-modify-write races.
    pub async fn save_provider(&self, config: AiProviderConfig) -> Result<()> {
        let _guard = self.write_lock.lock().await;

        let mut providers = self.list_providers().await?;

        if let Some(existing) = providers.iter_mut().find(|p| p.id == config.id) {
            *existing = config;
        } else {
            providers.push(config);
        }

        let value = serde_json::to_value(&providers).context("failed to serialize ai_providers")?;
        SiteConfig::set(&self.db, CONFIG_KEY_PROVIDERS, value)
            .await
            .context("failed to save ai_providers config")
    }

    /// Delete a provider by ID. Returns `true` if found and removed.
    ///
    /// Also removes this provider from any default assignments.
    /// Both operations are performed under a single write lock.
    pub async fn delete_provider(&self, id: &str) -> Result<bool> {
        let _guard = self.write_lock.lock().await;

        let mut providers = self.list_providers().await?;
        let len_before = providers.len();
        providers.retain(|p| p.id != id);
        let removed = providers.len() < len_before;

        if removed {
            let value =
                serde_json::to_value(&providers).context("failed to serialize ai_providers")?;
            SiteConfig::set(&self.db, CONFIG_KEY_PROVIDERS, value)
                .await
                .context("failed to save ai_providers config")?;

            // Clean up defaults that referenced this provider
            let mut defaults = self.get_defaults().await?;
            defaults.defaults.retain(|_, pid| pid != id);
            let dv = serde_json::to_value(&defaults).context("failed to serialize ai_defaults")?;
            SiteConfig::set(&self.db, CONFIG_KEY_DEFAULTS, dv)
                .await
                .context("failed to save ai_defaults config")?;
        }

        Ok(removed)
    }

    // -------------------------------------------------------------------------
    // Defaults
    // -------------------------------------------------------------------------

    /// Get the default provider assignments.
    pub async fn get_defaults(&self) -> Result<AiDefaults> {
        let value = SiteConfig::get(&self.db, CONFIG_KEY_DEFAULTS)
            .await
            .context("failed to read ai_defaults config")?;
        match value {
            Some(v) => serde_json::from_value(v).context("failed to parse ai_defaults config"),
            None => Ok(AiDefaults::default()),
        }
    }

    /// Set the default provider for an operation type.
    pub async fn set_default(&self, op: AiOperationType, provider_id: &str) -> Result<()> {
        let _guard = self.write_lock.lock().await;

        let mut defaults = self.get_defaults().await?;
        defaults.defaults.insert(op, provider_id.to_string());
        let value = serde_json::to_value(&defaults).context("failed to serialize ai_defaults")?;
        SiteConfig::set(&self.db, CONFIG_KEY_DEFAULTS, value)
            .await
            .context("failed to save ai_defaults config")
    }

    /// Remove the default provider for an operation type.
    pub async fn remove_default(&self, op: AiOperationType) -> Result<()> {
        let _guard = self.write_lock.lock().await;

        let mut defaults = self.get_defaults().await?;
        defaults.defaults.remove(&op);
        let value = serde_json::to_value(&defaults).context("failed to serialize ai_defaults")?;
        SiteConfig::set(&self.db, CONFIG_KEY_DEFAULTS, value)
            .await
            .context("failed to save ai_defaults config")
    }

    /// Save defaults atomically (replaces all defaults at once).
    ///
    /// Used by the admin defaults form to save all assignments in one operation.
    pub async fn save_defaults(&self, defaults: AiDefaults) -> Result<()> {
        let _guard = self.write_lock.lock().await;

        let value = serde_json::to_value(&defaults).context("failed to serialize ai_defaults")?;
        SiteConfig::set(&self.db, CONFIG_KEY_DEFAULTS, value)
            .await
            .context("failed to save ai_defaults config")
    }

    // -------------------------------------------------------------------------
    // Resolution
    // -------------------------------------------------------------------------

    /// Resolve a provider for the given operation type.
    ///
    /// If `override_id` is `Some`, that specific provider is used; otherwise
    /// the default for the operation type is looked up.
    ///
    /// Returns `None` if no provider is configured or the provider does not
    /// have a model for the requested operation.
    pub async fn resolve_provider(
        &self,
        op: AiOperationType,
        override_id: Option<&str>,
    ) -> Result<Option<ResolvedProvider>> {
        let provider_id = match override_id {
            Some(id) => id.to_string(),
            None => {
                let defaults = self.get_defaults().await?;
                match defaults.defaults.get(&op) {
                    Some(id) => id.clone(),
                    None => return Ok(None),
                }
            }
        };

        let config = match self.get_provider(&provider_id).await? {
            Some(c) if c.enabled => c,
            _ => return Ok(None),
        };

        let model = match config.models.iter().find(|m| m.operation == op) {
            Some(m) => m.model.clone(),
            None => return Ok(None),
        };

        let api_key = Self::resolve_api_key(&config);

        Ok(Some(ResolvedProvider {
            config,
            api_key,
            model,
        }))
    }

    /// The model string the active embedding provider resolves to, without
    /// making a provider call (P11f backfill). `None` when no embedding provider
    /// is configured. Used to key the backfill gap query so a model change
    /// re-enqueues items embedded under the previous model.
    pub async fn embedding_model(&self) -> Option<String> {
        self.resolve_provider(AiOperationType::Embedding, None)
            .await
            .ok()
            .flatten()
            .map(|r| r.model)
    }

    /// Resolve the API key from the environment variable referenced by the config.
    pub fn resolve_api_key(config: &AiProviderConfig) -> Option<String> {
        if config.api_key_env.is_empty() {
            return None;
        }
        std::env::var(&config.api_key_env).ok()
    }

    /// Check whether the referenced environment variable is set.
    pub fn key_is_set(config: &AiProviderConfig) -> bool {
        Self::resolve_api_key(config).is_some()
    }

    /// Return a masked representation of the key reference for display.
    ///
    /// Example: `"OPENAI_API_KEY (set)"` or `"OPENAI_API_KEY (not set)"`.
    pub fn mask_key_ref(config: &AiProviderConfig) -> String {
        if config.api_key_env.is_empty() {
            return "(no env var configured)".to_string();
        }
        if Self::key_is_set(config) {
            format!("{} (set)", config.api_key_env)
        } else {
            format!("{} (not set)", config.api_key_env)
        }
    }

    // -------------------------------------------------------------------------
    // The send path (B89)
    // -------------------------------------------------------------------------
    //
    // Every AI call the kernel makes, from a route, a job or a plugin, goes out
    // through one of the three methods below. Each one, before anything leaves
    // the process:
    //
    // - checks the provider's base URL with `validate_base_url`, at send time,
    //   because a provider row can reach `site_config` without passing the admin
    //   form and an IP literal never passes through the hardened resolver;
    // - resolves the outbound timeout from `ai_timeouts` for the caller's plane
    //   (request or background), unless the caller's own feature configures one;
    // - goes through the per-operation circuit breaker;
    //
    // and on failure returns an `AiCallError` whose text is safe to show: the
    // provider's own words go to the log through `log_provider_failure`.

    /// The base URL check every send makes.
    fn check_send_url(base_url: &str) -> Result<(), AiCallError> {
        if loopback_allowed_for_tests(base_url) {
            return Ok(());
        }
        validate_base_url(base_url).map_err(|reason| {
            tracing::warn!(
                base_url = %base_url,
                reason = %reason,
                "AI provider base URL refused at send time"
            );
            AiCallError::BaseUrlRefused
        })
    }

    /// The outbound timeout and the circuit breaker for one call.
    async fn send_guard(
        &self,
        op: AiOperationType,
        resolved: &ResolvedProvider,
        caller: &AiCaller,
        configured: Option<Duration>,
    ) -> (Duration, Arc<CircuitBreaker>) {
        let config = self.get_timeout_config().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read AI timeout config, using defaults");
            AiTimeoutConfig::default()
        });
        let op_key = op.config_key();
        let timeout = configured.unwrap_or_else(|| {
            config.resolve_timeout(op_key, &resolved.config.id, caller.background)
        });
        // The circuit breakers stay per server on purpose: each one reacts to
        // the failures this server sees, opens and probes on its own, and
        // sharing them would add a Redis round trip to every AI call to save a
        // few failed requests per server.
        let breaker = self.breaker_for_operation(op_key, config.resolve_breaker_window(op_key));
        (timeout, breaker)
    }

    /// Hold a call to its caller's budget before it is sent.
    ///
    /// A person is held to their per-user or per-role token budget; an
    /// anonymous visitor to the site-level anonymous cap (the `anonymous user`
    /// role's limit, metered over every anonymous call); a plugin's background
    /// call to the plugin's token and currency budgets. The kernel's own work
    /// (indexing embeddings) has no budget. A budget that cannot be read allows
    /// the call, with a warning, as it always has.
    async fn check_budget(
        &self,
        caller: &AiCaller,
        resolved: &ResolvedProvider,
    ) -> Result<(), AiCallError> {
        let provider = resolved.config.id.as_str();
        let checks: Vec<(&str, Result<BudgetVerdict>)> = match caller.principal {
            AiPrincipal::Kernel => return Ok(()),
            AiPrincipal::User(_) | AiPrincipal::Anonymous => {
                let user = caller.user_id().unwrap_or_else(uuid::Uuid::nil);
                vec![(
                    "token",
                    self.budgets
                        .check_budget(&self.db, user, provider)
                        .await
                        .map(|r| (r.allowed, r.action, r.used as f64, r.limit as f64)),
                )]
            }
            AiPrincipal::Plugin => vec![
                (
                    "token",
                    self.budgets
                        .check_plugin_budget(&self.db, &caller.source, provider)
                        .await
                        .map(|r| (r.allowed, r.action, r.used as f64, r.limit as f64)),
                ),
                (
                    "currency",
                    self.budgets
                        .check_plugin_cost_budget(&self.db, &caller.source, provider)
                        .await
                        .map(|r| (r.allowed, r.action, r.used, r.limit)),
                ),
            ],
        };
        for (kind, check) in checks {
            match check {
                Ok((false, BudgetAction::Deny | BudgetAction::Queue, used, limit)) => {
                    tracing::warn!(
                        source = %caller.source,
                        principal = ?caller.principal,
                        provider = %provider,
                        budget = kind,
                        used,
                        limit,
                        "AI budget exceeded"
                    );
                    return Err(AiCallError::BudgetExceeded);
                }
                Ok((false, BudgetAction::Warn, used, limit)) => tracing::warn!(
                    source = %caller.source,
                    principal = ?caller.principal,
                    provider = %provider,
                    budget = kind,
                    used,
                    limit,
                    "AI budget exceeded (warn mode, allowing)"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    error = %e,
                    source = %caller.source,
                    budget = kind,
                    "failed to check AI budget, allowing request"
                ),
            }
        }
        Ok(())
    }

    /// Record one completed call in `ai_usage_log` with the tokens the
    /// provider reported and a cost estimate, and return the estimate.
    ///
    /// Priced on `resolved.model`, the model the call ASKED for, not on
    /// `served_model`, the string the provider reports having served:
    /// Anthropic resolves an alias server side, so a request for
    /// `claude-haiku-4-5` answers as `claude-haiku-4-5-20251001`, which matches
    /// no row in a pricing table keyed by the name the operator configured.
    #[allow(clippy::too_many_arguments)]
    async fn record_usage(
        &self,
        caller: &AiCaller,
        resolved: &ResolvedProvider,
        op: AiOperationType,
        served_model: &str,
        (prompt_tokens, completion_tokens, total_tokens): (u32, u32, u32),
        latency_ms: u64,
    ) -> Option<f64> {
        record_call(
            &self.budgets,
            &self.db,
            caller,
            &resolved.config.id,
            &resolved.model,
            op,
            served_model,
            (prompt_tokens, completion_tokens, total_tokens),
            latency_ms,
        )
        .await
    }

    /// Run one chat completion, with or without tools.
    ///
    /// The one non-streaming chat send for the kernel: search expansion,
    /// summaries and follow-ups, the sentiment classifier, the writing
    /// assistant, the AI Assistant's tool-calling turns and a plugin's
    /// `ai-request` all come through here.
    pub async fn chat_once(
        &self,
        resolved: &ResolvedProvider,
        caller: &AiCaller,
        request: &ChatRequest,
    ) -> Result<ChatCompletion, AiCallError> {
        Self::check_send_url(&resolved.config.base_url)?;
        self.check_budget(caller, resolved).await?;
        let (timeout, breaker) = self
            .send_guard(AiOperationType::Chat, resolved, caller, request.timeout)
            .await;

        let result = breaker
            .call(|| async {
                let (url, body, headers) = ai_tools::build_chat_request(resolved, request, false);
                let started = Instant::now();
                let response = self
                    .post_json(&url, &body, &headers, timeout, resolved)
                    .await?;
                let response = check_status(response, "chat", resolved).await?;
                let value: serde_json::Value = response.json().await.map_err(|e| {
                    AiCallError::Malformed(format!("unreadable provider response: {e}"))
                })?;
                let mut completion = ai_tools::parse_chat_response(
                    resolved.config.protocol,
                    &value,
                )
                .ok_or_else(|| {
                    AiCallError::Malformed("provider response had no completion in it".into())
                })?;
                completion.latency_ms =
                    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                if completion.model.is_empty() {
                    completion.model = resolved.model.clone();
                }
                Ok(completion)
            })
            .await;

        let mut completion = match result {
            Ok(completion) => completion,
            Err(crate::circuit_breaker::CircuitBreakerError::Open) => {
                return Err(AiCallError::BreakerOpen);
            }
            Err(crate::circuit_breaker::CircuitBreakerError::ServiceError(e)) => return Err(e),
        };
        completion.cost_estimate = self
            .record_usage(
                caller,
                resolved,
                AiOperationType::Chat,
                &completion.model,
                completion.usage,
                completion.latency_ms,
            )
            .await;
        Ok(completion)
    }

    /// Start one streaming chat completion.
    ///
    /// The configured timeout bounds the start: connecting and the provider's
    /// response headers. Once the stream is flowing, each chunk has its own idle
    /// timeout in the stream parser, so a long answer is not cut off at a
    /// timeout sized for a one-shot call. A start that fails counts against the
    /// chat breaker, like any other chat call.
    pub async fn chat_stream(
        &self,
        resolved: &ResolvedProvider,
        caller: &AiCaller,
        request: &ChatRequest,
    ) -> Result<ChatEventStream, AiCallError> {
        Self::check_send_url(&resolved.config.base_url)?;
        self.check_budget(caller, resolved).await?;
        let (timeout, breaker) = self
            .send_guard(AiOperationType::Chat, resolved, caller, request.timeout)
            .await;
        let started = Instant::now();

        let result = breaker
            .call(|| async {
                let (url, body, headers) = ai_tools::build_chat_request(resolved, request, true);
                let mut builder = self
                    .http
                    .post(&url)
                    .header("content-type", "application/json")
                    .body(serde_json::to_string(&body).unwrap_or_default());
                for (name, value) in &headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }
                let response =
                    match tokio::time::timeout(timeout, send_traced(builder, resolved)).await {
                        Ok(sent) => sent.map_err(transport_error)?,
                        Err(_) => return Err(AiCallError::Timeout),
                    };
                check_status(response, "chat_stream", resolved).await
            })
            .await;

        let response = match result {
            Ok(response) => response,
            Err(crate::circuit_breaker::CircuitBreakerError::Open) => {
                return Err(AiCallError::BreakerOpen);
            }
            Err(crate::circuit_breaker::CircuitBreakerError::ServiceError(e)) => return Err(e),
        };

        let events: ChatEventStream = match resolved.config.protocol {
            ProviderProtocol::OpenAiCompatible => {
                Box::pin(crate::services::ai_chat::parse_openai_stream(response))
            }
            ProviderProtocol::Anthropic => {
                Box::pin(crate::services::ai_chat::parse_anthropic_stream(response))
            }
        };

        // Record the call when the stream reports its usage, which it does in
        // its last event: the token counts do not exist before then.
        let budgets = self.budgets.clone();
        let db = self.db.clone();
        let caller = caller.clone();
        let provider_id = resolved.config.id.clone();
        let model = resolved.model.clone();
        Ok(Box::pin(async_stream::stream! {
            use tokio_stream::StreamExt;
            let mut events = events;
            while let Some(event) = events.next().await {
                if let ChatStreamEvent::Done { prompt_tokens, completion_tokens, total_tokens } = &event {
                    record_call(
                        &budgets,
                        &db,
                        &caller,
                        &provider_id,
                        &model,
                        AiOperationType::Chat,
                        &model,
                        (*prompt_tokens, *completion_tokens, *total_tokens),
                        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    )
                    .await;
                }
                yield event;
            }
        }))
    }

    /// Generate an embedding vector for `text` using the configured
    /// `Embedding` provider.
    ///
    /// Returns `Ok(None)` when no embedding provider is configured, the
    /// provider is disabled, or the provider's protocol exposes no embeddings
    /// API (Anthropic has none). Returns `Err` only on a refused base URL, a
    /// transport, HTTP-status, or response-parse failure once a provider *is*
    /// resolved — callers treat both `None` and `Err` as "no embedding
    /// available" and degrade gracefully (no-match on read, skipped index on
    /// write).
    ///
    /// The resolved model is returned in [`EmbeddingResult`] so that write-time
    /// (index) and read-time (gather `SemanticSimilarity`) callers use the same
    /// model — `item_embeddings` similarity search filters by model.
    pub async fn embed(&self, text: &str, caller: &AiCaller) -> Result<Option<EmbeddingResult>> {
        let Some(resolved) = self
            .resolve_provider(AiOperationType::Embedding, None)
            .await
            .context("failed to resolve embedding provider")?
        else {
            return Ok(None);
        };

        // Embeddings are only defined for the OpenAI-compatible protocol;
        // Anthropic exposes no embeddings endpoint.
        if resolved.config.protocol != ProviderProtocol::OpenAiCompatible {
            tracing::warn!(
                protocol = %resolved.config.protocol,
                "embedding requested but provider protocol has no embeddings API"
            );
            return Ok(None);
        }

        let result = self
            .embed_with(&resolved, text, caller)
            .await
            .map_err(|e| anyhow::anyhow!("embedding request failed: {e}"))?;
        Ok(Some(result))
    }

    /// Embed `text` with an already resolved provider: the one embedding send,
    /// for the kernel's own indexing and search and a plugin's `ai-request`.
    pub async fn embed_with(
        &self,
        resolved: &ResolvedProvider,
        text: &str,
        caller: &AiCaller,
    ) -> Result<EmbeddingResult, AiCallError> {
        if resolved.config.protocol != ProviderProtocol::OpenAiCompatible {
            return Err(AiCallError::Unsupported(format!(
                "provider protocol {} exposes no embeddings API",
                resolved.config.protocol
            )));
        }
        Self::check_send_url(&resolved.config.base_url)?;
        self.check_budget(caller, resolved).await?;
        let (timeout, breaker) = self
            .send_guard(AiOperationType::Embedding, resolved, caller, None)
            .await;

        // Cap outbound embedding input. This is the universal choke point —
        // the index path (whole-item text), the read path (query text) and a
        // plugin's input all flow through here. Over-budget input would
        // otherwise be rejected by the provider (silently leaving an item
        // unsearchable) and unbounded payloads are a cost/DoS surface.
        let (input, truncated_from) = cap_embedding_input(text);
        if let Some(original) = truncated_from {
            tracing::warn!(
                original_chars = original,
                capped_chars = EMBEDDING_INPUT_MAX_CHARS,
                "embedding input exceeds budget; truncating before request"
            );
        }
        let url = format!(
            "{}/embeddings",
            resolved.config.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": resolved.model,
            "input": input.as_ref(),
        });
        let mut headers = Vec::new();
        if let Some(ref key) = resolved.api_key {
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }

        let result = breaker
            .call(|| async {
                let started = Instant::now();
                let response = self
                    .post_json(&url, &body, &headers, timeout, resolved)
                    .await?;
                let response = check_status(response, "embedding", resolved).await?;
                let json: serde_json::Value = response.json().await.map_err(|e| {
                    AiCallError::Malformed(format!("unreadable embedding response: {e}"))
                })?;
                let vector = parse_embedding_vector(&json)
                    .map_err(|e| AiCallError::Malformed(format!("{e:#}")))?;
                let prompt_tokens = usage_u32(&json, "prompt_tokens");
                let total_tokens = json["usage"]["total_tokens"]
                    .as_u64()
                    .map_or(prompt_tokens, |t| t.min(u64::from(u32::MAX)) as u32);
                Ok(EmbeddingResult {
                    vector,
                    model: resolved.model.clone(),
                    served_model: json["model"]
                        .as_str()
                        .filter(|m| !m.is_empty())
                        .unwrap_or(&resolved.model)
                        .to_string(),
                    prompt_tokens,
                    total_tokens,
                    latency_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    cost_estimate: None,
                })
            })
            .await;

        let mut result = match result {
            Ok(result) => result,
            Err(crate::circuit_breaker::CircuitBreakerError::Open) => {
                return Err(AiCallError::BreakerOpen);
            }
            Err(crate::circuit_breaker::CircuitBreakerError::ServiceError(e)) => return Err(e),
        };
        // An embedding reports input tokens only; there is no completion.
        result.cost_estimate = self
            .record_usage(
                caller,
                resolved,
                AiOperationType::Embedding,
                &result.served_model,
                (result.prompt_tokens, 0, result.total_tokens),
                result.latency_ms,
            )
            .await;
        Ok(result)
    }

    /// POST a JSON body to a provider with a per-request timeout.
    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(String, String)],
        timeout: Duration,
        resolved: &ResolvedProvider,
    ) -> Result<reqwest::Response, AiCallError> {
        let mut builder = self
            .http
            .post(url)
            .timeout(timeout)
            .header("content-type", "application/json")
            .body(serde_json::to_string(body).unwrap_or_default());
        for (name, value) in headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        send_traced(builder, resolved)
            .await
            .map_err(transport_error)
    }

    // -------------------------------------------------------------------------
    // Connection test
    // -------------------------------------------------------------------------

    /// Test the connection to a provider endpoint.
    ///
    /// Both protocols `GET {base_url}/models`, which needs a valid key and is
    /// not billed. Only a 2xx is a success; any other status fails the test,
    /// and the message carries the status and the provider's explanation.
    ///
    /// The message is truncated to [`CONNECTION_TEST_MESSAGE_MAX`] characters.
    pub async fn test_connection(&self, config: &AiProviderConfig) -> ConnectionTestResult {
        // Validate URL before making any outbound request
        if let Err(msg) = validate_base_url(&config.base_url) {
            return ConnectionTestResult {
                success: false,
                message: msg,
                latency_ms: 0,
            };
        }

        let api_key = Self::resolve_api_key(config);
        let start = Instant::now();

        // Wrap the provider call with a circuit breaker to prevent cascading
        // failures when an AI provider is down.
        let base_url = config.base_url.clone();
        let protocol = config.protocol;
        let result = self
            .circuit_breaker
            .call(|| async {
                match protocol {
                    ProviderProtocol::OpenAiCompatible => {
                        self.test_openai_compatible(&base_url, api_key.as_deref())
                            .await
                    }
                    ProviderProtocol::Anthropic => {
                        self.test_anthropic(&base_url, api_key.as_deref()).await
                    }
                }
            })
            .await;

        // Flatten CircuitBreakerError into the same Result shape.
        let result = match result {
            Ok(msg) => Ok(msg),
            Err(crate::circuit_breaker::CircuitBreakerError::Open) => {
                Err(anyhow::anyhow!("AI provider circuit breaker is open"))
            }
            Err(crate::circuit_breaker::CircuitBreakerError::ServiceError(e)) => Err(e),
        };

        let latency_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(msg) => ConnectionTestResult {
                success: true,
                message: truncate(&msg, CONNECTION_TEST_MESSAGE_MAX),
                latency_ms,
            },
            Err(e) => {
                let message = truncate(&e.to_string(), CONNECTION_TEST_MESSAGE_MAX);
                tracing::warn!(
                    provider = %config.id,
                    error = %message,
                    "AI provider connection test failed"
                );
                ConnectionTestResult {
                    success: false,
                    message,
                    latency_ms,
                }
            }
        }
    }

    /// Test an OpenAI-compatible endpoint by listing models.
    async fn test_openai_compatible(
        &self,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<String> {
        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let mut req = self.http.get(&url);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        let resp = crate::telemetry::send(req)
            .await
            .context("request failed")?;
        let status = resp.status();
        if status.is_success() {
            Ok(format!("Connected successfully (HTTP {status})"))
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {status}: {}", provider_error_excerpt(&body, api_key))
        }
    }

    /// Test an Anthropic endpoint by listing models.
    ///
    /// `GET {base_url}/models` needs a valid key and costs nothing, so a 2xx
    /// proves the URL, the network path and the key together. Anything else is
    /// a failure, reported with the provider's own explanation: a 404 here used
    /// to read "Connected successfully", because the test hit the bare base URL
    /// and counted any answer at all as proof.
    async fn test_anthropic(&self, base_url: &str, api_key: Option<&str>) -> Result<String> {
        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let mut req = self
            .http
            .get(&url)
            .header("anthropic-version", "2023-06-01");
        if let Some(key) = api_key {
            req = req.header("x-api-key", key);
        }
        let resp = crate::telemetry::send(req)
            .await
            .context("request failed")?;
        let status = resp.status();
        if status.is_success() {
            Ok(format!("Connected successfully (HTTP {status})"))
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {status}: {}", provider_error_excerpt(&body, api_key))
        }
    }
}

/// Extract the first embedding vector from an OpenAI-compatible
/// `/embeddings` response of the shape `{ "data": [ { "embedding": [..] } ] }`.
pub(crate) fn parse_embedding_vector(json: &serde_json::Value) -> Result<Vec<f32>> {
    let arr = json
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|d| d.first())
        .and_then(|e| e.get("embedding"))
        .and_then(|e| e.as_array())
        .context("missing data[0].embedding in embedding response")?;

    let mut vector = Vec::with_capacity(arr.len());
    for v in arr {
        let f = v.as_f64().context("embedding element was not a number")?;
        vector.push(f as f32);
    }
    if vector.is_empty() {
        anyhow::bail!("embedding vector was empty");
    }
    Ok(vector)
}

/// Truncate a string to at most `max` UTF-8 characters.
/// Longest slice of a provider's error body that reaches the log, in characters.
///
/// Long enough for any provider's error object (Anthropic's names the parameter
/// it refused), short enough that a provider answering an HTML error page does
/// not put the whole page in one log line.
pub const PROVIDER_ERROR_BODY_MAX: usize = 1_000;

/// Longest connection test message shown on the admin page, in characters:
/// room for the status line and a provider's error object.
pub const CONNECTION_TEST_MESSAGE_MAX: usize = 500;

/// A provider's error body as it may be logged: trimmed, cut to
/// [`PROVIDER_ERROR_BODY_MAX`] characters, and with the request's own key
/// removed should a provider ever echo it back.
pub fn provider_error_excerpt(body: &str, api_key: Option<&str>) -> String {
    let mut text = body.trim().to_string();
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        text = text.replace(key, "[redacted]");
    }
    truncate(&text, PROVIDER_ERROR_BODY_MAX)
}

/// Log a provider call that came back with a non-2xx status.
///
/// Every outbound AI path calls this before turning the failure into whatever
/// its caller sees, so the log always has the status and the provider's own
/// explanation: a 400 from Anthropic names the parameter it refused, and a log
/// line without that says nothing about why. The request is never logged, so
/// neither is its key.
pub fn log_provider_failure(operation: &str, resolved: &ResolvedProvider, status: u16, body: &str) {
    tracing::warn!(
        operation = %operation,
        provider = %resolved.config.id,
        model = %resolved.model,
        http_status = status,
        body = %provider_error_excerpt(body, resolved.api_key.as_deref()),
        "AI provider call failed"
    );
}

fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}...")
    }
}

/// Cap embedding input to [`EMBEDDING_INPUT_MAX_CHARS`] on a UTF-8 char
/// boundary (no ellipsis — the text is sent verbatim to the provider).
///
/// Returns the (possibly borrowed) input plus `Some(original_char_count)` when
/// truncation occurred so the caller can emit a warning, or `None` when the
/// input was already within budget.
pub(crate) fn cap_embedding_input(text: &str) -> (std::borrow::Cow<'_, str>, Option<usize>) {
    let char_count = text.chars().count();
    if char_count > EMBEDDING_INPUT_MAX_CHARS {
        let capped: String = text.chars().take(EMBEDDING_INPUT_MAX_CHARS).collect();
        (std::borrow::Cow::Owned(capped), Some(char_count))
    } else {
        (std::borrow::Cow::Borrowed(text), None)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A loopback server that answers every request with `status` and `body`,
    /// and records the paths it was asked for.
    async fn answering(
        status: u16,
        body: &'static str,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let paths: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let seen = paths.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(uri.path().to_string());
                (axum::http::StatusCode::from_u16(status).unwrap(), body)
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/v1"), paths)
    }

    fn lazy_service() -> AiProviderService {
        AiProviderService::new(
            sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato").unwrap(),
        )
    }

    /// A 404 is a failed connection test, not a successful one.
    ///
    /// The Anthropic test treated any HTTP answer as proof of a working
    /// provider, so a base URL with a typo, or a key that could not reach the
    /// API, reported "Connected successfully (HTTP 404)".
    #[tokio::test]
    async fn the_anthropic_connection_test_fails_on_a_404() {
        let (base, _) = answering(404, r#"{"error":{"message":"b2c: no such route"}}"#).await;
        let result = lazy_service().test_anthropic(&base, Some("sk-test")).await;
        let error = result.expect_err("a 404 must fail the connection test");
        let message = error.to_string();
        assert!(message.contains("404"), "{message}");
        assert!(
            message.contains("b2c: no such route"),
            "says why: {message}"
        );
    }

    /// A 2xx from `{base}/models` is the one answer that passes, on both
    /// protocols, and it is `/models` that is asked: the bare base URL is not
    /// an endpoint and proves nothing.
    #[tokio::test]
    async fn a_2xx_from_models_passes_the_connection_test() {
        let (base, paths) = answering(200, r#"{"data":[]}"#).await;
        let svc = lazy_service();
        let message = svc.test_anthropic(&base, Some("sk-test")).await.unwrap();
        assert!(message.contains("200"), "{message}");
        svc.test_openai_compatible(&base, Some("sk-test"))
            .await
            .unwrap();
        assert_eq!(*paths.lock().unwrap(), vec!["/v1/models", "/v1/models"]);
    }

    /// The logged excerpt is bounded and never carries the request's key.
    #[test]
    fn a_provider_error_excerpt_is_bounded_and_keyless() {
        let excerpt = provider_error_excerpt("  bad key sk-secret-1 here  ", Some("sk-secret-1"));
        assert_eq!(excerpt, "bad key [redacted] here");

        let long = "é".repeat(PROVIDER_ERROR_BODY_MAX + 50);
        let excerpt = provider_error_excerpt(&long, None);
        assert_eq!(excerpt.chars().count(), PROVIDER_ERROR_BODY_MAX + 3);
        assert!(excerpt.ends_with("..."));

        assert_eq!(provider_error_excerpt("body", Some("")), "body");
    }

    /// Any non 2xx fails it, on both protocols, and a 401 says why.
    #[tokio::test]
    async fn every_non_2xx_fails_the_connection_test_on_both_protocols() {
        for status in [401u16, 403, 404, 405, 500] {
            let (base, _) = answering(status, r#"{"error":{"message":"b2c-why"}}"#).await;
            let svc = lazy_service();
            let anthropic = svc.test_anthropic(&base, Some("sk-test")).await;
            assert!(anthropic.is_err(), "anthropic {status} must fail");
            assert!(
                anthropic.unwrap_err().to_string().contains("b2c-why"),
                "anthropic {status} must say why"
            );
            let openai = svc.test_openai_compatible(&base, Some("sk-test")).await;
            assert!(openai.is_err(), "openai {status} must fail");
        }
    }

    // ---- P11c / D-43 timeout + per-operation breaker ----

    #[test]
    fn timeout_defaults_request_and_background() {
        let cfg = AiTimeoutConfig::default();
        // No overrides: request-scoped ~10s, background ~90s.
        assert_eq!(
            cfg.resolve_timeout("chat", "prov-1", false),
            Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS)
        );
        assert_eq!(
            cfg.resolve_timeout("chat", "prov-1", true),
            Duration::from_secs(DEFAULT_BACKGROUND_TIMEOUT_SECS)
        );
    }

    #[test]
    fn timeout_precedence_provider_over_operation_over_default() {
        let mut cfg = AiTimeoutConfig::default();
        cfg.per_operation_secs.insert("chat".to_string(), 45);
        // Operation override beats the (background) default.
        assert_eq!(
            cfg.resolve_timeout("chat", "prov-1", true),
            Duration::from_secs(45)
        );
        // A per-provider override is the most specific and wins over operation.
        cfg.per_provider_secs.insert("prov-1".to_string(), 60);
        assert_eq!(
            cfg.resolve_timeout("chat", "prov-1", true),
            Duration::from_secs(60)
        );
        // A different provider still resolves to the operation override.
        assert_eq!(
            cfg.resolve_timeout("chat", "prov-2", true),
            Duration::from_secs(45)
        );
    }

    #[test]
    fn timeout_clamped_to_epoch_ceiling() {
        let mut cfg = AiTimeoutConfig::default();
        cfg.per_operation_secs.insert("chat".to_string(), 100_000);
        assert_eq!(
            cfg.resolve_timeout("chat", "prov-1", true),
            Duration::from_secs(MAX_TIMEOUT_SECS),
            "a config value can never exceed the 150s background epoch budget"
        );
        // Zero is clamped up to the 1s floor (never an instantaneous timeout).
        cfg.per_operation_secs.insert("chat".to_string(), 0);
        assert_eq!(
            cfg.resolve_timeout("chat", "prov-1", true),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn breaker_window_default_and_override() {
        let mut cfg = AiTimeoutConfig::default();
        assert_eq!(
            cfg.resolve_breaker_window("chat"),
            Duration::from_secs(DEFAULT_BREAKER_WINDOW_SECS)
        );
        cfg.breaker_window_secs.insert("chat".to_string(), 600);
        assert_eq!(cfg.resolve_breaker_window("chat"), Duration::from_secs(600));
        // A window may exceed the timeout ceiling but is still bounded.
        cfg.breaker_window_secs.insert("chat".to_string(), 999_999);
        assert_eq!(
            cfg.resolve_breaker_window("chat"),
            Duration::from_secs(MAX_BREAKER_WINDOW_SECS)
        );
    }

    #[test]
    fn operation_config_keys_are_snake_case() {
        assert_eq!(AiOperationType::Chat.config_key(), "chat");
        assert_eq!(AiOperationType::Embedding.config_key(), "embedding");
        assert_eq!(
            AiOperationType::ImageGeneration.config_key(),
            "image_generation"
        );
        // The config key must match the serde representation so config authored
        // against the serialized enum lines up with resolution.
        for op in AiOperationType::ALL {
            let serde_name = serde_json::to_value(op).unwrap();
            assert_eq!(serde_name, serde_json::json!(op.config_key()));
        }
    }

    /// A provider service whose database is unreachable: the `ai_timeouts`
    /// read falls back to defaults at once.
    fn offline_service() -> AiProviderService {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://trovato:unused@127.0.0.1:1/unused")
            .expect("a lazy pool never connects here");
        AiProviderService::new(pool)
    }

    fn provider_at(base_url: &str, protocol: ProviderProtocol) -> ResolvedProvider {
        ResolvedProvider {
            config: AiProviderConfig {
                id: "send-path".to_string(),
                label: "Send path".to_string(),
                protocol,
                base_url: base_url.to_string(),
                api_key_env: String::new(),
                models: Vec::new(),
                rate_limit_rpm: 0,
                enabled: true,
            },
            api_key: None,
            model: "m".to_string(),
        }
    }

    /// **B50.** All three sends check the base URL themselves, so a provider
    /// row that reached `site_config` without the admin form cannot send to a
    /// private address: an IP literal never passes through the hardened
    /// resolver, so this check is the only thing that stops it.
    #[tokio::test]
    async fn every_send_refuses_a_private_ip_literal() {
        let service = offline_service();
        let caller = AiCaller::anonymous("test");
        let request = ChatRequest::system_and_user("s", "u", 16);
        for base in [
            "http://10.0.0.1/v1",
            "http://169.254.169.254",
            "http://[::1]:8080",
            "http://127.0.0.1:9",
            "http://localhost:11434/v1",
        ] {
            let provider = provider_at(base, ProviderProtocol::OpenAiCompatible);
            assert!(
                matches!(
                    service.chat_once(&provider, &caller, &request).await,
                    Err(AiCallError::BaseUrlRefused)
                ),
                "chat_once: {base}"
            );
            assert!(
                matches!(
                    service.chat_stream(&provider, &caller, &request).await,
                    Err(AiCallError::BaseUrlRefused)
                ),
                "chat_stream: {base}"
            );
            assert!(
                matches!(
                    service.embed_with(&provider, "text", &caller).await,
                    Err(AiCallError::BaseUrlRefused)
                ),
                "embed_with: {base}"
            );
        }
    }

    /// The test seam lets one exact loopback URL through and nothing else.
    #[test]
    fn the_loopback_test_seam_is_exact() {
        allow_loopback_provider_for_tests("http://127.0.0.1:61234");
        assert!(loopback_allowed_for_tests("http://127.0.0.1:61234/"));
        assert!(!loopback_allowed_for_tests("http://127.0.0.1:61235"));
        assert!(!loopback_allowed_for_tests("http://localhost:61234"));
        assert!(
            std::panic::catch_unwind(|| allow_loopback_provider_for_tests("http://10.0.0.1:80"))
                .is_err(),
            "a non-loopback address is never allowed"
        );
    }

    /// A refused send and an unreachable provider say nothing a person should
    /// not see: no URL, no provider text.
    #[test]
    fn user_messages_carry_no_detail() {
        let errors = [
            AiCallError::BaseUrlRefused,
            AiCallError::Provider {
                status: 500,
                detail: "secret provider text".into(),
            },
            AiCallError::Transport("error sending request for url (http://10.0.0.1/)".into()),
            AiCallError::Malformed("secret".into()),
        ];
        for error in errors {
            let shown = error.user_message();
            assert!(
                !shown.contains("secret") && !shown.contains("10.0.0.1"),
                "{shown}"
            );
        }
        let provider = AiCallError::Provider {
            status: 500,
            detail: "secret provider text".into(),
        };
        assert!(!provider.to_string().contains("secret"), "{provider}");
    }

    #[tokio::test]
    async fn per_operation_breakers_are_distinct_and_open_independently() {
        let svc = AiProviderService::new(
            sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato").unwrap(),
        );
        let chat = svc.breaker_for_operation("chat", Duration::from_secs(120));
        let embed = svc.breaker_for_operation("embedding", Duration::from_secs(120));
        // Same key returns the same instance; different keys are distinct.
        assert!(Arc::ptr_eq(
            &chat,
            &svc.breaker_for_operation("chat", Duration::from_secs(120))
        ));
        assert!(!Arc::ptr_eq(&chat, &embed));

        // Genuine repeated failures open the chat breaker (threshold 3)...
        for _ in 0..3 {
            let _: Result<(), _> = chat
                .call(|| async { Err::<(), String>("boom".to_string()) })
                .await;
        }
        assert_eq!(
            chat.state_name(),
            "open",
            "breaker opens on repeated failures"
        );
        // ...without touching the embedding breaker's independent window.
        assert_eq!(embed.state_name(), "closed");
    }

    // ---- truncate ----

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_adds_ellipsis() {
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn truncate_multibyte_characters() {
        // 3 CJK characters = 9 bytes but 3 chars
        let s = "日本語テスト";
        assert_eq!(truncate(s, 3), "日本語...");
    }

    #[test]
    fn truncate_empty_string() {
        assert_eq!(truncate("", 10), "");
    }

    // ---- cap_embedding_input ----

    #[test]
    fn cap_embedding_input_leaves_under_budget_untouched() {
        let text = "a short embedding input";
        let (input, truncated) = cap_embedding_input(text);
        assert_eq!(input.as_ref(), text);
        assert!(truncated.is_none());
        // Borrowed, not reallocated, when within budget.
        assert!(matches!(input, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn cap_embedding_input_at_exact_budget_untouched() {
        let text = "x".repeat(EMBEDDING_INPUT_MAX_CHARS);
        let (input, truncated) = cap_embedding_input(&text);
        assert_eq!(input.chars().count(), EMBEDDING_INPUT_MAX_CHARS);
        assert!(truncated.is_none());
    }

    #[test]
    fn cap_embedding_input_caps_over_budget_with_warning_signal() {
        let original = EMBEDDING_INPUT_MAX_CHARS + 500;
        let text = "y".repeat(original);
        let (input, truncated) = cap_embedding_input(&text);
        assert_eq!(input.chars().count(), EMBEDDING_INPUT_MAX_CHARS);
        assert_eq!(truncated, Some(original));
    }

    #[test]
    fn cap_embedding_input_truncates_on_char_boundary() {
        // Multibyte chars must not be split mid-codepoint.
        let text = "✓".repeat(EMBEDDING_INPUT_MAX_CHARS + 10);
        let (input, truncated) = cap_embedding_input(&text);
        assert_eq!(input.chars().count(), EMBEDDING_INPUT_MAX_CHARS);
        assert!(truncated.is_some());
        // Round-trips as valid UTF-8 (no panic / no replacement chars).
        assert!(input.chars().all(|c| c == '✓'));
    }

    // ---- parse_embedding_vector ----

    #[test]
    fn parse_embedding_vector_extracts_first() {
        let json = serde_json::json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3] },
                { "embedding": [9.0, 9.0, 9.0] }
            ],
            "model": "text-embedding-3-small"
        });
        let v = parse_embedding_vector(&json).unwrap();
        assert_eq!(v.len(), 3);
        assert!((v[0] - 0.1).abs() < 1e-6);
        assert!((v[2] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn parse_embedding_vector_rejects_missing() {
        let json = serde_json::json!({ "data": [] });
        assert!(parse_embedding_vector(&json).is_err());
        let json = serde_json::json!({ "object": "list" });
        assert!(parse_embedding_vector(&json).is_err());
    }

    #[test]
    fn parse_embedding_vector_rejects_empty_vector() {
        let json = serde_json::json!({ "data": [ { "embedding": [] } ] });
        assert!(parse_embedding_vector(&json).is_err());
    }

    // ---- validate_env_var_name ----

    #[test]
    fn env_var_empty_is_ok() {
        assert!(validate_env_var_name("").is_ok());
    }

    #[test]
    fn env_var_valid_names() {
        assert!(validate_env_var_name("OPENAI_API_KEY").is_ok());
        assert!(validate_env_var_name("ANTHROPIC_KEY").is_ok());
        assert!(validate_env_var_name("MY_KEY_123").is_ok());
        assert!(validate_env_var_name("X").is_ok());
    }

    #[test]
    fn env_var_rejects_lowercase() {
        assert!(validate_env_var_name("openai_key").is_err());
    }

    #[test]
    fn env_var_rejects_starts_with_digit() {
        assert!(validate_env_var_name("123KEY").is_err());
    }

    #[test]
    fn env_var_rejects_special_chars() {
        assert!(validate_env_var_name("KEY-NAME").is_err());
        assert!(validate_env_var_name("KEY.NAME").is_err());
    }

    #[test]
    fn env_var_rejects_denied_names() {
        assert!(validate_env_var_name("DATABASE_URL").is_err());
        assert!(validate_env_var_name("JWT_SECRET").is_err());
        assert!(validate_env_var_name("SMTP_PASSWORD").is_err());
    }

    // ---- validate_base_url ----

    #[test]
    fn base_url_valid_https() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
    }

    #[test]
    fn base_url_valid_http() {
        assert!(validate_base_url("http://localhost:11434/v1").is_err()); // localhost is blocked
        assert!(validate_base_url("http://ollama.example.com/v1").is_ok());
    }

    #[test]
    fn base_url_rejects_non_http_schemes() {
        assert!(validate_base_url("ftp://example.com").is_err());
        assert!(validate_base_url("file:///etc/passwd").is_err());
        assert!(validate_base_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn base_url_rejects_private_ips() {
        assert!(validate_base_url("http://127.0.0.1/v1").is_err());
        assert!(validate_base_url("http://10.0.0.1/v1").is_err());
        assert!(validate_base_url("http://192.168.1.1/v1").is_err());
        assert!(validate_base_url("http://172.16.0.1/v1").is_err());
        assert!(validate_base_url("http://169.254.169.254/latest").is_err());
    }

    /// The bracketed IPv6 forms. `host_str()` keeps the brackets, so none of
    /// these ever reached the address check: an administrator URL (or anything
    /// that could set one) pointed straight at a loopback sidecar.
    #[test]
    fn base_url_rejects_ipv6_literals() {
        for blocked in [
            "http://[::1]/v1",
            "http://[::1]:8080/v1",
            "http://[fd00::1]/v1",
            "http://[fe80::1]/v1",
            "http://[::ffff:127.0.0.1]/v1",
            "http://[::ffff:169.254.169.254]/latest",
        ] {
            assert!(
                validate_base_url(blocked).is_err(),
                "expected {blocked} to be rejected"
            );
        }
    }

    /// A public IPv6 literal still works: the fix checks IPv6, it does not ban it.
    #[test]
    fn base_url_allows_public_ipv6_literals() {
        assert!(validate_base_url("https://[2606:4700:4700::1111]/v1").is_ok());
    }

    /// The ranges only the plugin fence used to block are blocked here too, now
    /// that both sides share one list.
    #[test]
    fn base_url_rejects_the_ranges_this_copy_used_to_miss() {
        for blocked in [
            "http://100.64.0.1/v1",      // carrier-grade NAT
            "http://255.255.255.255/v1", // broadcast
            "http://224.0.0.1/v1",       // multicast
            "http://198.18.0.1/v1",      // benchmarking
            "http://192.0.0.1/v1",       // IETF protocol assignments
        ] {
            assert!(
                validate_base_url(blocked).is_err(),
                "expected {blocked} to be rejected"
            );
        }
    }

    /// A `.internal` or `.local` name passes this string check: the hostname
    /// list is deliberately narrower than the plugin fence's. The address
    /// behind the name is checked when the request connects, where the provider
    /// client's hardened resolver refuses the connection if the name resolves to
    /// an address `net_policy` refuses (so a name pointing at a private or
    /// loopback model server is refused there). Pinning the string rule in a
    /// test so the narrower list is not "fixed" into the plugin fence's by a
    /// later reader.
    #[test]
    fn base_url_allows_internal_hostnames_deliberately() {
        assert!(validate_base_url("http://ollama.internal/v1").is_ok());
        assert!(validate_base_url("http://ollama.local/v1").is_ok());
    }

    #[test]
    fn base_url_rejects_cloud_metadata() {
        assert!(validate_base_url("http://metadata.google.internal/v1").is_err());
    }

    #[test]
    fn base_url_rejects_garbage() {
        assert!(validate_base_url("not-a-url").is_err());
    }

    // ---- the shared address policy, as this file reaches it ----

    #[test]
    fn private_ip_detection() {
        assert!(is_disallowed_ip("127.0.0.1".parse().unwrap()));
        assert!(is_disallowed_ip("10.0.0.1".parse().unwrap()));
        assert!(is_disallowed_ip("192.168.1.1".parse().unwrap()));
        assert!(is_disallowed_ip("172.16.0.1".parse().unwrap()));
        assert!(is_disallowed_ip("169.254.1.1".parse().unwrap()));
        assert!(is_disallowed_ip("::1".parse().unwrap()));
        assert!(!is_disallowed_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_disallowed_ip("1.1.1.1".parse().unwrap()));
    }

    // ---- serde roundtrips ----

    #[test]
    fn ai_operation_type_serde_roundtrip() {
        for op in AiOperationType::ALL {
            let json = serde_json::to_value(op).unwrap();
            let back: AiOperationType = serde_json::from_value(json).unwrap();
            assert_eq!(*op, back);
        }
    }

    #[test]
    fn provider_protocol_serde_roundtrip() {
        let protos = [
            ProviderProtocol::OpenAiCompatible,
            ProviderProtocol::Anthropic,
        ];
        for p in protos {
            let json = serde_json::to_value(p).unwrap();
            let back: ProviderProtocol = serde_json::from_value(json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn ai_defaults_serde_roundtrip() {
        let mut defaults = AiDefaults::default();
        defaults
            .defaults
            .insert(AiOperationType::Chat, "provider-1".to_string());
        defaults
            .defaults
            .insert(AiOperationType::Embedding, "provider-2".to_string());

        let json = serde_json::to_value(&defaults).unwrap();
        let back: AiDefaults = serde_json::from_value(json).unwrap();
        assert_eq!(back.defaults.len(), 2);
        assert_eq!(back.defaults[&AiOperationType::Chat], "provider-1");
        assert_eq!(back.defaults[&AiOperationType::Embedding], "provider-2");
    }

    #[test]
    fn ai_provider_config_serde_roundtrip() {
        let config = AiProviderConfig {
            id: "test-id".to_string(),
            label: "Test Provider".to_string(),
            protocol: ProviderProtocol::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            models: vec![OperationModel {
                operation: AiOperationType::Chat,
                model: "gpt-4o".to_string(),
            }],
            rate_limit_rpm: 60,
            enabled: true,
        };

        let json = serde_json::to_value(&config).unwrap();
        let back: AiProviderConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "test-id");
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.models[0].model, "gpt-4o");
    }

    // ---- mask_key_ref ----

    #[test]
    fn mask_key_ref_empty_env() {
        let config = AiProviderConfig {
            id: String::new(),
            label: String::new(),
            protocol: ProviderProtocol::OpenAiCompatible,
            base_url: String::new(),
            api_key_env: String::new(),
            models: vec![],
            rate_limit_rpm: 0,
            enabled: false,
        };
        assert_eq!(
            AiProviderService::mask_key_ref(&config),
            "(no env var configured)"
        );
    }

    #[test]
    fn mask_key_ref_with_unset_var() {
        let config = AiProviderConfig {
            id: String::new(),
            label: String::new(),
            protocol: ProviderProtocol::OpenAiCompatible,
            base_url: String::new(),
            api_key_env: "VERY_UNLIKELY_UNSET_VAR_XYZ_12345".to_string(),
            models: vec![],
            rate_limit_rpm: 0,
            enabled: false,
        };
        let result = AiProviderService::mask_key_ref(&config);
        assert!(result.contains("(not set)"), "got: {result}");
    }
}
