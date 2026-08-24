//! The AI Assistant: configuration by conversation.
//!
//! One person, one thing being configured, one conversation. The plugin that
//! owns the thing declares a scope ([`crate::assistant`]), describes it once
//! when the conversation opens, and answers the model's tool calls. This module
//! is the part in between: the configuration, and the turn loop that runs when
//! somebody sends a message.
//!
//! # The shape of a turn
//!
//! A turn is a loop of complete (non-streamed) model calls, because the kernel
//! has to act on a tool call before the next call can be made:
//!
//! 1. Build the message list: a system message (core prompt, scope prompt, the
//!    context block), then the transcript, bounded to the last few exchanges.
//! 2. Ask the model, offering the scope's tools.
//! 3. Run what it asked for. A **read** tool is dispatched immediately. A
//!    **write** tool is not run at all: it is dispatched in `Describe` mode,
//!    which changes nothing, and becomes a proposal the person applies.
//! 4. If it called anything, go back to 1 with the results in the transcript.
//!    If it did not, the turn is over.
//!
//! # Why a write is never executed here
//!
//! This is the whole safety posture, and it is structural rather than a matter
//! of prompting. A model cannot change anything on this path, however
//! confidently it says it has, because the only `Execute` dispatch of a write
//! tool is in the apply route, reached by a person clicking Apply on a proposal
//! they read. The prompt tells the model this so it stops and says what it
//! proposed instead of claiming the change is done; the kernel does not rely on
//! it having listened.
//!
//! # What bounds a turn
//!
//! Four separate limits, because a conversation with a model is an unbounded
//! bill by default: the per-call provider timeout, an overall wall clock of
//! three times that for the whole message, a cap on tool calls per message, and
//! per-conversation caps on messages and on tokens. The last two make a
//! conversation read-only rather than closing it, so the transcript stays
//! readable and Start over is the way out.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::{debug, warn};
use trovato_sdk::types::{
    AssistantContext, AssistantContextRequest, AssistantToolCall, AssistantToolKind,
    AssistantToolMode, AssistantToolResult,
};
use uuid::Uuid;

use crate::assistant::RegisteredScope;
use crate::models::SiteConfig;
use crate::models::assistant::{
    Conversation, PROPOSAL_PROPOSED, Proposal, TranscriptEntry, now as epoch_now,
};
use crate::services::ai_provider::{AiOperationType, ResolvedProvider};
use crate::services::ai_token_budget::UsageLogEntry;
use crate::services::ai_tools::{ChatCompletion, ChatMessage, ToolCall, ToolSpec, chat_complete};
use crate::state::AppState;
use crate::tap::{RequestState, UserContext};

// =============================================================================
// Configuration
// =============================================================================

/// Site config key holding [`AssistantConfig`].
pub const CONFIG_KEY: &str = "ai_assistant_config";

/// The plugin name usage is logged under, so assistant spend is separable from
/// the visitor chatbot's (`kernel_chat`) and from any plugin's.
pub const USAGE_PLUGIN: &str = "kernel_assistant";

/// The stock core prompt.
///
/// It is one paragraph of role and six rules, and every rule is there because
/// its absence produces a specific failure: inventing identifiers, claiming a
/// change was made, proposing the same change twice, proposing things nobody
/// asked for, guessing instead of asking, and writing an essay.
pub const DEFAULT_CORE_PROMPT: &str = "\
You are Trovato's configuration assistant. You are helping one person configure one specific thing, described in the CONTEXT block. Your job is to understand what they want, look at the evidence, and propose precise changes.

Rules:
- Read before you write. Use the read tools to check facts you are not sure of; prefer one narrow read over a broad one.
- Refer to things only by the identifiers and names that appear in the context or in tool results. Never invent an identifier.
- To change anything, call a write tool. A write tool does not change anything by itself: it creates a proposal the person has to apply. After proposing, say what you proposed and why, then stop and wait. Never say a change was made unless a [Trovato] message says it was applied.
- Do not propose the same change twice. Do not propose changes the person did not ask for; suggest them in words instead.
- When an instruction is ambiguous or the evidence is thin, say so and ask one clear question.
- Be brief. Plain sentences, no headings, no lists longer than five items.";

fn default_temperature() -> f32 {
    0.2
}
fn default_turn_timeout_secs() -> u64 {
    60
}
fn default_max_tool_calls() -> u32 {
    8
}
fn default_max_messages() -> u32 {
    40
}
fn default_max_tokens_per_conversation() -> u64 {
    60_000
}
fn default_max_history_exchanges() -> u32 {
    12
}
fn default_max_response_tokens() -> u32 {
    1024
}
fn default_snapshot_max_bytes() -> usize {
    12_288
}
fn default_tool_result_max_bytes() -> usize {
    16_384
}
fn default_rate_limit_per_hour() -> u32 {
    60
}
fn default_conversation_ttl_hours() -> u32 {
    24
}
fn default_core_prompt() -> String {
    DEFAULT_CORE_PROMPT.to_string()
}

/// Per-scope overrides, set from the admin page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantScopeConfig {
    /// Whether this scope can be opened at all. A disabled scope 404s.
    #[serde(default = "crate::services::ai_assistant::default_true")]
    pub enabled: bool,
    /// Replaces the plugin's stock domain prompt when set.
    #[serde(default)]
    pub prompt_override: Option<String>,
}

/// Default for a scope's `enabled`: a registered scope works out of the box.
pub fn default_true() -> bool {
    true
}

impl Default for AssistantScopeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt_override: None,
        }
    }
}

