//! AI Chat service for streaming chatbot with RAG context.
//!
//! Provides a kernel-side chat service that sends streaming requests to AI
//! providers via `AiProviderService`. SSE streaming cannot cross the WASM
//! boundary, so this lives in the kernel rather than as a plugin.
//!
//! Configuration is stored in `site_config` under key `"ai_chat_config"`.
//! Conversation history is stored in the user's Redis session.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::models::SiteConfig;
use crate::models::stage::LIVE_STAGE_ID;
use crate::search::SearchService;
use crate::services::ai_provider::{
    AiOperationType, AiProviderService, ProviderProtocol, ResolvedProvider,
};

// =============================================================================
// Configuration types
// =============================================================================

/// Site config key for chat configuration.
const CONFIG_KEY: &str = "ai_chat_config";

/// Chat service configuration stored in `site_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConfig {
    /// System prompt template. `{site_name}` is replaced at runtime.
    /// The single-variable approach is intentional — site_name is the only
    /// dynamic value needed in the system prompt for the chatbot use case.
    pub system_prompt: String,
    /// Whether RAG context injection is enabled.
    pub rag_enabled: bool,
    /// Maximum number of search results to inject as context.
    pub rag_max_results: u32,
    /// Minimum relevance score for RAG results.
    pub rag_min_score: f32,
    /// Maximum conversation history turns to include.
    pub max_history_turns: u32,
    /// Rate limit: requests per hour per user.
    pub rate_limit_per_hour: u32,
    /// Maximum tokens for the AI response.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Temperature for AI response generation (0.0–2.0).
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

/// Default max tokens for chat responses.
fn default_max_tokens() -> u32 {
    1024
}

/// Default temperature for chat responses.
fn default_temperature() -> f32 {
    0.7
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            system_prompt: "You are a helpful assistant for the {site_name} website. Answer questions about the site's content based on the context provided. If you don't know the answer, say so.".to_string(),
            rag_enabled: true,
            rag_max_results: 5,
            rag_min_score: 0.1,
            max_history_turns: 5,
            rate_limit_per_hour: 20,
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
        }
    }
}

/// Role in a conversation turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    /// User message.
    User,
    /// Assistant response.
    Assistant,
}

impl ChatRole {
    /// Return the role as a lowercase string for AI provider messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// A single conversation turn stored in the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    /// Role: user or assistant.
    pub role: ChatRole,
    /// Message content.
    pub content: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
}

/// An event emitted by the streaming chat response.
#[derive(Debug)]
pub enum ChatStreamEvent {
    /// A token of text from the AI response.
    Token(String),
    /// Stream completed with usage information.
    Done {
        /// Token usage from the provider.
        prompt_tokens: u32,
        /// Completion tokens from the provider.
        completion_tokens: u32,
        /// Total tokens from the provider.
        total_tokens: u32,
    },
    /// An error occurred during streaming.
    Error(String),
}

/// A message in the AI conversation (role + content).
#[derive(Debug, Clone, Serialize)]
pub struct AiMessage {
    /// Message role: "system", "user", or "assistant".
    pub role: String,
    /// Message content.
    pub content: String,
}

// Role constants to avoid magic strings throughout the module.
const ROLE_SYSTEM: &str = "system";
const ROLE_USER: &str = "user";

// =============================================================================
// Service
// =============================================================================

/// AI chat service for streaming chatbot with RAG context.
pub struct ChatService {
    db: PgPool,
    ai_providers: Arc<AiProviderService>,
    search: Arc<SearchService>,
}

impl fmt::Debug for ChatService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatService").finish()
    }
}

impl ChatService {
    /// Create a new chat service.
    pub fn new(
        db: PgPool,
        ai_providers: Arc<AiProviderService>,
        search: Arc<SearchService>,
    ) -> Self {
        Self {
            db,
            ai_providers,
            search,
        }
    }

    /// Get the database pool.
    pub fn db(&self) -> &PgPool {
        &self.db
    }

    /// Get the AI provider service.
    pub fn ai_providers(&self) -> &Arc<AiProviderService> {
        &self.ai_providers
    }

    // -------------------------------------------------------------------------
    // Config
    // -------------------------------------------------------------------------

    /// Load chat configuration from `site_config`.
    pub async fn load_config(&self) -> Result<ChatConfig> {
        let value = SiteConfig::get(&self.db, CONFIG_KEY)
            .await
            .context("failed to read ai_chat_config")?;
        match value {
            Some(v) => serde_json::from_value(v).context("failed to parse ai_chat_config"),
            None => Ok(ChatConfig::default()),
        }
    }

