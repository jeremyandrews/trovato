//! Tool-calling chat completions, for the AI Assistant.
//!
//! The kernel already speaks to AI providers in two places, and neither can
//! carry a tool call. [`crate::services::ai_chat`] streams a text-only visitor
//! conversation; the plugin AI host (`host::ai`) serves a plugin's one-shot `ai_request`,
//! whose message roles are validated to `system|user|assistant`. Neither builds
//! a `tools` array and neither parses `tool_calls` or `tool_use` out of a
//! response, so before this module a model could not ask for anything to be
//! done.
//!
//! This is the third path, and it is deliberately separate rather than an
//! extension of either: the existing text-only request builders and parsers are
//! left byte for byte alone, so nothing about the visitor chatbot or a plugin's
//! `ai_request` changes shape because the assistant exists.
//!
//! What it adds over the text paths:
//!
//! - **Tools on the wire**, in each protocol's own shape: OpenAI's
//!   `tools: [{type:"function", function:{…}}]` with `tool_choice: "auto"`, and
//!   Anthropic's `tools: [{name, description, input_schema}]`.
//! - **A conversation that contains tool calls**, so a second turn can carry
//!   what the first one did. The two protocols disagree about how: OpenAI puts
//!   the call on the assistant message and answers it with a `role:"tool"`
//!   message; Anthropic puts a `tool_use` block in the assistant's content and
//!   requires **the very next user turn** to answer every one of them with
//!   `tool_result` blocks. The Anthropic body builder merges consecutive
//!   [`ChatMessage::ToolResult`]s into one user message for exactly that reason.
//! - **No streaming.** A turn is a loop of complete calls: the caller cannot act
//!   on a tool call until the whole message has arrived, so streaming would buy
//!   nothing and would have to be re-assembled anyway.
//!
//! The outbound call goes through the provider's per-operation circuit breaker,
//! like the other chat paths, and distinguishes auth, rate-limit and other
//! provider failures so the caller can say something honest without leaking the
//! provider's response body to the person.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::services::ai_provider::{
    AiOperationType, AiProviderService, ProviderProtocol, ResolvedProvider,
};

// =============================================================================
// Types
// =============================================================================

/// One tool offered to the model.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Tool name, as the model will call it.
    pub name: String,
    /// What the tool does, for the model to read.
    pub description: String,
    /// A JSON Schema object describing the arguments.
    pub parameters: Value,
}

/// One tool call the model made.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Provider-assigned call id, echoed back with the result.
    pub id: String,
    /// The tool the model called.
    pub name: String,
    /// The arguments, as a JSON object.
    ///
    /// OpenAI sends these as a **string** holding JSON. A string that does not
    /// parse becomes `{"_unparsed": "<raw>"}` rather than an error, so the
    /// caller can report the problem back to the model and let it try again —
    /// which is a far better outcome than failing the turn.
    pub arguments: Value,
}

/// One message in a tool-calling conversation.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    /// System instructions. Anthropic takes these in a separate `system` field,
    /// so several are joined rather than sent as messages.
    System(String),
    /// Something the person said.
    User(String),
    /// Something the model said, with any tool calls it made.
    Assistant {
        /// The text of the message, if it said anything.
        text: Option<String>,
        /// The tool calls it made, possibly none.
        tool_calls: Vec<ToolCall>,
    },
    /// The answer to one of the model's tool calls.
    ToolResult {
        /// The `id` of the [`ToolCall`] being answered.
        call_id: String,
        /// The tool's name (OpenAI ignores it; it keeps logs readable).
        name: String,
        /// What the tool produced, as text.
        content: String,
        /// Whether this is an error rather than a result.
        is_error: bool,
    },
}

/// One completed model response.
#[derive(Debug, Clone)]
pub struct ChatCompletion {
    /// The text the model produced, if any.
    pub text: Option<String>,
    /// The tool calls it made, in the order it made them.
    pub tool_calls: Vec<ToolCall>,
    /// The provider's stop reason, verbatim.
    pub finish_reason: Option<String>,
    /// `(prompt, completion, total)` tokens.
    pub usage: (u32, u32, u32),
    /// The model that answered.
    pub model: String,
    /// Round-trip latency in milliseconds.
    pub latency_ms: u64,
}

/// Why a completion failed, at the granularity the caller needs to say something
/// useful without repeating the provider's body to the person.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
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
    /// The transport failed, or the response was not the shape we parse.
    #[error("{0}")]
    Transport(String),
}