/// Everything a site can say about the assistant.
///
/// Every field has a serde default, so an absent `ai_assistant_config` key is
/// the default configuration rather than an error — and a config written by an
/// older kernel still loads when a field is added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantConfig {
    /// Off by default. A site turns the assistant on deliberately, because it
    /// spends money and lets a model read domain data.
    #[serde(default)]
    pub enabled: bool,
    /// Provider to use, or the site's Chat default when unset.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Model to use, or the provider's Chat model when unset.
    #[serde(default)]
    pub model: Option<String>,
    /// Sampling temperature. Low on purpose: this is configuration work.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Per-call provider timeout. The whole message gets three times this.
    #[serde(default = "default_turn_timeout_secs")]
    pub turn_timeout_secs: u64,
    /// How many tools the model may call in answer to one message.
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls_per_message: u32,
    /// How many messages a conversation may carry before it goes read-only.
    #[serde(default = "default_max_messages")]
    pub max_messages: u32,
    /// How many tokens a conversation may cost before it goes read-only.
    #[serde(default = "default_max_tokens_per_conversation")]
    pub max_tokens_per_conversation: u64,
    /// How many exchanges of history each request carries.
    #[serde(default = "default_max_history_exchanges")]
    pub max_history_exchanges: u32,
    /// Cap on the model's own response length.
    #[serde(default = "default_max_response_tokens")]
    pub max_response_tokens: u32,
    /// Cap on a plugin's snapshot, in bytes.
    #[serde(default = "default_snapshot_max_bytes")]
    pub snapshot_max_bytes: usize,
    /// Cap on one tool result, in bytes.
    #[serde(default = "default_tool_result_max_bytes")]
    pub tool_result_max_bytes: usize,
    /// Messages per person per hour. 0 is unlimited.
    #[serde(default = "default_rate_limit_per_hour")]
    pub rate_limit_per_hour: u32,
    /// How long a conversation stays writable after it was created.
    #[serde(default = "default_conversation_ttl_hours")]
    pub conversation_ttl_hours: u32,
    /// The prompt every scope's prompt is appended to.
    #[serde(default = "default_core_prompt")]
    pub core_prompt: String,
    /// Per-scope overrides, keyed by scope name.
    #[serde(default)]
    pub scopes: HashMap<String, AssistantScopeConfig>,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_id: None,
            model: None,
            temperature: default_temperature(),
            turn_timeout_secs: default_turn_timeout_secs(),
            max_tool_calls_per_message: default_max_tool_calls(),
            max_messages: default_max_messages(),
            max_tokens_per_conversation: default_max_tokens_per_conversation(),
            max_history_exchanges: default_max_history_exchanges(),
            max_response_tokens: default_max_response_tokens(),
            snapshot_max_bytes: default_snapshot_max_bytes(),
            tool_result_max_bytes: default_tool_result_max_bytes(),
            rate_limit_per_hour: default_rate_limit_per_hour(),
            conversation_ttl_hours: default_conversation_ttl_hours(),
            core_prompt: default_core_prompt(),
            scopes: HashMap::new(),
        }
    }
}

impl AssistantConfig {
    /// Bring every numeric field into range.
    ///
    /// Applied on save **and** on load: a config row written by hand, or by an
    /// older kernel with different bounds, must not be able to put a value the
    /// loop cannot survive in front of it.
    pub fn clamp(&mut self) {
        if !self.temperature.is_finite() {
            self.temperature = default_temperature();
        }
        self.temperature = self.temperature.clamp(0.0, 2.0);
        self.turn_timeout_secs = self.turn_timeout_secs.clamp(5, 150);
        self.max_tool_calls_per_message = self.max_tool_calls_per_message.clamp(1, 32);
        self.max_messages = self.max_messages.clamp(4, 400);
        self.max_tokens_per_conversation = self.max_tokens_per_conversation.clamp(1_000, 2_000_000);
        self.max_history_exchanges = self.max_history_exchanges.clamp(1, 100);
        self.max_response_tokens = self.max_response_tokens.clamp(64, 16_384);
        self.snapshot_max_bytes = self.snapshot_max_bytes.clamp(1_024, 32_768);
        self.tool_result_max_bytes = self.tool_result_max_bytes.clamp(1_024, 32_768);
        self.rate_limit_per_hour = self.rate_limit_per_hour.min(1_000);
        if self.core_prompt.trim().is_empty() {
            self.core_prompt = default_core_prompt();
        }
    }

    /// Whether this scope is enabled. An unmentioned scope is enabled.
    pub fn scope_enabled(&self, scope: &str) -> bool {
        self.scopes.get(scope).is_none_or(|s| s.enabled)
    }

    /// The domain prompt for this scope: the site's override, else the stock one.
    pub fn scope_prompt<'a>(&'a self, registered: &'a RegisteredScope) -> &'a str {
        self.scopes
            .get(&registered.scope.name)
            .and_then(|s| s.prompt_override.as_deref())
            .filter(|p| !p.trim().is_empty())
            .unwrap_or(&registered.scope.prompt)
    }

    /// The per-call provider timeout.
    pub fn call_timeout(&self) -> Duration {
        Duration::from_secs(self.turn_timeout_secs)
    }

    /// The wall clock for one whole message: three model calls' worth.
    pub fn turn_deadline(&self) -> Duration {
        Duration::from_secs(self.turn_timeout_secs.saturating_mul(3))
    }
}

/// Reads and writes [`AssistantConfig`]. Held on `AppState`.
pub struct AssistantService {
    db: PgPool,
}

impl std::fmt::Debug for AssistantService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantService").finish()
    }
}

impl AssistantService {
    /// Build the service.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Load the configuration, clamped. An absent key is the default config.
    pub async fn load_config(&self) -> Result<AssistantConfig> {
        let value = SiteConfig::get(&self.db, CONFIG_KEY)
            .await
            .context("failed to read ai_assistant_config")?;
        let mut config = match value {
            Some(value) => serde_json::from_value::<AssistantConfig>(value)
                .context("failed to parse ai_assistant_config")?,
            None => AssistantConfig::default(),
        };
        config.clamp();
        Ok(config)
    }

    /// Save the configuration, clamped.
    pub async fn save_config(&self, config: &AssistantConfig) -> Result<()> {
        let mut config = config.clone();
        config.clamp();
        let value =
            serde_json::to_value(&config).context("failed to serialize ai_assistant_config")?;
        SiteConfig::set(&self.db, CONFIG_KEY, value)
            .await
            .context("failed to save ai_assistant_config")
    }
}

// =============================================================================
// Truncation
// =============================================================================