    /// Save chat configuration to `site_config`.
    ///
    /// Validates that numeric fields are finite and within range before saving.
    pub async fn save_config(&self, config: &ChatConfig) -> Result<()> {
        // Validate float fields are finite (NaN/infinity pass through f32::clamp).
        anyhow::ensure!(
            config.temperature.is_finite(),
            "temperature must be a finite number"
        );
        anyhow::ensure!(
            config.rag_min_score.is_finite(),
            "rag_min_score must be a finite number"
        );

        // Range validation (defense in depth — admin form also clamps these).
        anyhow::ensure!(
            (0.0..=2.0).contains(&config.temperature),
            "temperature must be between 0.0 and 2.0"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&config.rag_min_score),
            "rag_min_score must be between 0.0 and 1.0"
        );
        anyhow::ensure!(
            config.max_tokens >= 64 && config.max_tokens <= 16384,
            "max_tokens must be between 64 and 16384"
        );
        anyhow::ensure!(
            config.rag_max_results >= 1 && config.rag_max_results <= 20,
            "rag_max_results must be between 1 and 20"
        );
        anyhow::ensure!(
            config.max_history_turns <= 20,
            "max_history_turns must be at most 20"
        );
        anyhow::ensure!(
            config.rate_limit_per_hour <= 1000,
            "rate_limit_per_hour must be at most 1000"
        );

