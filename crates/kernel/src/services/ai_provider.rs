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
    /// The model identifier that produced the vector.
    pub model: String,
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

    let Some(host) = parsed.host_str() else {
        return Err("URL must include a host.".to_string());
    };

    // Check if host is a literal IP in a blocked range
    if let Ok(ip) = host.parse::<IpAddr>()
        && is_private_ip(ip)
    {
        return Err("URL must not target private, loopback, or link-local addresses.".to_string());
    }

    // Block well-known local hostnames
    if host == "localhost" || host.ends_with(".localhost") {
        return Err("URL must not target private, loopback, or link-local addresses.".to_string());
    }

    // Check for common metadata endpoint hostnames
    if host == "metadata.google.internal" || host == "metadata.google.com" {
        return Err("URL must not target cloud metadata services.".to_string());
    }

    Ok(())
}

/// Check if an IP address is in a private, loopback, or link-local range.
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()         // 127.0.0.0/8
                || v4.is_private()   // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local() // 169.254.0.0/16
                || v4.octets()[0] == 0 // 0.0.0.0/8
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() // ::1
                || v6.is_unspecified() // ::
                // fe80::/10 link-local — check first 10 bits
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // fc00::/7 unique-local — check first 7 bits
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
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
}

impl AiProviderService {
    /// Create a new AI provider service.
    pub fn new(db: PgPool) -> Self {
        // The client-level timeout is the request-scoped default and the backstop
        // for any path that does not set a per-request timeout (embedding,
        // connection test). The AI request path overrides it per request with the
        // per-operation / per-provider timeout resolved from `ai_timeouts`
        // (P11c / D-43); a `RequestBuilder::timeout` overrides this default.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();
        Self {
            db,
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
    // Embeddings
    // -------------------------------------------------------------------------

    /// Generate an embedding vector for `text` using the configured
    /// `Embedding` provider.
    ///
    /// Returns `Ok(None)` when no embedding provider is configured, the
    /// provider is disabled, or the provider's protocol exposes no embeddings
    /// API (Anthropic has none). Returns `Err` only on a transport, HTTP-status,
    /// or response-parse failure once a provider *is* resolved — callers treat
    /// both `None` and `Err` as "no embedding available" and degrade
    /// gracefully (no-match on read, skipped index on write).
    ///
    /// The resolved model is returned in [`EmbeddingResult`] so that write-time
    /// (index) and read-time (gather `SemanticSimilarity`) callers use the same
    /// model — `item_embeddings` similarity search filters by model.
    ///
    /// # Panics
    /// Does not panic.
    pub async fn embed(&self, text: &str) -> Result<Option<EmbeddingResult>> {
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

        // SSRF defense-in-depth: re-validate the base URL before the outbound
        // request (mirrors `test_connection`).
        if let Err(msg) = validate_base_url(&resolved.config.base_url) {
            anyhow::bail!("invalid embedding provider base_url: {msg}");
        }

        let url = format!(
            "{}/embeddings",
            resolved.config.base_url.trim_end_matches('/')
        );

        // Cap outbound embedding input. This is the universal choke point —
        // both the index path (whole-item text) and the read path (query text)
        // flow through here. Over-budget input would otherwise be rejected by
        // the provider (silently leaving an item unsearchable) and unbounded
        // payloads are a cost/DoS surface.
        let (input, truncated_from) = cap_embedding_input(text);
        if let Some(original) = truncated_from {
            tracing::warn!(
                original_chars = original,
                capped_chars = EMBEDDING_INPUT_MAX_CHARS,
                "embedding input exceeds budget; truncating before request"
            );
        }

        let request_body = serde_json::json!({
            "model": resolved.model,
            "input": input.as_ref(),
        });

        let mut req = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .json(&request_body);
        if let Some(ref key) = resolved.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send().await.context("embedding request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "embedding provider returned HTTP {status}: {}",
                truncate(&body, 200)
            );
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse embedding response")?;

        let vector =
            parse_embedding_vector(&json).context("embedding response had no usable vector")?;

        Ok(Some(EmbeddingResult {
            vector,
            model: resolved.model,
        }))
    }

    // -------------------------------------------------------------------------
    // Connection test
    // -------------------------------------------------------------------------

    /// Test the connection to a provider endpoint.
    ///
    /// - `OpenAiCompatible`: `GET {base_url}/models`
    /// - `Anthropic`: `GET {base_url}` (HEAD-like connectivity check)
    ///
    /// Response bodies are truncated to 200 characters for safety.
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
                message: truncate(&msg, 200),
                latency_ms,
            },
            Err(e) => ConnectionTestResult {
                success: false,
                message: truncate(&e.to_string(), 200),
                latency_ms,
            },
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
        let resp = req.send().await.context("request failed")?;
        let status = resp.status();
        if status.is_success() {
            Ok(format!("Connected successfully (HTTP {status})"))
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {status}: {body}")
        }
    }

    /// Test an Anthropic endpoint with a GET to verify connectivity.
    ///
    /// Uses a simple GET request instead of POST /messages to avoid incurring
    /// API billing charges on every test. Any HTTP response (including 404 or
    /// 405) proves network connectivity and valid authentication headers.
    async fn test_anthropic(&self, base_url: &str, api_key: Option<&str>) -> Result<String> {
        let url = base_url.trim_end_matches('/').to_string();
        let mut req = self
            .http
            .get(&url)
            .header("anthropic-version", "2023-06-01");
        if let Some(key) = api_key {
            req = req.header("x-api-key", key);
        }
        let resp = req.send().await.context("request failed")?;
        let status = resp.status();
        // Any response proves connectivity. 404/405 are expected since we're
        // hitting the base URL, not a real endpoint. Only network-level
        // failures (DNS, TLS, timeout) are treated as errors.
        Ok(format!("Connected successfully (HTTP {status})"))
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

    #[test]
    fn base_url_rejects_cloud_metadata() {
        assert!(validate_base_url("http://metadata.google.internal/v1").is_err());
    }

    #[test]
    fn base_url_rejects_garbage() {
        assert!(validate_base_url("not-a-url").is_err());
    }

    // ---- is_private_ip ----

    #[test]
    fn private_ip_detection() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("169.254.1.1".parse().unwrap()));
        assert!(is_private_ip("::1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse().unwrap()));
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