/// Truncate a plugin's snapshot at a line boundary and say so.
///
/// The line boundary matters because a snapshot is labelled lines: cutting one
/// in half produces a half-fact ("last seen 2026-08-2"), which is worse than
/// dropping it. This is the second fence — the first is the 64 KiB tap output
/// buffer, which the plugin has to stay under itself.
pub fn truncate_snapshot(snapshot: &str, max_bytes: usize) -> String {
    if snapshot.len() <= max_bytes {
        return snapshot.to_string();
    }
    let mut end = max_bytes.min(snapshot.len());
    while end > 0 && !snapshot.is_char_boundary(end) {
        end -= 1;
    }
    let head = &snapshot[..end];
    let cut = head.rfind('\n').map(|i| i + 1).unwrap_or(head.len());
    format!("{}\n[snapshot truncated]", &head[..cut].trim_end())
}

/// Truncate a tool result and say so.
pub fn truncate_tool_result(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let mut end = max_bytes.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[result truncated]", &content[..end])
}

// =============================================================================
// Argument checking
// =============================================================================

/// Check a model's arguments against a tool's schema, shallowly.
///
/// Required keys present, and declared property types matching for the scalar
/// types; nothing deeper. This is not validation on the plugin's behalf — the
/// plugin still checks what it was sent — it is the cheapest thing that turns
/// the model's most common mistakes into a message it can act on rather than a
/// confusing failure inside somebody's tool.
pub fn check_arguments(schema: &Value, arguments: &Value) -> Result<(), String> {
    let Some(object) = arguments.as_object() else {
        return Err("arguments must be a JSON object".to_string());
    };

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key) || object[key].is_null() {
                return Err(format!("missing required argument '{key}'"));
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (key, declared) in properties {
            let Some(value) = object.get(key) else {
                continue;
            };
            let Some(expected) = declared.get("type").and_then(Value::as_str) else {
                continue;
            };
            let matches = match expected {
                "string" => value.is_string(),
                "number" => value.is_number(),
                "integer" => value.is_i64() || value.is_u64(),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                // Anything else (object, array, a union, no type at all) is the
                // plugin's business, not ours.
                _ => true,
            };
            if !matches {
                return Err(format!(
                    "argument '{key}' must be a {expected}, got {}",
                    json_type_name(value)
                ));
            }
        }
    }

    Ok(())
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// =============================================================================
// Message building
// =============================================================================

/// What a proposal's tool call is answered with, so the model knows it has been
/// heard and must not try again.
pub const PROPOSAL_TOOL_RESULT_NOTE: &str = "Waiting for the user to apply or discard this in the \
transcript. Do not call this tool again for the same change. Tell the user what you proposed and \
stop.";

/// The message that replaces history the bounding dropped.
pub const HISTORY_DROPPED_NOTE: &str = "[Trovato] Earlier parts of this conversation were dropped \
to save space. The context block above is current.";

/// Build the system message for a conversation.
pub fn system_message(
    config: &AssistantConfig,
    scope_prompt: &str,
    conversation: &Conversation,
) -> String {
    let mut message = String::with_capacity(
        config.core_prompt.len() + scope_prompt.len() + conversation.snapshot.len() + 128,
    );
    message.push_str(config.core_prompt.trim_end());
    message.push_str("\n\n");
    message.push_str(scope_prompt.trim());
    message.push_str("\n\n=== CONTEXT ===\n");
    message.push_str(conversation.snapshot.trim_end());
    message.push_str("\n=== END CONTEXT ===\n");
    match conversation.scope_id.as_deref() {
        Some(id) => message.push_str(&format!("Scope: {} (id {id})", conversation.scope)),
        None => message.push_str(&format!("Scope: {}", conversation.scope)),
    }
    message
}

/// Keep the last `max_exchanges` exchanges, and say whether anything was cut.
///
/// An exchange starts at a user entry and runs to the next one, so a tool call
/// and its result are never separated: everything between two user entries
/// moves together or not at all.
pub fn bound_history(
    entries: &[TranscriptEntry],
    max_exchanges: u32,
) -> (&[TranscriptEntry], bool) {
    let starts: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.starts_exchange())
        .map(|(index, _)| index)
        .collect();

    let max = max_exchanges.max(1) as usize;
    if starts.len() <= max {
        return (entries, false);
    }
    let cut = starts[starts.len() - max];
    (&entries[cut..], cut > 0)
}

/// Turn a bounded transcript into the model's message list.
///
/// Adjacent assistant messages are merged, because one model turn that both said
/// something and called a tool is stored as two entries but is one turn on the
/// wire — and because Anthropic will not take two assistant turns in a row.
pub fn transcript_to_messages(entries: &[TranscriptEntry]) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::with_capacity(entries.len());

    let push_assistant =
        |messages: &mut Vec<ChatMessage>, text: Option<String>, calls: Vec<ToolCall>| {
            if let Some(ChatMessage::Assistant {
                text: previous_text,
                tool_calls,
            }) = messages.last_mut()
            {
                if let Some(text) = text {
                    match previous_text {
                        Some(existing) if !existing.is_empty() => {
                            existing.push_str("\n\n");
                            existing.push_str(&text);
                        }
                        _ => *previous_text = Some(text),
                    }
                }
                tool_calls.extend(calls);
                return;
            }
            messages.push(ChatMessage::Assistant {
                text,
                tool_calls: calls,
            });
        };

    for entry in entries {
        match entry {
            TranscriptEntry::User { text, .. } => messages.push(ChatMessage::User(text.clone())),
            TranscriptEntry::Note { text, .. } => {
                messages.push(ChatMessage::User(format!("[Trovato] {text}")));
            }
            TranscriptEntry::Assistant { text, .. } => {
                push_assistant(&mut messages, Some(text.clone()), Vec::new());
            }
            TranscriptEntry::ToolCall {
                call_id,
                tool,
                arguments,
                ..
            } => {
                push_assistant(
                    &mut messages,
                    None,
                    vec![ToolCall {
                        id: call_id.clone(),
                        name: tool.clone(),
                        arguments: arguments.clone(),
                    }],
                );
            }
            TranscriptEntry::ToolResult {
                call_id,
                tool,
                ok,
                content,
                ..
            } => messages.push(ChatMessage::ToolResult {
                call_id: call_id.clone(),
                name: tool.clone(),
                content: content.clone(),
                is_error: !ok,
            }),
            TranscriptEntry::Proposal {
                proposal_id,
                call_id,
                tool,
                arguments,
                ..
            } => {
                // A proposal is the model's tool call plus the only answer it
                // can honestly be given: nothing happened, and asking again will
                // not change that.
                push_assistant(
                    &mut messages,
                    None,
                    vec![ToolCall {
                        id: call_id.clone(),
                        name: tool.clone(),
                        arguments: arguments.clone(),
                    }],
                );
                messages.push(ChatMessage::ToolResult {
                    call_id: call_id.clone(),
                    name: tool.clone(),
                    content: json!({
                        "status": "proposed",
                        "proposal_id": proposal_id,
                        "note": PROPOSAL_TOOL_RESULT_NOTE,
                    })
                    .to_string(),
                    is_error: false,
                });
            }
        }
    }

    messages
}