impl ChatError {
    /// A short, provider-agnostic sentence for the person. The detail belongs in
    /// the log, never here.
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::Unauthorized => "The model provider refused the request.",
            Self::RateLimited => "The model provider is busy. Try again shortly.",
            Self::Timeout => "The model took too long to answer.",
            Self::BreakerOpen => "The model provider is unavailable right now.",
            Self::Provider { .. } | Self::Transport(_) => {
                "Something went wrong talking to the model provider."
            }
        }
    }
}

/// Longest slice of a provider's error body kept for the log.
const ERROR_DETAIL_MAX: usize = 500;

// =============================================================================
// Entry point
// =============================================================================

/// Run one tool-calling chat completion.
///
/// `tools` may be empty, in which case neither protocol is sent a tools array at
/// all — a request with an empty `tools: []` is rejected by some providers and
/// means nothing to the rest.
///
/// The call is wrapped in the provider's per-operation circuit breaker, the same
/// one the other chat paths use, so a provider that is failing stops being
/// hammered.
pub async fn chat_complete(
    providers: &AiProviderService,
    resolved: &ResolvedProvider,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    temperature: f32,
    max_tokens: u32,
    timeout: Duration,
) -> Result<ChatCompletion, ChatError> {
    let op_key = AiOperationType::Chat.config_key();
    let window = providers
        .get_timeout_config()
        .await
        .unwrap_or_default()
        .resolve_breaker_window(op_key);
    let breaker = providers.breaker_for_operation(op_key, window);

    let result = breaker
        .call(|| async {
            send_once(
                providers,
                resolved,
                messages,
                tools,
                temperature,
                max_tokens,
                timeout,
            )
            .await
        })
        .await;

    match result {
        Ok(completion) => Ok(completion),
        Err(crate::circuit_breaker::CircuitBreakerError::Open) => Err(ChatError::BreakerOpen),
        Err(crate::circuit_breaker::CircuitBreakerError::ServiceError(e)) => Err(e),
    }
}

/// One HTTP round trip, without the breaker.
#[allow(clippy::too_many_arguments)]
async fn send_once(
    providers: &AiProviderService,
    resolved: &ResolvedProvider,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    temperature: f32,
    max_tokens: u32,
    timeout: Duration,
) -> Result<ChatCompletion, ChatError> {
    let base = resolved.config.base_url.trim_end_matches('/');
    let (url, body, headers) = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => (
            format!("{base}/chat/completions"),
            build_openai_body(&resolved.model, messages, tools, temperature, max_tokens),
            openai_headers(resolved),
        ),
        ProviderProtocol::Anthropic => (
            format!("{base}/messages"),
            build_anthropic_body(&resolved.model, messages, tools, temperature, max_tokens),
            anthropic_headers(resolved),
        ),
    };

    let body_str = serde_json::to_string(&body).unwrap_or_default();
    let started = Instant::now();

    let mut request = providers
        .http()
        .post(&url)
        .timeout(timeout)
        .header("content-type", "application/json")
        .body(body_str);
    for (name, value) in &headers {
        request = request.header(name.as_str(), value.as_str());
    }

    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            ChatError::Timeout
        } else {
            ChatError::Transport(e.to_string())
        }
    })?;

    let status = response.status();
    if !status.is_success() {
        let detail =
            truncate_on_char_boundary(&response.text().await.unwrap_or_default(), ERROR_DETAIL_MAX);
        return Err(match status.as_u16() {
            401 | 403 => ChatError::Unauthorized,
            429 => ChatError::RateLimited,
            other => ChatError::Provider {
                status: other,
                detail,
            },
        });
    }

    let value: Value = response
        .json()
        .await
        .map_err(|e| ChatError::Transport(format!("unreadable provider response: {e}")))?;
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let mut completion = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => parse_openai_response(&value),
        ProviderProtocol::Anthropic => parse_anthropic_response(&value),
    }
    .ok_or_else(|| ChatError::Transport("provider response had no completion in it".to_string()))?;

    completion.latency_ms = latency_ms;
    if completion.model.is_empty() {
        completion.model = resolved.model.clone();
    }
    Ok(completion)
}

// =============================================================================
// OpenAI-compatible
// =============================================================================

fn openai_headers(resolved: &ResolvedProvider) -> Vec<(String, String)> {
    match resolved.api_key {
        Some(ref key) => vec![("authorization".to_string(), format!("Bearer {key}"))],
        None => Vec::new(),
    }
}