        let value = serde_json::to_value(config).context("failed to serialize ai_chat_config")?;
        SiteConfig::set(&self.db, CONFIG_KEY, value)
            .await
            .context("failed to save ai_chat_config")
    }

    // -------------------------------------------------------------------------
    // RAG context
    // -------------------------------------------------------------------------

    /// Search for relevant content and format as context text.
    ///
    /// Returns an empty string if RAG is disabled or no results match.
    pub async fn search_for_context(
        &self,
        query: &str,
        config: &ChatConfig,
        user_id: Option<Uuid>,
    ) -> String {
        if !config.rag_enabled {
            return String::new();
        }

        let stage_ids = vec![LIVE_STAGE_ID];
        let results = match self
            .search
            .search(
                query,
                &stage_ids,
                user_id,
                i64::from(config.rag_max_results),
                0,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "RAG search failed, continuing without context");
                return String::new();
            }
        };

        let filtered: Vec<_> = results
            .results
            .into_iter()
            .filter(|r| r.rank >= config.rag_min_score)
            .collect();

        if filtered.is_empty() {
            return String::new();
        }

        let mut context = String::from("Relevant site content:\n\n");
        for (i, r) in filtered.iter().enumerate() {
            use std::fmt::Write;
            // Titles are user-controlled content — truncate to limit prompt injection surface.
            let title = truncate_str(&r.title, 200);
            // Infallible: writing to String
            writeln!(context, "{}. {} (/item/{})", i + 1, title, r.id).unwrap_or_default();
            if let Some(ref snippet) = r.snippet {
                // Strip HTML tags from snippet for clean AI context.
                let clean = strip_html_tags(snippet);
                // Truncate snippets to limit prompt size and injection surface.
                let trimmed = truncate_str(&clean, 500);
                // Infallible: writing to String
                write!(context, "   {trimmed}\n\n").unwrap_or_default();
            }
        }
        context
    }

    // -------------------------------------------------------------------------
    // Message assembly
    // -------------------------------------------------------------------------

    /// Build the full message list for the AI provider.
    ///
    /// Order: system prompt (with RAG context) → history turns → user message.
    pub fn build_messages(
        &self,
        system_prompt: &str,
        rag_context: &str,
        history: &[ChatTurn],
        user_message: &str,
    ) -> Vec<AiMessage> {
        let mut messages = Vec::new();

        // System prompt with optional RAG context, delimited to reduce
        // prompt injection surface from user-controlled search results.
        let mut system = system_prompt.to_string();
        if !rag_context.is_empty() {
            system.push_str("\n\n---BEGIN CONTEXT---\n");
            system.push_str(rag_context);
            system.push_str("---END CONTEXT---");
        }
        messages.push(AiMessage {
            role: ROLE_SYSTEM.to_string(),
            content: system,
        });

        // Conversation history
        for turn in history {
            messages.push(AiMessage {
                role: turn.role.as_str().to_string(),
                content: turn.content.clone(),
            });
        }

        // Current user message
        messages.push(AiMessage {
            role: ROLE_USER.to_string(),
            content: user_message.to_string(),
        });

        messages
    }

    // -------------------------------------------------------------------------
    // Streaming execution
    // -------------------------------------------------------------------------

    /// Resolve the AI provider for chat operations.
    ///
    /// Returns the resolved provider so the caller can perform budget checks
    /// before starting the stream.
    pub async fn resolve_chat_provider(&self) -> Result<ResolvedProvider> {
        self.ai_providers
            .resolve_provider(AiOperationType::Chat, None)
            .await
            .context("failed to resolve chat provider")?
            .context("no chat provider configured")
    }

    /// Execute a streaming chat request against the given resolved provider.
    ///
    /// The caller must resolve the provider first via [`Self::resolve_chat_provider`]
    /// and perform budget checks before calling this method. This avoids a
    /// TOCTOU race where the provider could change between budget check and
    /// request execution.
    ///
    /// Returns a pinned stream of `ChatStreamEvent`s. The caller wraps this
    /// in an SSE response.
    pub async fn execute_streaming(
        &self,
        messages: Vec<AiMessage>,
        config: &ChatConfig,
        resolved: &ResolvedProvider,
    ) -> Result<(
        Pin<Box<dyn futures_core::Stream<Item = ChatStreamEvent> + Send>>,
        StreamMeta,
    )> {
        let protocol = resolved.config.protocol;
        let provider_id = resolved.config.id.clone();
        let model = resolved.model.clone();

        let (url, body_str, headers) = match protocol {
            ProviderProtocol::OpenAiCompatible => {
                build_streaming_openai_request(resolved, &messages, config)
            }
            ProviderProtocol::Anthropic => {
                build_streaming_anthropic_request(resolved, &messages, config)
            }
        };

        debug!(url = %url, protocol = ?protocol, "sending streaming chat request");

        let mut req = self
            .ai_providers
            .http()
            .post(&url)
            .timeout(Duration::from_secs(120))
            .header("content-type", "application/json")
            .body(body_str);

        for (key, value) in &headers {
            req = req.header(key.as_str(), value.as_str());
        }

        let response = req.send().await.context("streaming chat request failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let truncated = if body.len() > 200 {
                let mut end = 200;
                while end > 0 && !body.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &body[..end])
            } else {
                body
            };
            anyhow::bail!("Provider returned HTTP {status}: {truncated}");
        }

        let meta = StreamMeta { provider_id, model };

        let stream: Pin<Box<dyn futures_core::Stream<Item = ChatStreamEvent> + Send>> =
            match protocol {
                ProviderProtocol::OpenAiCompatible => Box::pin(parse_openai_stream(response)),
                ProviderProtocol::Anthropic => Box::pin(parse_anthropic_stream(response)),
            };

        Ok((stream, meta))
    }
}

/// Metadata about the streaming request (for usage logging after completion).
pub struct StreamMeta {
    /// Provider configuration ID.
    pub provider_id: String,
    /// Model identifier.
    pub model: String,
}

// =============================================================================
// Streaming request builders
// =============================================================================

/// Build a streaming OpenAI-compatible chat completions request.
fn build_streaming_openai_request(
    resolved: &ResolvedProvider,
    messages: &[AiMessage],
    config: &ChatConfig,
) -> (String, String, Vec<(String, String)>) {
    let url = format!(
        "{}/chat/completions",
        resolved.config.base_url.trim_end_matches('/')
    );

    let msg_values: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content,
            })
        })
        .collect();

    let body = serde_json::json!({
        "model": &resolved.model,
        "messages": msg_values,
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
    });

    let mut headers = Vec::new();
    if let Some(ref key) = resolved.api_key {
        headers.push(("authorization".to_string(), format!("Bearer {key}")));
    }

    // Infallible: serde_json::Value serialization to string cannot fail.
    let body_str = serde_json::to_string(&body).unwrap_or_default();
    (url, body_str, headers)
}