/// Build the full message list for one model call.
pub fn build_messages(
    config: &AssistantConfig,
    scope_prompt: &str,
    conversation: &Conversation,
    entries: &[TranscriptEntry],
) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage::System(system_message(
        config,
        scope_prompt,
        conversation,
    ))];
    let (bounded, dropped) = bound_history(entries, config.max_history_exchanges);
    if dropped {
        messages.push(ChatMessage::User(HISTORY_DROPPED_NOTE.to_string()));
    }
    messages.extend(transcript_to_messages(bounded));
    messages
}

/// The tools a scope offers the model. Reads and writes both: the model is
/// meant to ask for a write, it just cannot perform one.
pub fn tool_specs(scope: &RegisteredScope) -> Vec<ToolSpec> {
    scope
        .scope
        .tools
        .iter()
        .map(|tool| ToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        })
        .collect()
}

// =============================================================================
// Tap dispatch
// =============================================================================

/// Decode a tap's JSON output, tolerating the string-wrapped form a
/// `String`-returning tap would produce (G-VIEW-OUTPUT-JSON-ENCODED).
fn decode_tap_output<T: serde::de::DeserializeOwned>(raw: &str) -> Option<T> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let value = match value {
        Value::String(ref inner) => serde_json::from_str::<Value>(inner).unwrap_or(value),
        other => other,
    };
    serde_json::from_value(value).ok()
}

/// Ask a plugin to describe the thing being configured.
///
/// Dispatched with services and the caller's real user, so the plugin can query
/// its own tables and check the caller's permissions.
pub async fn dispatch_context(
    state: &AppState,
    user: &UserContext,
    plugin: &str,
    scope: &str,
    scope_id: Option<&str>,
) -> Option<AssistantContext> {
    let request =
        AssistantContextRequest::new(scope, scope_id.map(str::to_string), user.id.to_string());
    let payload = serde_json::to_string(&request).ok()?;
    let tap_state = RequestState::new(user.clone(), state.tap_services().clone());
    let result = state
        .tap_dispatcher()
        .dispatch_to_plugin("tap_assistant_context", &payload, plugin, tap_state)
        .await?;
    let context = decode_tap_output::<AssistantContext>(&result.output);
    if context.is_none() {
        warn!(
            plugin = %plugin,
            scope = %scope,
            output = %crate::services::ai_tools::truncate_on_char_boundary(&result.output, 200),
            "tap_assistant_context returned something that is not an AssistantContext"
        );
    }
    context
}

/// Dispatch one tool call to the plugin that owns the scope.
///
/// A trap, a timeout or an unparseable answer all become a failed result with a
/// generic message for the model; the detail goes to the log. The turn survives
/// either way, which is the point: one broken tool must not end a conversation.
pub async fn dispatch_tool(
    state: &AppState,
    user: &UserContext,
    plugin: &str,
    call: &AssistantToolCall,
) -> AssistantToolResult {
    let Ok(payload) = serde_json::to_string(call) else {
        return AssistantToolResult::failed("the tool call could not be encoded");
    };
    let tap_state = RequestState::new(user.clone(), state.tap_services().clone());
    let result = state
        .tap_dispatcher()
        .dispatch_to_plugin("tap_assistant_tool", &payload, plugin, tap_state)
        .await;

    match result {
        Some(result) => match decode_tap_output::<AssistantToolResult>(&result.output) {
            Some(parsed) => parsed,
            None => {
                warn!(
                    plugin = %plugin,
                    tool = %call.tool,
                    output = %crate::services::ai_tools::truncate_on_char_boundary(&result.output, 200),
                    "tap_assistant_tool returned something that is not an AssistantToolResult"
                );
                AssistantToolResult::failed("the tool returned an unreadable result")
            }
        },
        None => {
            warn!(plugin = %plugin, tool = %call.tool, "tap_assistant_tool dispatch failed");
            AssistantToolResult::failed("the tool did not run")
        }
    }
}

// =============================================================================
// The turn
// =============================================================================

/// Everything the turn loop needs that is not on [`AppState`].
pub struct TurnRequest {
    /// The conversation being added to.
    pub conversation: Conversation,
    /// The scope it is in.
    pub scope: RegisteredScope,
    /// The site's assistant configuration.
    pub config: AssistantConfig,
    /// The provider, already resolved so the budget check and the call cannot
    /// disagree about which one is being spent.
    pub resolved: ResolvedProvider,
    /// The caller, with real permissions, for tap dispatch.
    pub user: UserContext,
}

/// One SSE event, as the client sees it.
fn event(kind: &str, mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.insert("type".to_string(), json!(kind));
    }
    body
}