/// Build an OpenAI-compatible `/chat/completions` body.
///
/// `tools` and `tool_choice` are omitted entirely when there are no tools.
pub(crate) fn build_openai_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    temperature: f32,
    max_tokens: u32,
) -> Value {
    let wire: Vec<Value> = messages
        .iter()
        .map(|message| match message {
            ChatMessage::System(text) => json!({"role": "system", "content": text}),
            ChatMessage::User(text) => json!({"role": "user", "content": text}),
            ChatMessage::Assistant { text, tool_calls } => {
                let mut object = json!({
                    "role": "assistant",
                    "content": text.clone().map(Value::String).unwrap_or(Value::Null),
                });
                if !tool_calls.is_empty() {
                    object["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": {
                                        "name": call.name,
                                        // OpenAI takes the arguments as a JSON
                                        // *string*, not an object.
                                        "arguments": serde_json::to_string(&call.arguments)
                                            .unwrap_or_else(|_| "{}".to_string()),
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                object
            }
            ChatMessage::ToolResult {
                call_id, content, ..
            } => json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content,
            }),
        })
        .collect();

    let mut body = json!({
        "model": model,
        "messages": wire,
        "temperature": temperature,
        "max_tokens": max_tokens,
    });

    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }

    body
}

/// Parse an OpenAI-compatible completion response.
pub(crate) fn parse_openai_response(value: &Value) -> Option<ChatCompletion> {
    let choice = value.get("choices")?.get(0)?;
    let message = choice.get("message")?;

    let text = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty());

    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let function = call.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{index}"));
            let raw = function
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let arguments =
                serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({"_unparsed": raw}));
            tool_calls.push(ToolCall {
                id,
                name,
                arguments,
            });
        }
    }

    let usage = value.get("usage");
    Some(ChatCompletion {
        text,
        tool_calls,
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage: (
            usage_field(usage, "prompt_tokens"),
            usage_field(usage, "completion_tokens"),
            usage_field(usage, "total_tokens"),
        ),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        latency_ms: 0,
    })
}

// =============================================================================
// Anthropic
// =============================================================================

fn anthropic_headers(resolved: &ResolvedProvider) -> Vec<(String, String)> {
    let mut headers = vec![("anthropic-version".to_string(), "2023-06-01".to_string())];
    if let Some(ref key) = resolved.api_key {
        headers.push(("x-api-key".to_string(), key.clone()));
    }
    headers
}

/// Build an Anthropic `/messages` body.
///
/// Two shape rules the protocol enforces and this respects:
///
/// - Every `tool_use` block in an assistant turn must be answered in **the very
///   next user turn**, so consecutive [`ChatMessage::ToolResult`]s are merged
///   into a single user message carrying one `tool_result` block each.
/// - An assistant message must never have an empty `content` array, so the text
///   block is omitted only when there is also at least one tool call.
pub(crate) fn build_anthropic_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    temperature: f32,
    max_tokens: u32,
) -> Value {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut wire: Vec<Value> = Vec::new();
    // Blocks for the user message currently being accumulated from a run of
    // consecutive tool results.
    let mut pending_results: Vec<Value> = Vec::new();

    for message in messages {
        // Anything that is not a tool result closes an open run of them.
        if !matches!(message, ChatMessage::ToolResult { .. }) && !pending_results.is_empty() {
            wire.push(json!({
                "role": "user",
                "content": Value::Array(std::mem::take(&mut pending_results)),
            }));
        }

        match message {
            ChatMessage::System(text) => system_parts.push(text),
            ChatMessage::User(text) => wire.push(json!({"role": "user", "content": text})),
            ChatMessage::Assistant { text, tool_calls } => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = text.as_ref().filter(|t| !t.is_empty()) {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for call in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                // Never an empty content array: an assistant turn that said
                // nothing and called nothing is not a turn the API accepts.
                if blocks.is_empty() {
                    blocks.push(json!({"type": "text", "text": ""}));
                }
                wire.push(json!({"role": "assistant", "content": Value::Array(blocks)}));
            }
            ChatMessage::ToolResult {
                call_id,
                content,
                is_error,
                ..
            } => {
                pending_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content,
                    "is_error": is_error,
                }));
            }
        }
    }
    if !pending_results.is_empty() {
        wire.push(json!({"role": "user", "content": Value::Array(pending_results)}));
    }

    let wire = merge_same_role(wire);

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "messages": wire,
    });

    if !system_parts.is_empty() {
        body["system"] = json!(system_parts.join("\n\n"));
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect(),
        );
    }

    body
}