/// Build a streaming Anthropic Messages API request.
fn build_streaming_anthropic_request(
    resolved: &ResolvedProvider,
    messages: &[AiMessage],
    config: &ChatConfig,
) -> (String, String, Vec<(String, String)>) {
    let url = format!(
        "{}/messages",
        resolved.config.base_url.trim_end_matches('/')
    );

    // Anthropic: system messages go in a separate "system" field.
    let mut system_parts: Vec<&str> = Vec::new();
    let msg_values: Vec<serde_json::Value> = messages
        .iter()
        .filter_map(|m| {
            if m.role == ROLE_SYSTEM {
                system_parts.push(&m.content);
                None
            } else {
                Some(serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                }))
            }
        })
        .collect();

    let mut body = serde_json::json!({
        "model": &resolved.model,
        "messages": msg_values,
        "stream": true,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
    });

    if !system_parts.is_empty() {
        body["system"] = serde_json::json!(system_parts.join("\n"));
    }

    let mut headers = Vec::new();
    if let Some(ref key) = resolved.api_key {
        headers.push(("x-api-key".to_string(), key.clone()));
    }
    headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));

    // Infallible: serde_json::Value serialization to string cannot fail.
    let body_str = serde_json::to_string(&body).unwrap_or_default();
    (url, body_str, headers)
}

// =============================================================================
// SSE stream parsers
// =============================================================================

/// Maximum SSE buffer size (1 MiB) to prevent unbounded memory growth from
/// a misbehaving or malicious provider.
const MAX_STREAM_BUFFER: usize = 1024 * 1024;

/// Per-chunk idle timeout for provider streams. If no data arrives within
/// this duration, the stream is considered stalled and an error is emitted.
const CHUNK_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Parse an OpenAI-compatible SSE stream into `ChatStreamEvent`s.
///
/// Each SSE `data:` line is a JSON object with `choices[0].delta.content`.
/// The final chunk (with `stream_options.include_usage`) has usage data.
/// The stream terminates with `data: [DONE]`.
fn parse_openai_stream(
    response: reqwest::Response,
) -> impl futures_core::Stream<Item = ChatStreamEvent> + Send {
    async_stream::stream! {
        use tokio_stream::StreamExt;
        let mut byte_stream = response.bytes_stream();
        // Accumulate raw bytes to avoid corrupting multi-byte UTF-8 sequences
        // that may be split across chunk boundaries.
        let mut raw_buf: Vec<u8> = Vec::new();
        let mut prompt_tokens: u32 = 0;
        let mut completion_tokens: u32 = 0;

        loop {
            let chunk_result = match tokio::time::timeout(
                CHUNK_IDLE_TIMEOUT,
                byte_stream.next(),
            ).await {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(_) => {
                    yield ChatStreamEvent::Error("Provider response timed out".to_string());
                    return;
                }
            };

            let bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    yield ChatStreamEvent::Error(format!("Stream read error: {e}"));
                    return;
                }
            };

            raw_buf.extend_from_slice(&bytes);

            if raw_buf.len() > MAX_STREAM_BUFFER {
                yield ChatStreamEvent::Error("Stream buffer exceeded maximum size".to_string());
                return;
            }

            // Decode as much valid UTF-8 as possible from the front of the buffer.
            // Any trailing incomplete multi-byte sequence stays in raw_buf for the
            // next chunk to complete.
            let valid_up_to = match std::str::from_utf8(&raw_buf) {
                Ok(_) => raw_buf.len(),
                Err(e) => e.valid_up_to(),
            };
            if valid_up_to == 0 {
                continue;
            }
            // Infallible: from_utf8 confirmed bytes [..valid_up_to] are valid UTF-8.
            let text = std::str::from_utf8(&raw_buf[..valid_up_to]).unwrap_or_default();
            let text = text.to_string();
            raw_buf = raw_buf[valid_up_to..].to_vec();

            // We need a mutable string buffer for line splitting across decode calls.
            // Re-use the decoded text directly since we process all complete lines.
            let mut line_buf = text;

            // Process complete SSE lines
            while let Some(line_end) = line_buf.find('\n') {
                let line = line_buf[..line_end].trim_end_matches('\r').to_string();
                line_buf = line_buf[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        let total = prompt_tokens.saturating_add(completion_tokens);
                        yield ChatStreamEvent::Done {
                            prompt_tokens,
                            completion_tokens,
                            total_tokens: total,
                        };
                        return;
                    }

                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        // Extract token text
                        if let Some(content) = json["choices"]
                            .get(0)
                            .and_then(|c| c["delta"]["content"].as_str())
                            && !content.is_empty()
                        {
                            yield ChatStreamEvent::Token(content.to_string());
                        }

                        // Extract usage from final chunk
                        if let Some(usage) = json.get("usage") {
                            prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                            completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
                        }
                    }
                }
            }

            // Put any partial line back into raw_buf for the next iteration.
            if !line_buf.is_empty() {
                let mut remaining = line_buf.into_bytes();
                remaining.extend_from_slice(&raw_buf);
                raw_buf = remaining;
            }
        }

        // Stream ended without [DONE] — emit done with whatever we have
        let total = prompt_tokens.saturating_add(completion_tokens);
        yield ChatStreamEvent::Done {
            prompt_tokens,
            completion_tokens,
            total_tokens: total,
        };
    }
}