/// Run one message's turn, emitting SSE payloads as it goes.
///
/// The transcript is written after every tool execution as well as at the end,
/// so a crash mid-turn loses at most one model call rather than the exchange.
pub fn run_turn(
    state: AppState,
    request: TurnRequest,
) -> impl futures_core::Stream<Item = Value> + Send {
    async_stream::stream! {
        let TurnRequest { conversation, scope, config, resolved, user } = request;
        let db = state.db().clone();
        let plugin = scope.plugin.clone();
        let conversation_id = conversation.id;
        let started = Instant::now();
        let deadline = config.turn_deadline();

        let mut entries = conversation.entries();
        let mut tokens_used = conversation.tokens_used;
        let message_count = conversation.message_count;
        let mut tool_calls_made: u32 = 0;
        // The model that answered, which is what a proposal records. Starts as
        // the one asked for and becomes the one the provider says it used.
        let mut model_used = resolved.model.clone();
        let _ = &model_used;

        yield event("turn_start", json!({}));

        'turn: loop {
            if started.elapsed() > deadline {
                let text = "This message took too long and was stopped.".to_string();
                entries.push(TranscriptEntry::Note { text: text.clone(), ts: epoch_now() });
                yield event("note", json!({"text": text}));
                break 'turn;
            }

            let messages = build_messages(
                &config,
                config.scope_prompt(&scope),
                &conversation,
                &entries,
            );
            let specs = tool_specs(&scope);

            let completion: ChatCompletion = match chat_complete(
                state.ai_providers(),
                &resolved,
                &messages,
                &specs,
                config.temperature,
                config.max_response_tokens,
                config.call_timeout(),
            )
            .await
            {
                Ok(completion) => completion,
                Err(e) => {
                    // The detail is for the log; the person gets a sentence.
                    warn!(
                        conversation = %conversation_id,
                        scope = %scope.scope.name,
                        error = %e,
                        "assistant model call failed"
                    );
                    yield event("error", json!({"message": e.user_message()}));
                    break 'turn;
                }
            };

            model_used = completion.model.clone();
            tokens_used = tokens_used.saturating_add(i64::from(completion.usage.2));
            record_usage(&state, &db, &user, &resolved, &completion).await;

            if let Some(text) = completion.text.as_ref().filter(|t| !t.trim().is_empty()) {
                entries.push(TranscriptEntry::Assistant {
                    text: text.clone(),
                    ts: epoch_now(),
                });
                yield event("assistant", json!({"text": text}));
            }

            if completion.tool_calls.is_empty() {
                break 'turn;
            }

            for call in &completion.tool_calls {
                if tool_calls_made >= config.max_tool_calls_per_message {
                    let text = "Tool call limit reached for this message.".to_string();
                    entries.push(TranscriptEntry::Note { text: text.clone(), ts: epoch_now() });
                    yield event("note", json!({"text": text}));
                    let _ = Conversation::save_transcript(
                        &db, conversation_id, &entries, message_count, tokens_used,
                    ).await;
                    break 'turn;
                }
                tool_calls_made += 1;

                // A tool the scope did not declare is never dispatched: the
                // model is told so and the loop carries on.
                let Some(declared) = scope.tool(&call.name).cloned() else {
                    for value in refuse_call(
                        &mut entries,
                        call,
                        format!("no such tool: {}", call.name),
                    ) {
                        yield value;
                    }
                    continue;
                };

                if let Err(reason) = check_arguments(&declared.parameters, &call.arguments) {
                    for value in refuse_call(&mut entries, call, reason) {
                        yield value;
                    }
                    continue;
                }

                match declared.kind {
                    AssistantToolKind::Read => {
                        let tool_call = AssistantToolCall::new(
                            conversation.scope.clone(),
                            conversation.scope_id.clone(),
                            call.name.clone(),
                            call.arguments.clone(),
                            AssistantToolMode::Execute,
                            user.id.to_string(),
                        );
                        let result = dispatch_tool(&state, &user, &plugin, &tool_call).await;
                        let content =
                            truncate_tool_result(&result.content, config.tool_result_max_bytes);
                        let ts = epoch_now();
                        entries.push(TranscriptEntry::ToolCall {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            arguments: call.arguments.clone(),
                            ts,
                        });
                        entries.push(TranscriptEntry::ToolResult {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            ok: result.ok,
                            summary: result.summary.clone(),
                            content: content.clone(),
                            ts,
                        });
                        yield event("tool_call", json!({
                            "call_id": call.id,
                            "tool": call.name,
                            "arguments": call.arguments,
                        }));
                        yield event("tool_result", json!({
                            "call_id": call.id,
                            "tool": call.name,
                            "ok": result.ok,
                            "summary": result.summary.clone()
                                .unwrap_or_else(|| summarize(&content)),
                        }));
                    }
                    AssistantToolKind::Write => {
                        let proposal_id = Uuid::now_v7();
                        let tool_call = AssistantToolCall::new(
                            conversation.scope.clone(),
                            conversation.scope_id.clone(),
                            call.name.clone(),
                            call.arguments.clone(),
                            AssistantToolMode::Describe,
                            user.id.to_string(),
                        )
                        .proposal(proposal_id.to_string());
                        let described = dispatch_tool(&state, &user, &plugin, &tool_call).await;

                        // A Describe that failed or said nothing still yields a
                        // proposal, described from the call itself: the person
                        // must be able to see and refuse what was asked for even
                        // when the plugin was unhelpful about naming it.
                        let description = described
                            .summary
                            .clone()
                            .filter(|s| described.ok && !s.trim().is_empty())
                            .unwrap_or_else(|| {
                                format!("{} with {}", call.name, compact(&call.arguments))
                            });
                        let risk = risk_name(declared.risk);

                        let proposal = Proposal {
                            id: proposal_id,
                            conversation_id,
                            user_id: user.id,
                            scope: conversation.scope.clone(),
                            scope_id: conversation.scope_id.clone(),
                            tool: call.name.clone(),
                            arguments: call.arguments.clone(),
                            description: description.clone(),
                            risk: risk.to_string(),
                            status: PROPOSAL_PROPOSED.to_string(),
                            result: None,
                            model: model_used.clone(),
                            created: epoch_now(),
                            resolved: None,
                            resolved_by: None,
                        };
                        if let Err(e) = Proposal::create(&db, &proposal).await {
                            warn!(error = %e, "failed to record a proposal");
                            for value in refuse_call(
                                &mut entries,
                                call,
                                "the proposal could not be recorded".to_string(),
                            ) {
                                yield value;
                            }
                            continue;
                        }

                        entries.push(TranscriptEntry::Proposal {
                            proposal_id: proposal_id.to_string(),
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            arguments: call.arguments.clone(),
                            description: description.clone(),
                            risk: risk.to_string(),
                            ts: epoch_now(),
                        });
                        yield event("proposal", json!({
                            "proposal_id": proposal_id.to_string(),
                            "tool": call.name,
                            "description": description,
                            "risk": risk,
                            "status": PROPOSAL_PROPOSED,
                        }));
                    }
                }

                // After every tool, so a crash costs one model call at most.
                if let Err(e) = Conversation::save_transcript(
                    &db, conversation_id, &entries, message_count, tokens_used,
                ).await {
                    warn!(error = %e, "failed to save the transcript mid-turn");
                }
            }
        }

        // The conversation may now be full. It is not closed: the transcript
        // stays readable and Start over is the way on.
        if message_count >= config.max_messages as i32 {
            let text = "This conversation has reached its message limit. \
                        Start over to keep going.".to_string();
            entries.push(TranscriptEntry::Note { text: text.clone(), ts: epoch_now() });
            yield event("note", json!({"text": text}));
        } else if tokens_used >= config.max_tokens_per_conversation as i64 {
            let text = "This conversation has reached its token limit. \
                        Start over to keep going.".to_string();
            entries.push(TranscriptEntry::Note { text: text.clone(), ts: epoch_now() });
            yield event("note", json!({"text": text}));
        }

        if let Err(e) = Conversation::save_transcript(
            &db, conversation_id, &entries, message_count, tokens_used,
        ).await {
            warn!(error = %e, "failed to save the transcript at the end of a turn");
        }

        yield event("done", json!({
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
            "tokens_used": tokens_used,
            "message_count": message_count,
        }));
    }
}