/// Merge adjacent Anthropic messages that share a role.
///
/// The API takes alternating turns, and the assistant's conversation can produce
/// two in a row honestly: a `[Trovato]` note followed by the person's message is
/// two user turns, and a tool result followed by a note is another. Rather than
/// teach every caller the protocol's alternation rule, normalize here — this is
/// the one place that knows it is Anthropic.
fn merge_same_role(messages: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        let same_role = merged
            .last()
            .is_some_and(|last| last.get("role") == message.get("role"));
        if !same_role {
            merged.push(message);
            continue;
        }
        // Infallible: `same_role` is only true when there is a last element.
        let Some(last) = merged.last_mut() else {
            continue;
        };
        let previous = last["content"].take();
        let next = message["content"].clone();
        last["content"] = match (previous, next) {
            (Value::String(a), Value::String(b)) => Value::String(format!("{a}\n\n{b}")),
            (Value::Array(mut a), Value::Array(b)) => {
                a.extend(b);
                Value::Array(a)
            }
            (Value::String(a), Value::Array(mut b)) => {
                b.insert(0, json!({"type": "text", "text": a}));
                Value::Array(b)
            }
            (Value::Array(mut a), Value::String(b)) => {
                a.push(json!({"type": "text", "text": b}));
                Value::Array(a)
            }
            (a, _) => a,
        };
    }
    merged
}

/// Parse an Anthropic `/messages` response.
pub(crate) fn parse_anthropic_response(value: &Value) -> Option<ChatCompletion> {
    let blocks = value.get("content")?.as_array()?;

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    text_parts.push(text.to_string());
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                tool_calls.push(ToolCall {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{index}")),
                    name,
                    // Anthropic sends the arguments as an object already.
                    arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                });
            }
            _ => {}
        }
    }

    let usage = value.get("usage");
    let prompt = usage_field(usage, "input_tokens");
    let completion = usage_field(usage, "output_tokens");
    Some(ChatCompletion {
        text: (!text_parts.is_empty()).then(|| text_parts.join("")),
        tool_calls,
        finish_reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage: (prompt, completion, prompt.saturating_add(completion)),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        latency_ms: 0,
    })
}

// =============================================================================
// Helpers
// =============================================================================