/// Parse an Anthropic SSE stream into `ChatStreamEvent`s.
///
/// Anthropic uses named events: `content_block_delta` for text tokens,
/// `message_delta` for stop reason and usage, `message_stop` for stream end,
/// and `error` for provider-side errors.
fn parse_anthropic_stream(
    response: reqwest::Response,
) -> impl futures_core::Stream<Item = ChatStreamEvent> + Send {
    async_stream::stream! {
        use tokio_stream::StreamExt;
        let mut byte_stream = response.bytes_stream();
        // Accumulate raw bytes to avoid corrupting multi-byte UTF-8 sequences.
        let mut raw_buf: Vec<u8> = Vec::new();
        let mut completion_tokens: u32 = 0;
        let mut prompt_tokens: u32 = 0;

        loop {
            let chunk_result = match tokio::time::timeout(
                CHUNK_IDLE_TIMEOUT,
                byte_stream.next(),
            ).await {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(_) => {
                    yield ChatStreamEvent::Error("Provider response timed out".to_string());
                    return;
                }
            };

            let bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    yield ChatStreamEvent::Error(format!("Stream read error: {e}"));
                    return;
                }
            };

            raw_buf.extend_from_slice(&bytes);

            if raw_buf.len() > MAX_STREAM_BUFFER {
                yield ChatStreamEvent::Error("Stream buffer exceeded maximum size".to_string());
                return;
            }

            // Decode valid UTF-8 from the front, leaving trailing partial sequences.
            let valid_up_to = match std::str::from_utf8(&raw_buf) {
                Ok(_) => raw_buf.len(),
                Err(e) => e.valid_up_to(),
            };
            if valid_up_to == 0 {
                continue;
            }
            // Infallible: from_utf8 confirmed bytes [..valid_up_to] are valid UTF-8.
            let text = std::str::from_utf8(&raw_buf[..valid_up_to]).unwrap_or_default();
            let text = text.to_string();
            raw_buf = raw_buf[valid_up_to..].to_vec();

            let mut buffer = text;

            // Process complete SSE blocks (double newline separated)
            while let Some(block_end) = buffer.find("\n\n") {
                let block = buffer[..block_end].to_string();
                buffer = buffer[block_end + 2..].to_string();

                let mut event_type = "";
                let mut data_str = String::new();

                for line in block.lines() {
                    if let Some(et) = line.strip_prefix("event: ") {
                        event_type = match et.trim() {
                            "content_block_delta" => "content_block_delta",
                            "message_delta" => "message_delta",
                            "message_stop" => "message_stop",
                            "message_start" => "message_start",
                            "error" => "error",
                            _ => "",
                        };
                    } else if let Some(d) = line.strip_prefix("data: ") {
                        data_str = d.trim().to_string();
                    }
                }

                match event_type {
                    "message_start" => {
                        // Extract input_tokens from message_start
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data_str)
                            && let Some(usage) = json["message"].get("usage")
                        {
                            prompt_tokens = usage["input_tokens"].as_u64().unwrap_or(0) as u32;
                        }
                    }
                    "content_block_delta" => {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data_str)
                            && let Some(text) = json["delta"]["text"].as_str()
                            && !text.is_empty()
                        {
                            yield ChatStreamEvent::Token(text.to_string());
                        }
                    }
                    "message_delta" => {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data_str)
                            && let Some(usage) = json.get("usage")
                        {
                            completion_tokens = usage["output_tokens"].as_u64().unwrap_or(0) as u32;
                        }
                    }
                    "message_stop" => {
                        let total = prompt_tokens.saturating_add(completion_tokens);
                        yield ChatStreamEvent::Done {
                            prompt_tokens,
                            completion_tokens,
                            total_tokens: total,
                        };
                        return;
                    }
                    // F18: Handle Anthropic error events instead of silently dropping.
                    "error" => {
                        let msg = serde_json::from_str::<serde_json::Value>(&data_str)
                            .ok()
                            .and_then(|j| j["error"]["message"].as_str().map(String::from))
                            .unwrap_or_else(|| "Unknown provider error".to_string());
                        yield ChatStreamEvent::Error(msg);
                        return;
                    }
                    _ => {}
                }
            }

            // Put any partial block back into raw_buf for the next iteration.
            if !buffer.is_empty() {
                let mut remaining = buffer.into_bytes();
                remaining.extend_from_slice(&raw_buf);
                raw_buf = remaining;
            }
        }

        // Stream ended without message_stop
        let total = prompt_tokens.saturating_add(completion_tokens);
        yield ChatStreamEvent::Done {
            prompt_tokens,
            completion_tokens,
            total_tokens: total,
        };
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Strip all HTML tags from a string, keeping only text content.
///
/// Uses `ammonia` with an empty allowed-tag set, which properly handles
/// edge cases (unclosed tags, comments, self-closing tags) that a naive
/// char-by-char stripper would miss.
fn strip_html_tags(s: &str) -> String {
    ammonia::Builder::default()
        .tags(std::collections::HashSet::new())
        .clean(s)
        .to_string()
}