/// Append a call and a refusal, and return the two events for them.
fn refuse_call(entries: &mut Vec<TranscriptEntry>, call: &ToolCall, reason: String) -> Vec<Value> {
    let ts = epoch_now();
    entries.push(TranscriptEntry::ToolCall {
        call_id: call.id.clone(),
        tool: call.name.clone(),
        arguments: call.arguments.clone(),
        ts,
    });
    entries.push(TranscriptEntry::ToolResult {
        call_id: call.id.clone(),
        tool: call.name.clone(),
        ok: false,
        summary: Some(reason.clone()),
        content: reason.clone(),
        ts,
    });
    vec![
        event(
            "tool_call",
            json!({"call_id": call.id, "tool": call.name, "arguments": call.arguments}),
        ),
        event(
            "tool_result",
            json!({"call_id": call.id, "tool": call.name, "ok": false, "summary": reason}),
        ),
    ]
}

/// Record one model call against the caller's budget and the usage log.
async fn record_usage(
    state: &AppState,
    db: &PgPool,
    user: &UserContext,
    resolved: &ResolvedProvider,
    completion: &ChatCompletion,
) {
    let (prompt, completion_tokens, total) = completion.usage;
    let cost_estimate = state
        .ai_budgets()
        .estimate_cost(
            &completion.model,
            i64::from(prompt),
            i64::from(completion_tokens),
        )
        .await;
    let entry = UsageLogEntry {
        user_id: Some(user.id),
        plugin_name: USAGE_PLUGIN.to_string(),
        provider_id: resolved.config.id.clone(),
        operation: AiOperationType::Chat.to_string(),
        model: completion.model.clone(),
        prompt_tokens: prompt.min(i32::MAX as u32) as i32,
        completion_tokens: completion_tokens.min(i32::MAX as u32) as i32,
        total_tokens: total.min(i32::MAX as u32) as i32,
        latency_ms: completion.latency_ms.min(i64::MAX as u64) as i64,
        cost_estimate,
    };
    if let Err(e) = state.ai_budgets().record_usage(db, entry).await {
        warn!(error = %e, "failed to record assistant usage");
    }
    debug!(tokens = total, "assistant model call complete");
}

/// A one-line stand-in for a result the plugin did not summarize.
fn summarize(content: &str) -> String {
    let line = content.lines().next().unwrap_or_default().trim();
    crate::services::ai_tools::truncate_on_char_boundary(line, 160)
}

/// Compact JSON for a description fallback.
fn compact(value: &Value) -> String {
    crate::services::ai_tools::truncate_on_char_boundary(&value.to_string(), 200)
}