fn usage_field(usage: Option<&Value>, key: &str) -> u32 {
    usage
        .and_then(|u| u.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

/// Truncate to at most `max` bytes without splitting a character.
pub(crate) fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

// =============================================================================
// Tests
// =============================================================================

/// A scripted local AI provider: an axum server on an ephemeral loopback port
/// that records every request body it received and answers from a queue of
/// canned responses.
///
/// Compiled into the library (behind `test-support`-shaped `#[doc(hidden)]`
/// visibility rather than `#[cfg(test)]`) because the assistant's **integration**
/// tests live in `crates/kernel/tests/` and need exactly this server: a
/// tool-calling turn cannot be driven end to end without a provider that can be
/// told what to call next.
#[doc(hidden)]
pub mod scripted_provider {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::Value;

    /// One canned answer.
    #[derive(Debug, Clone)]
    pub struct Scripted {
        /// HTTP status to answer with.
        pub status: u16,
        /// The JSON body.
        pub body: Value,
        /// Delay before answering, for timeout tests.
        pub delay: std::time::Duration,
    }

    impl Scripted {
        /// A 200 answer with this body.
        pub fn ok(body: Value) -> Self {
            Self {
                status: 200,
                body,
                delay: std::time::Duration::ZERO,
            }
        }

        /// A failing status with a token error body.
        pub fn status(status: u16) -> Self {
            Self {
                status,
                body: serde_json::json!({"error": {"message": "scripted failure"}}),
                delay: std::time::Duration::ZERO,
            }
        }

        /// Answer only after `delay`.
        pub fn after(mut self, delay: std::time::Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    /// The server's shared state: what to answer next, and what came in.
    #[derive(Clone, Default)]
    pub struct Recorder {
        queue: Arc<Mutex<std::collections::VecDeque<Scripted>>>,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl Recorder {
        /// Queue one more answer.
        pub fn push(&self, response: Scripted) {
            self.lock_queue().push_back(response);
        }

        /// Every request body received, in order.
        pub fn requests(&self) -> Vec<Value> {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        /// How many requests have arrived.
        pub fn request_count(&self) -> usize {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).len()
        }

        fn lock_queue(&self) -> std::sync::MutexGuard<'_, std::collections::VecDeque<Scripted>> {
            self.queue.lock().unwrap_or_else(|e| e.into_inner())
        }
    }

    async fn handle(State(recorder): State<Recorder>, body: String) -> axum::response::Response {
        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        recorder
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(parsed);

        let next = recorder.lock_queue().pop_front();
        match next {
            Some(scripted) => {
                if !scripted.delay.is_zero() {
                    tokio::time::sleep(scripted.delay).await;
                }
                let status = axum::http::StatusCode::from_u16(scripted.status)
                    .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                (status, Json(scripted.body)).into_response()
            }
            None => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "scripted provider ran out of responses"})),
            )
                .into_response(),
        }
    }

    /// Start a scripted provider. Returns its base URL and the recorder.
    ///
    /// It serves both protocols' endpoints (`/chat/completions` and `/messages`)
    /// so one server can stand in for either.
    pub async fn start() -> (String, Recorder) {
        let recorder = Recorder::default();
        let app = Router::new()
            .route("/chat/completions", post(handle))
            .route("/messages", post(handle))
            .with_state(recorder.clone());

        // Port 0: the OS picks a free port, so parallel tests never collide.
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) => panic!("scripted provider could not bind a loopback port: {e}"),
        };
        let addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(e) => panic!("scripted provider has no local address: {e}"),
        };
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), recorder)
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::scripted_provider::{Scripted, start};
    use super::*;
    use crate::services::ai_provider::AiProviderConfig;

    fn resolved(base_url: String, protocol: ProviderProtocol) -> ResolvedProvider {
        ResolvedProvider {
            config: AiProviderConfig {
                id: "scripted".to_string(),
                label: "Scripted".to_string(),
                protocol,
                base_url,
                api_key_env: String::new(),
                models: Vec::new(),
                rate_limit_rpm: 0,
                enabled: true,
            },
            api_key: None,
            model: "test-model".to_string(),
        }
    }

    fn tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "read_widget".to_string(),
                description: "Read the widget".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
            ToolSpec {
                name: "set_widget_color".to_string(),
                description: "Set the colour".to_string(),
                parameters: json!({
                    "type": "object",
                    "required": ["color"],
                    "properties": {"color": {"type": "string"}}
                }),
            },
        ]
    }

    /// A provider service with no database behind it. Every path exercised here
    /// (`http`, `breaker_for_operation`, `get_timeout_config`) tolerates the
    /// pool being unusable — the `ai_timeouts` read falls back to defaults.
    ///
    /// The short `acquire_timeout` is load-bearing rather than tidy: sqlx keeps
    /// retrying a refused connection until the acquire timeout expires, which
    /// defaults to 30 seconds, and `chat_complete` reads `ai_timeouts` on every
    /// call. Left at the default, three of these tests sat for a minute apiece
    /// waiting for a database nobody wants here.
    fn provider_service() -> AiProviderService {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://trovato:unused@127.0.0.1:1/unused")
            .expect("a lazy pool never connects here");
        AiProviderService::new(pool)
    }

    async fn complete(
        base: &str,
        protocol: ProviderProtocol,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ChatCompletion, ChatError> {
        let service = provider_service();
        chat_complete(
            &service,
            &resolved(base.to_string(), protocol),
            messages,
            tools,
            0.2,
            256,
            Duration::from_secs(5),
        )
        .await
    }

    fn openai_text(text: &str) -> serde_json::Value {
        json!({
            "model": "test-model",
            "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14}
        })
    }

    fn openai_calls(calls: serde_json::Value) -> serde_json::Value {
        json!({
            "model": "test-model",
            "choices": [{
                "message": {"role": "assistant", "content": null, "tool_calls": calls},
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25}
        })
    }

    // -------------------------------------------------------------------------
    // OpenAI-compatible
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn openai_without_tools_sends_no_tools_array() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(openai_text("hello")));

        let completion = complete(
            &base,
            ProviderProtocol::OpenAiCompatible,
            &[ChatMessage::User("hi".into())],
            &[],
        )
        .await
        .expect("a text-only completion");

        assert_eq!(completion.text.as_deref(), Some("hello"));
        assert!(completion.tool_calls.is_empty());
        assert_eq!(completion.usage, (11, 3, 14));
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));

        let sent = &recorder.requests()[0];
        // An empty `tools: []` is rejected by some providers and means nothing
        // to the rest, so it must be absent entirely.
        assert!(sent.get("tools").is_none(), "{sent}");
        assert!(sent.get("tool_choice").is_none(), "{sent}");
    }

    #[tokio::test]
    async fn openai_with_tools_sends_the_function_shape() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(openai_text("ok")));

        complete(
            &base,
            ProviderProtocol::OpenAiCompatible,
            &[
                ChatMessage::System("be brief".into()),
                ChatMessage::User("hi".into()),
            ],
            &tools(),
        )
        .await
        .expect("a completion");

        let sent = &recorder.requests()[0];
        assert_eq!(sent["tool_choice"], "auto");
        assert_eq!(sent["tools"][0]["type"], "function");
        assert_eq!(sent["tools"][0]["function"]["name"], "read_widget");
        assert_eq!(
            sent["tools"][1]["function"]["parameters"]["required"][0],
            "color"
        );
        // A system message is a message here, unlike Anthropic.
        assert_eq!(sent["messages"][0]["role"], "system");
    }

    #[tokio::test]
    async fn openai_parses_one_tool_call() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(openai_calls(json!([{
            "id": "call_a",
            "type": "function",
            "function": {"name": "read_widget", "arguments": "{}"}
        }]))));

        let completion = complete(
            &base,
            ProviderProtocol::OpenAiCompatible,
            &[ChatMessage::User("what colour?".into())],
            &tools(),
        )
        .await
        .expect("a tool-calling completion");

        assert!(completion.text.is_none());
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].id, "call_a");
        assert_eq!(completion.tool_calls[0].name, "read_widget");
        assert_eq!(completion.tool_calls[0].arguments, json!({}));
    }

    #[tokio::test]
    async fn openai_parses_two_tool_calls_in_order() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(openai_calls(json!([
            {"id": "c1", "type": "function",
             "function": {"name": "read_widget", "arguments": "{}"}},
            {"id": "c2", "type": "function",
             "function": {"name": "set_widget_color", "arguments": "{\"color\":\"teal\"}"}}
        ]))));

        let completion = complete(
            &base,
            ProviderProtocol::OpenAiCompatible,
            &[ChatMessage::User("make it teal".into())],
            &tools(),
        )
        .await
        .expect("a two-call completion");

        assert_eq!(completion.tool_calls.len(), 2);
        assert_eq!(completion.tool_calls[0].name, "read_widget");
        assert_eq!(completion.tool_calls[1].name, "set_widget_color");
        assert_eq!(completion.tool_calls[1].arguments["color"], "teal");
    }

    #[tokio::test]
    async fn openai_arguments_that_are_not_json_survive_as_unparsed() {
        // The turn must not die because a model emitted malformed arguments:
        // the caller reports the problem back to the model instead.
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(openai_calls(json!([{
            "id": "c1",
            "type": "function",
            "function": {"name": "read_widget", "arguments": "{not json"}
        }]))));

        let completion = complete(
            &base,
            ProviderProtocol::OpenAiCompatible,
            &[ChatMessage::User("go".into())],
            &tools(),
        )
        .await
        .expect("a completion even with bad arguments");

        assert_eq!(completion.tool_calls[0].arguments["_unparsed"], "{not json");
    }

    #[tokio::test]
    async fn openai_round_trip_encodes_a_prior_call_and_its_result() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(openai_text("It is teal.")));

        complete(
            &base,
            ProviderProtocol::OpenAiCompatible,
            &[
                ChatMessage::User("what colour?".into()),
                ChatMessage::Assistant {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "read_widget".into(),
                        arguments: json!({"deep": {"n": 1}}),
                    }],
                },
                ChatMessage::ToolResult {
                    call_id: "c1".into(),
                    name: "read_widget".into(),
                    content: r#"{"color":"teal"}"#.into(),
                    is_error: false,
                },
            ],
            &tools(),
        )
        .await
        .expect("a follow-up completion");

        let sent = &recorder.requests()[0];
        let messages = sent["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], Value::Null);
        assert_eq!(messages[1]["tool_calls"][0]["id"], "c1");
        // The arguments go on the wire as a JSON *string*, not an object.
        let encoded = messages[1]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("OpenAI arguments must be a string");
        assert_eq!(
            serde_json::from_str::<Value>(encoded).unwrap(),
            json!({"deep": {"n": 1}})
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "c1");
        assert_eq!(messages[2]["content"], r#"{"color":"teal"}"#);
    }

    // -------------------------------------------------------------------------
    // Anthropic
    // -------------------------------------------------------------------------

    fn anthropic_body(content: serde_json::Value, stop: &str) -> serde_json::Value {
        json!({
            "model": "test-model",
            "content": content,
            "stop_reason": stop,
            "usage": {"input_tokens": 30, "output_tokens": 7}
        })
    }

    #[tokio::test]
    async fn anthropic_without_tools_sends_no_tools_and_hoists_system() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(anthropic_body(
            json!([{"type": "text", "text": "hello"}]),
            "end_turn",
        )));

        let completion = complete(
            &base,
            ProviderProtocol::Anthropic,
            &[
                ChatMessage::System("first".into()),
                ChatMessage::System("second".into()),
                ChatMessage::User("hi".into()),
            ],
            &[],
        )
        .await
        .expect("a text completion");

        assert_eq!(completion.text.as_deref(), Some("hello"));
        // Anthropic reports two counts; the total is derived.
        assert_eq!(completion.usage, (30, 7, 37));

        let sent = &recorder.requests()[0];
        assert!(sent.get("tools").is_none(), "{sent}");
        assert_eq!(sent["system"], "first\n\nsecond");
        assert_eq!(sent["messages"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn anthropic_with_tools_sends_input_schema() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(anthropic_body(
            json!([{"type": "text", "text": "ok"}]),
            "end_turn",
        )));

        complete(
            &base,
            ProviderProtocol::Anthropic,
            &[ChatMessage::User("hi".into())],
            &tools(),
        )
        .await
        .expect("a completion");

        let sent = &recorder.requests()[0];
        assert_eq!(sent["tools"][0]["name"], "read_widget");
        assert_eq!(sent["tools"][1]["input_schema"]["required"][0], "color");
        // No `type: function` wrapper here, and no tool_choice.
        assert!(sent["tools"][0].get("function").is_none(), "{sent}");
    }

    #[tokio::test]
    async fn anthropic_parses_one_and_two_tool_uses() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(anthropic_body(
            json!([{"type": "tool_use", "id": "t1", "name": "read_widget", "input": {}}]),
            "tool_use",
        )));
        recorder.push(Scripted::ok(anthropic_body(
            json!([
                {"type": "tool_use", "id": "t1", "name": "read_widget", "input": {}},
                {"type": "tool_use", "id": "t2", "name": "set_widget_color",
                 "input": {"color": "teal"}}
            ]),
            "tool_use",
        )));

        let one = complete(
            &base,
            ProviderProtocol::Anthropic,
            &[ChatMessage::User("go".into())],
            &tools(),
        )
        .await
        .expect("one call");
        assert_eq!(one.tool_calls.len(), 1);
        assert_eq!(one.tool_calls[0].id, "t1");
        assert_eq!(one.finish_reason.as_deref(), Some("tool_use"));

        let two = complete(
            &base,
            ProviderProtocol::Anthropic,
            &[ChatMessage::User("go".into())],
            &tools(),
        )
        .await
        .expect("two calls");
        assert_eq!(two.tool_calls.len(), 2);
        assert_eq!(two.tool_calls[1].arguments["color"], "teal");
    }

    #[tokio::test]
    async fn anthropic_mixes_text_and_tool_use_in_one_response() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(anthropic_body(
            json!([
                {"type": "text", "text": "Let me look. "},
                {"type": "tool_use", "id": "t1", "name": "read_widget", "input": {}},
            ]),
            "tool_use",
        )));

        let completion = complete(
            &base,
            ProviderProtocol::Anthropic,
            &[ChatMessage::User("go".into())],
            &tools(),
        )
        .await
        .expect("a mixed completion");

        assert_eq!(completion.text.as_deref(), Some("Let me look. "));
        assert_eq!(completion.tool_calls.len(), 1);
    }

    #[tokio::test]
    async fn anthropic_round_trip_merges_consecutive_tool_results_into_one_user_turn() {
        // The API requires every tool_use in an assistant turn to be answered in
        // the very next user turn. Two results must therefore arrive as two
        // blocks in ONE message, not two messages.
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(anthropic_body(
            json!([{"type": "text", "text": "done"}]),
            "end_turn",
        )));

        complete(
            &base,
            ProviderProtocol::Anthropic,
            &[
                ChatMessage::User("go".into()),
                ChatMessage::Assistant {
                    text: Some("Looking.".into()),
                    tool_calls: vec![
                        ToolCall {
                            id: "t1".into(),
                            name: "read_widget".into(),
                            arguments: json!({}),
                        },
                        ToolCall {
                            id: "t2".into(),
                            name: "read_widget".into(),
                            arguments: json!({}),
                        },
                    ],
                },
                ChatMessage::ToolResult {
                    call_id: "t1".into(),
                    name: "read_widget".into(),
                    content: "teal".into(),
                    is_error: false,
                },
                ChatMessage::ToolResult {
                    call_id: "t2".into(),
                    name: "read_widget".into(),
                    content: "nope".into(),
                    is_error: true,
                },
            ],
            &tools(),
        )
        .await
        .expect("a follow-up completion");

        let sent = &recorder.requests()[0];
        let messages = sent["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            3,
            "two results must be one user turn: {sent}"
        );
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        // Anthropic takes the input as an object, not a string.
        assert!(messages[1]["content"][1]["input"].is_object(), "{sent}");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(messages[2]["content"][1]["tool_use_id"], "t2");
        assert_eq!(messages[2]["content"][1]["is_error"], true);
    }

    #[test]
    fn anthropic_merges_adjacent_same_role_messages() {
        // A [Trovato] note followed by the person's message is two user turns,
        // which the API will not take. One turn is what it becomes.
        let body = build_anthropic_body(
            "m",
            &[
                ChatMessage::User("[Trovato] Applied: x".into()),
                ChatMessage::User("thanks, now do y".into()),
            ],
            &[],
            0.2,
            64,
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "{body}");
        assert_eq!(
            messages[0]["content"],
            "[Trovato] Applied: x\n\nthanks, now do y"
        );

        // A tool result (a user turn, as blocks) followed by a note merges into
        // the same turn, keeping the blocks and appending the text.
        let body = build_anthropic_body(
            "m",
            &[
                ChatMessage::ToolResult {
                    call_id: "t1".into(),
                    name: "read".into(),
                    content: "teal".into(),
                    is_error: false,
                },
                ChatMessage::User("[Trovato] Applied: x".into()),
            ],
            &[],
            0.2,
            64,
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "{body}");
        assert_eq!(messages[0]["content"][0]["type"], "tool_result");
        assert_eq!(messages[0]["content"][1]["type"], "text");
    }

    #[test]
    fn anthropic_never_sends_an_empty_assistant_content_array() {
        let body = build_anthropic_body(
            "m",
            &[ChatMessage::Assistant {
                text: None,
                tool_calls: Vec::new(),
            }],
            &[],
            0.2,
            64,
        );
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "{body}");
        assert_eq!(content[0]["type"], "text");
    }

    // -------------------------------------------------------------------------
    // Failure modes, both protocols
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn provider_failures_map_to_distinct_errors() {
        for protocol in [
            ProviderProtocol::OpenAiCompatible,
            ProviderProtocol::Anthropic,
        ] {
            for (status, expect) in [(401u16, "unauthorized"), (429, "rate"), (500, "provider")] {
                // A fresh server per case: the breaker counts failures per
                // operation, and three consecutive failures would open it.
                let (base, recorder) = start().await;
                recorder.push(Scripted::status(status));
                let error = complete(&base, protocol, &[ChatMessage::User("hi".into())], &[])
                    .await
                    .expect_err("a failing status must be an error");
                match (expect, &error) {
                    ("unauthorized", ChatError::Unauthorized) => {}
                    ("rate", ChatError::RateLimited) => {}
                    ("provider", ChatError::Provider { status: s, detail }) => {
                        assert_eq!(*s, 500);
                        assert!(detail.contains("scripted failure"), "{detail}");
                    }
                    _ => panic!("{status} on {protocol:?} produced {error:?}"),
                }
                // Whatever went wrong, the person is told something generic.
                assert!(!error.user_message().contains("scripted"));
            }
        }
    }

    #[tokio::test]
    async fn a_slow_provider_becomes_a_timeout() {
        let (base, recorder) = start().await;
        recorder.push(Scripted::ok(openai_text("too late")).after(Duration::from_secs(5)));

        let service = provider_service();
        let error = chat_complete(
            &service,
            &resolved(base, ProviderProtocol::OpenAiCompatible),
            &[ChatMessage::User("hi".into())],
            &[],
            0.2,
            64,
            Duration::from_millis(150),
        )
        .await
        .expect_err("a slow provider must time out");

        assert!(matches!(error, ChatError::Timeout), "{error:?}");
    }

    #[tokio::test]
    async fn repeated_failures_open_the_breaker() {
        let (base, recorder) = start().await;
        for _ in 0..4 {
            recorder.push(Scripted::status(500));
        }
        let service = provider_service();
        let provider = resolved(base, ProviderProtocol::OpenAiCompatible);

        let mut last = None;
        for _ in 0..4 {
            last = Some(
                chat_complete(
                    &service,
                    &provider,
                    &[ChatMessage::User("hi".into())],
                    &[],
                    0.2,
                    64,
                    Duration::from_secs(5),
                )
                .await
                .expect_err("scripted failure"),
            );
        }
        assert!(
            matches!(last, Some(ChatError::BreakerOpen)),
            "the fourth call should be short-circuited, got {last:?}"
        );
        // The breaker stopped the call before it left the process.
        assert_eq!(recorder.request_count(), 3);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let s = "aé\u{1F600}b";
        for max in 0..s.len() + 2 {
            let out = truncate_on_char_boundary(s, max);
            assert!(out.len() <= max.max(s.len()));
            assert!(s.starts_with(&out));
        }
    }

    #[test]
    fn a_response_with_no_choices_is_a_transport_error_not_a_panic() {
        assert!(parse_openai_response(&json!({"model": "m"})).is_none());
        assert!(parse_anthropic_response(&json!({"model": "m"})).is_none());
    }
}