/// Truncate a string to at most `max_len` characters on a char boundary.
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn chat_config_default_roundtrip() {
        let config = ChatConfig::default();
        let json = serde_json::to_value(&config).unwrap();
        let back: ChatConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.rag_max_results, 5);
        assert_eq!(back.max_history_turns, 5);
        assert_eq!(back.rate_limit_per_hour, 20);
        assert!(back.rag_enabled);
        assert_eq!(back.max_tokens, 1024);
        assert!((back.temperature - 0.7).abs() < f32::EPSILON);
    }

    /// Configs saved before max_tokens/temperature existed deserialize with defaults.
    #[test]
    fn chat_config_legacy_without_new_fields() {
        let json = serde_json::json!({
            "system_prompt": "hello",
            "rag_enabled": true,
            "rag_max_results": 3,
            "rag_min_score": 0.2,
            "max_history_turns": 5,
            "rate_limit_per_hour": 10
        });
        let config: ChatConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.max_tokens, 1024);
        assert!((config.temperature - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn chat_turn_serde_roundtrip() {
        let turn = ChatTurn {
            role: ChatRole::User,
            content: "Hello".to_string(),
            timestamp: 1700000000,
        };
        let json = serde_json::to_value(&turn).unwrap();
        assert_eq!(json["role"], "user");
        let back: ChatTurn = serde_json::from_value(json).unwrap();
        assert_eq!(back.role, ChatRole::User);
        assert_eq!(back.content, "Hello");
    }

    #[test]
    fn chat_role_as_str() {
        assert_eq!(ChatRole::User.as_str(), "user");
        assert_eq!(ChatRole::Assistant.as_str(), "assistant");
    }

    #[test]
    fn strip_html_tags_basic() {
        assert_eq!(strip_html_tags("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(strip_html_tags("<mark>test</mark> content"), "test content");
        assert_eq!(strip_html_tags("no tags here"), "no tags here");
        assert_eq!(strip_html_tags(""), "");
        // Handles edge cases the naive char-by-char approach would miss:
        assert_eq!(strip_html_tags("a <!-- comment --> b"), "a  b");
    }

    #[test]
    fn truncate_str_respects_char_boundary() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello");
        // Multi-byte: "café" — 'é' is 2 bytes at offset 3..5
        let s = "café";
        let t = truncate_str(s, 4);
        assert!(t.len() <= 4);
        assert!(t == "caf" || t == "café");
    }

    #[tokio::test]
    async fn build_messages_assembles_correctly() {
        let db = PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let ai = Arc::new(AiProviderService::new(db.clone()));
        let search = Arc::new(SearchService::new(db.clone()));
        let service = ChatService::new(db, ai, search);

        let history = vec![
            ChatTurn {
                role: ChatRole::User,
                content: "Hi".to_string(),
                timestamp: 1,
            },
            ChatTurn {
                role: ChatRole::Assistant,
                content: "Hello!".to_string(),
                timestamp: 2,
            },
        ];

        let messages =
            service.build_messages("You are helpful.", "Context here", &history, "New question");

        assert_eq!(messages.len(), 4); // system + 2 history + user
        assert_eq!(messages[0].role, "system");
        assert!(messages[0].content.contains("You are helpful."));
        assert!(messages[0].content.contains("---BEGIN CONTEXT---"));
        assert!(messages[0].content.contains("Context here"));
        assert!(messages[0].content.contains("---END CONTEXT---"));
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content, "Hi");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[2].content, "Hello!");
        assert_eq!(messages[3].role, "user");
        assert_eq!(messages[3].content, "New question");
    }

    #[tokio::test]
    async fn build_messages_without_rag_context() {
        let db = PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let ai = Arc::new(AiProviderService::new(db.clone()));
        let search = Arc::new(SearchService::new(db.clone()));
        let service = ChatService::new(db, ai, search);

        let messages = service.build_messages("System prompt.", "", &[], "Hello");

        assert_eq!(messages.len(), 2); // system + user
        assert_eq!(messages[0].content, "System prompt.");
        assert_eq!(messages[1].content, "Hello");
    }

    // -------------------------------------------------------------------------
    // Stream parser unit tests (F12)
    // -------------------------------------------------------------------------

    /// Helper: build a fake reqwest::Response from raw SSE bytes.
    fn fake_response(body: &[u8]) -> reqwest::Response {
        // axum re-exports the http crate.
        axum::http::Response::builder()
            .status(200)
            .body(body.to_vec())
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn parse_openai_stream_tokens_and_done() {
        use tokio_stream::StreamExt;

        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
                     data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n\
                     data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n\
                     data: [DONE]\n\n";
        let response = fake_response(sse);

        let stream = parse_openai_stream(response);
        tokio::pin!(stream);

        let mut tokens = String::new();
        let mut done_event = None;
        while let Some(event) = stream.next().await {
            match event {
                ChatStreamEvent::Token(t) => tokens.push_str(&t),
                ChatStreamEvent::Done {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                } => {
                    done_event = Some((prompt_tokens, completion_tokens, total_tokens));
                }
                ChatStreamEvent::Error(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(tokens, "Hello world");
        let (pt, ct, tt) = done_event.expect("should have received Done event");
        assert_eq!(pt, 10);
        assert_eq!(ct, 5);
        assert_eq!(tt, 15);
    }

    #[tokio::test]
    async fn parse_anthropic_stream_tokens_and_done() {
        use tokio_stream::StreamExt;

        let sse = b"event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":8}}}\n\n\
                     event: content_block_delta\ndata: {\"delta\":{\"text\":\"Hi\"}}\n\n\
                     event: content_block_delta\ndata: {\"delta\":{\"text\":\" there\"}}\n\n\
                     event: message_delta\ndata: {\"usage\":{\"output_tokens\":4}}\n\n\
                     event: message_stop\ndata: {}\n\n";
        let response = fake_response(sse);

        let stream = parse_anthropic_stream(response);
        tokio::pin!(stream);

        let mut tokens = String::new();
        let mut done_event = None;
        while let Some(event) = stream.next().await {
            match event {
                ChatStreamEvent::Token(t) => tokens.push_str(&t),
                ChatStreamEvent::Done {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                } => {
                    done_event = Some((prompt_tokens, completion_tokens, total_tokens));
                }
                ChatStreamEvent::Error(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(tokens, "Hi there");
        let (pt, ct, tt) = done_event.expect("should have received Done event");
        assert_eq!(pt, 8);
        assert_eq!(ct, 4);
        assert_eq!(tt, 12);
    }

    #[tokio::test]
    async fn parse_anthropic_stream_handles_error_event() {
        use tokio_stream::StreamExt;

        let sse = b"event: error\ndata: {\"error\":{\"message\":\"rate limited\"}}\n\n";
        let response = fake_response(sse);

        let stream = parse_anthropic_stream(response);
        tokio::pin!(stream);

        let event = stream.next().await.expect("should have an event");
        match event {
            ChatStreamEvent::Error(msg) => assert!(msg.contains("rate limited")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parse_openai_stream_buffer_limit() {
        use tokio_stream::StreamExt;

        // Create a response that exceeds 1 MiB without a newline to trigger buffer limit.
        let big = vec![b'x'; MAX_STREAM_BUFFER + 1];
        let response = fake_response(&big);

        let stream = parse_openai_stream(response);
        tokio::pin!(stream);

        let event = stream.next().await.expect("should have an event");
        match event {
            ChatStreamEvent::Error(msg) => {
                assert!(msg.contains("buffer exceeded"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