/// The wire name of a risk level, matching the SDK's serde representation.
pub fn risk_name(risk: trovato_sdk::types::AssistantRisk) -> &'static str {
    match risk {
        trovato_sdk::types::AssistantRisk::Low => "low",
        trovato_sdk::types::AssistantRisk::Normal => "normal",
        trovato_sdk::types::AssistantRisk::High => "high",
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use trovato_sdk::types::{AssistantIdKind, AssistantRisk, AssistantScope, AssistantTool};

    fn conversation(snapshot: &str, scope_id: Option<&str>) -> Conversation {
        Conversation {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            plugin: "widgets".into(),
            scope: "test_widget".into(),
            scope_id: scope_id.map(str::to_string),
            title: "Widget 7".into(),
            status: "open".into(),
            snapshot: snapshot.into(),
            links: json!([]),
            transcript: json!([]),
            message_count: 0,
            tokens_used: 0,
            created: 0,
            changed: 0,
        }
    }

    fn registered() -> RegisteredScope {
        RegisteredScope {
            plugin: "widgets".into(),
            scope: AssistantScope::new("test_widget", "Widget", "p", AssistantIdKind::String)
                .prompt("You configure a test widget.")
                .tool(AssistantTool::read("read_widget", "Read it"))
                .tool(
                    AssistantTool::write("set_widget_color", "Set it", AssistantRisk::Normal)
                        .parameters(json!({
                            "type": "object",
                            "required": ["color"],
                            "properties": {"color": {"type": "string"}, "shade": {"type": "integer"}}
                        })),
                ),
        }
    }

    fn user(text: &str) -> TranscriptEntry {
        TranscriptEntry::User {
            text: text.into(),
            ts: 1,
        }
    }

    // -------------------------------------------------------------------------
    // Configuration
    // -------------------------------------------------------------------------

    #[test]
    fn an_absent_config_is_the_default_config() {
        let config: AssistantConfig = serde_json::from_str("{}").unwrap();
        assert!(
            !config.enabled,
            "the assistant is off until a site turns it on"
        );
        assert_eq!(config.temperature, 0.2);
        assert_eq!(config.max_tool_calls_per_message, 8);
        assert_eq!(config.core_prompt, DEFAULT_CORE_PROMPT);
    }

    #[test]
    fn clamping_brings_every_field_into_range() {
        let mut config = AssistantConfig {
            temperature: f32::NAN,
            turn_timeout_secs: 10_000,
            max_tool_calls_per_message: 0,
            max_messages: 1,
            max_tokens_per_conversation: 1,
            max_history_exchanges: 0,
            max_response_tokens: 1,
            snapshot_max_bytes: 10,
            tool_result_max_bytes: 10_000_000,
            rate_limit_per_hour: 99_999,
            core_prompt: "   ".to_string(),
            ..AssistantConfig::default()
        };
        config.clamp();

        assert_eq!(
            config.temperature, 0.2,
            "NaN falls back rather than clamping"
        );
        assert_eq!(config.turn_timeout_secs, 150);
        assert_eq!(config.max_tool_calls_per_message, 1);
        assert_eq!(config.max_messages, 4);
        assert_eq!(config.max_tokens_per_conversation, 1_000);
        assert_eq!(config.max_history_exchanges, 1);
        assert_eq!(config.max_response_tokens, 64);
        assert_eq!(config.snapshot_max_bytes, 1_024);
        assert_eq!(config.tool_result_max_bytes, 32_768);
        assert_eq!(config.rate_limit_per_hour, 1_000);
        assert_eq!(config.core_prompt, DEFAULT_CORE_PROMPT);
    }

    #[test]
    fn a_scope_override_replaces_the_stock_prompt_and_can_disable_the_scope() {
        let mut config = AssistantConfig::default();
        let scope = registered();
        assert_eq!(config.scope_prompt(&scope), "You configure a test widget.");
        assert!(config.scope_enabled("test_widget"));

        config.scopes.insert(
            "test_widget".to_string(),
            AssistantScopeConfig {
                enabled: false,
                prompt_override: Some("Say only 'no'.".to_string()),
            },
        );
        assert_eq!(config.scope_prompt(&scope), "Say only 'no'.");
        assert!(!config.scope_enabled("test_widget"));

        // A blank override is not an override.
        config.scopes.insert(
            "test_widget".to_string(),
            AssistantScopeConfig {
                enabled: true,
                prompt_override: Some("   ".to_string()),
            },
        );
        assert_eq!(config.scope_prompt(&scope), "You configure a test widget.");
    }

    // -------------------------------------------------------------------------
    // Truncation
    // -------------------------------------------------------------------------

    #[test]
    fn a_snapshot_is_cut_at_a_line_boundary_and_says_so() {
        let snapshot = "name: Widget\ncolor: teal\nlast seen: 2026-08-24T09:00:00Z\n";
        let cut = truncate_snapshot(snapshot, 30);
        assert!(cut.ends_with("[snapshot truncated]"), "{cut}");
        // The whole point: no half-facts. Every surviving line is complete.
        for line in cut.lines().filter(|l| *l != "[snapshot truncated]") {
            assert!(
                snapshot.contains(&format!("{line}\n")),
                "half a line: {line}"
            );
        }
        // Under the cap it is returned untouched.
        assert_eq!(truncate_snapshot(snapshot, 10_000), snapshot);
    }

    #[test]
    fn a_tool_result_is_cut_and_says_so() {
        let content = "x".repeat(100);
        let cut = truncate_tool_result(&content, 20);
        assert!(cut.ends_with("[result truncated]"), "{cut}");
        assert_eq!(truncate_tool_result("short", 20), "short");
    }

    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        let snapshot = "naïve café ☕ widget\nsecond line\n";
        for max in 1..snapshot.len() {
            let cut = truncate_snapshot(snapshot, max);
            assert!(cut.is_char_boundary(cut.len()));
        }
    }

    // -------------------------------------------------------------------------
    // Argument checking
    // -------------------------------------------------------------------------

    #[test]
    fn arguments_are_checked_for_required_keys_and_declared_types() {
        let schema = json!({
            "type": "object",
            "required": ["color"],
            "properties": {
                "color": {"type": "string"},
                "shade": {"type": "integer"},
                "extra": {"type": "object"}
            }
        });

        assert!(check_arguments(&schema, &json!({"color": "teal"})).is_ok());
        assert!(check_arguments(&schema, &json!({"color": "teal", "shade": 3})).is_ok());
        // An undeclared key is the plugin's business, not ours.
        assert!(check_arguments(&schema, &json!({"color": "teal", "who": 1})).is_ok());

        let missing = check_arguments(&schema, &json!({})).unwrap_err();
        assert!(missing.contains("color"), "{missing}");

        let wrong = check_arguments(&schema, &json!({"color": 7})).unwrap_err();
        assert!(wrong.contains("string"), "{wrong}");

        let float = check_arguments(&schema, &json!({"color": "t", "shade": 1.5})).unwrap_err();
        assert!(float.contains("integer"), "{float}");

        // The `_unparsed` shape a malformed OpenAI arguments string becomes.
        let unparsed = check_arguments(&schema, &json!({"_unparsed": "{not json"})).unwrap_err();
        assert!(unparsed.contains("color"), "{unparsed}");

        // Not an object at all.
        assert!(check_arguments(&schema, &json!("teal")).is_err());
        // An explicit null does not satisfy a required key.
        assert!(check_arguments(&schema, &json!({"color": null})).is_err());
    }

    // -------------------------------------------------------------------------
    // Message building
    // -------------------------------------------------------------------------

    #[test]
    fn the_system_message_carries_the_prompts_the_context_and_the_scope() {
        let config = AssistantConfig::default();
        let scope = registered();
        let message = system_message(
            &config,
            config.scope_prompt(&scope),
            &conversation("Widget 7 has color teal.", Some("7")),
        );
        assert!(message.starts_with("You are Trovato's configuration assistant."));
        assert!(message.contains("You configure a test widget."));
        assert!(message.contains("=== CONTEXT ===\nWidget 7 has color teal.\n=== END CONTEXT ==="));
        assert!(message.ends_with("Scope: test_widget (id 7)"));

        let no_id = system_message(&config, "p", &conversation("s", None));
        assert!(no_id.ends_with("Scope: test_widget"));
    }

    #[test]
    fn history_bounding_keeps_whole_exchanges_and_reports_the_cut() {
        let entries = vec![
            user("one"),
            TranscriptEntry::Assistant {
                text: "a".into(),
                ts: 1,
            },
            user("two"),
            TranscriptEntry::ToolCall {
                call_id: "c".into(),
                tool: "read_widget".into(),
                arguments: json!({}),
                ts: 2,
            },
            TranscriptEntry::ToolResult {
                call_id: "c".into(),
                tool: "read_widget".into(),
                ok: true,
                summary: None,
                content: "teal".into(),
                ts: 2,
            },
            user("three"),
        ];

        let (kept, dropped) = bound_history(&entries, 1);
        assert!(dropped);
        assert_eq!(kept.len(), 1, "only the last exchange survives");

        let (kept, dropped) = bound_history(&entries, 2);
        assert!(dropped);
        // A call and its result move with their exchange or not at all.
        assert_eq!(kept.len(), 4);
        assert!(matches!(kept[0], TranscriptEntry::User { .. }));
        assert!(matches!(kept[1], TranscriptEntry::ToolCall { .. }));

        let (kept, dropped) = bound_history(&entries, 12);
        assert!(!dropped);
        assert_eq!(kept.len(), entries.len());
    }

    #[test]
    fn a_dropped_history_puts_the_notice_first_in_the_message_list() {
        let config = AssistantConfig {
            max_history_exchanges: 1,
            ..AssistantConfig::default()
        };
        let entries = vec![user("one"), user("two")];
        let messages = build_messages(&config, "p", &conversation("s", None), &entries);

        assert!(matches!(messages[0], ChatMessage::System(_)));
        match &messages[1] {
            ChatMessage::User(text) => assert_eq!(text, HISTORY_DROPPED_NOTE),
            other => panic!("expected the dropped-history notice, got {other:?}"),
        }
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn a_tool_call_and_its_result_become_an_assistant_turn_and_a_tool_message() {
        let entries = vec![
            user("what colour?"),
            TranscriptEntry::ToolCall {
                call_id: "c1".into(),
                tool: "read_widget".into(),
                arguments: json!({}),
                ts: 1,
            },
            TranscriptEntry::ToolResult {
                call_id: "c1".into(),
                tool: "read_widget".into(),
                ok: true,
                summary: None,
                content: r#"{"color":"teal"}"#.into(),
                ts: 1,
            },
            TranscriptEntry::Assistant {
                text: "It is teal.".into(),
                ts: 2,
            },
        ];
        let messages = transcript_to_messages(&entries);

        assert_eq!(messages.len(), 4);
        match &messages[1] {
            ChatMessage::Assistant { text, tool_calls } => {
                assert!(text.is_none());
                assert_eq!(tool_calls[0].name, "read_widget");
            }
            other => panic!("expected an assistant turn, got {other:?}"),
        }
        match &messages[2] {
            ChatMessage::ToolResult {
                call_id, is_error, ..
            } => {
                assert_eq!(call_id, "c1");
                assert!(!is_error);
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn a_proposal_is_replayed_as_its_call_answered_with_waiting() {
        let entries = vec![
            user("make it teal"),
            TranscriptEntry::Proposal {
                proposal_id: "p1".into(),
                call_id: "c9".into(),
                tool: "set_widget_color".into(),
                arguments: json!({"color": "teal"}),
                description: "Set widget color to teal".into(),
                risk: "normal".into(),
                ts: 1,
            },
        ];
        let messages = transcript_to_messages(&entries);

        match &messages[1] {
            ChatMessage::Assistant { tool_calls, .. } => {
                // Rebuilt from the entry, which is why the entry carries them.
                assert_eq!(tool_calls[0].arguments["color"], "teal");
                assert_eq!(tool_calls[0].id, "c9");
            }
            other => panic!("expected the model's own call, got {other:?}"),
        }
        match &messages[2] {
            ChatMessage::ToolResult { content, .. } => {
                let parsed: Value = serde_json::from_str(content).unwrap();
                assert_eq!(parsed["status"], "proposed");
                assert_eq!(parsed["proposal_id"], "p1");
                assert!(
                    parsed["note"]
                        .as_str()
                        .unwrap()
                        .contains("Do not call this tool again")
                );
            }
            other => panic!("expected the waiting result, got {other:?}"),
        }
    }

    #[test]
    fn a_note_reaches_the_model_as_a_trovato_user_message() {
        let entries = vec![TranscriptEntry::Note {
            text: "Applied: Set widget color to teal. Widget color is now teal".into(),
            ts: 1,
        }];
        match &transcript_to_messages(&entries)[0] {
            ChatMessage::User(text) => {
                assert!(text.starts_with("[Trovato] Applied:"), "{text}");
            }
            other => panic!("expected a user message, got {other:?}"),
        }
    }

    #[test]
    fn one_model_turn_that_spoke_and_called_a_tool_is_one_assistant_message() {
        // Stored as two entries, but it was one turn — and Anthropic refuses two
        // assistant turns in a row.
        let entries = vec![
            TranscriptEntry::Assistant {
                text: "Let me look.".into(),
                ts: 1,
            },
            TranscriptEntry::ToolCall {
                call_id: "c1".into(),
                tool: "read_widget".into(),
                arguments: json!({}),
                ts: 1,
            },
        ];
        let messages = transcript_to_messages(&entries);
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            ChatMessage::Assistant { text, tool_calls } => {
                assert_eq!(text.as_deref(), Some("Let me look."));
                assert_eq!(tool_calls.len(), 1);
            }
            other => panic!("expected one merged assistant turn, got {other:?}"),
        }
    }

    #[test]
    fn tool_specs_offer_reads_and_writes_alike() {
        // The model is meant to *ask* for a write. It just cannot perform one.
        let specs = tool_specs(&registered());
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["read_widget", "set_widget_color"]);
    }

    #[test]
    fn risk_names_match_the_sdk_wire_form() {
        for (risk, name) in [
            (AssistantRisk::Low, "low"),
            (AssistantRisk::Normal, "normal"),
            (AssistantRisk::High, "high"),
        ] {
            assert_eq!(risk_name(risk), name);
            assert_eq!(serde_json::to_value(risk).unwrap(), json!(name));
        }
    }
}
