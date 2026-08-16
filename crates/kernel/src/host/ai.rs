//! AI API host function for WASM plugins.
//!
//! Provides `ai-request` under the `trovato:kernel/ai-api` WIT interface.
//! The kernel resolves the provider, injects the API key, makes the HTTP
//! request, and returns a normalized `AiResponse`. API keys never cross
//! the WASM boundary.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};
use wasmtime::Linker;

use crate::plugin::{PluginState, WasmtimeExt};
use crate::services::ai_provider::{
    EMBEDDING_INPUT_MAX_CHARS, ProviderProtocol, ResolvedProvider, cap_embedding_input,
};
use crate::services::ai_token_budget::{BudgetAction, UsageLogEntry};
use crate::tap::UserContext;
use trovato_sdk::host_errors;
use trovato_sdk::types::{AiRequest, AiResponse, AiUsage};

use super::{read_string_from_memory, write_string_to_memory};

// =============================================================================
// Rate limiter (best-effort, in-memory, per-provider)
// =============================================================================

/// Per-provider rate limit state.
struct RateWindow {
    count: AtomicU64,
    window_start: Mutex<Instant>,
}

/// Simple in-memory per-provider RPM rate limiter.
///
/// Uses a fixed 60-second sliding window. Not distributed — sufficient
/// for single-instance deployments.
static RATE_LIMITS: std::sync::LazyLock<Mutex<HashMap<String, RateWindow>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Check and increment the rate counter for a provider.
///
/// Returns `true` if the request is allowed, `false` if rate limited.
/// Only increments the counter when the request is allowed (not on rejection).
fn check_rate_limit(provider_id: &str, rpm_limit: u32) -> bool {
    if rpm_limit == 0 {
        return true;
    }

    let mut map = RATE_LIMITS.lock().unwrap_or_else(|e| e.into_inner());

    // Evict stale entries (windows older than 2 minutes) to prevent unbounded growth.
    // Provider IDs are admin-configured UUIDs so the map is naturally small,
    // but this guards against edge cases (e.g. deleted providers).
    if map.len() > 50 {
        map.retain(|_, w| {
            w.window_start
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed()
                .as_secs()
                < 120
        });
    }

    let window = map
        .entry(provider_id.to_string())
        .or_insert_with(|| RateWindow {
            count: AtomicU64::new(0),
            window_start: Mutex::new(Instant::now()),
        });

    let mut start = window
        .window_start
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if start.elapsed().as_secs() >= 60 {
        window.count.store(0, Ordering::Relaxed);
        *start = Instant::now();
    }

    let current = window.count.load(Ordering::Relaxed);
    if current >= u64::from(rpm_limit) {
        return false;
    }
    window.count.fetch_add(1, Ordering::Relaxed);
    true
}

// =============================================================================
// Permission mapping
// =============================================================================

/// Map an SDK `AiOperationType` to the required operation-specific permission.
///
/// All AI operations require `use ai` as a base permission (checked separately).
/// This returns the *additional* operation-specific permission, or `"use ai"`
/// when only the base permission is needed.
fn permission_for_operation(op: &trovato_sdk::types::AiOperationType) -> &'static str {
    use trovato_sdk::types::AiOperationType;
    match op {
        AiOperationType::Chat => "use ai chat",
        AiOperationType::Embedding => "use ai embeddings",
        AiOperationType::ImageGeneration => "use ai image generation",
        // Future operations: base permission only
        _ => "use ai",
    }
}

/// Authorize an `ai-request` against the caller's principal (P11c / D-40, D-41).
///
/// Two **disjoint** authorization planes, selected by [`UserContext::is_background`]:
///
/// - **Background principal** (cron / queue worker — the only contexts that set
///   the marker): authorized **iff** the calling plugin declared the
///   `ai_background` manifest capability. The human `use ai` permission plane is
///   deliberately *not* consulted — a background context carries no human
///   identity and holds no permissions. Denied → [`ERR_AI_BACKGROUND_DENIED`].
/// - **Web / user** (every non-background context): the pre-P11c gate, unchanged
///   — the caller must hold `use ai`, and for a typed operation the
///   operation-specific permission too. Denied → [`ERR_AI_PERMISSION_DENIED`].
///   This branch is keyed on `has_permission("use ai")`, **not** the
///   `authenticated` flag, so an anonymous web caller (empty permission set) is
///   denied here exactly as before.
///
/// Returns `Ok(())` when the call may proceed, otherwise the host error code the
/// caller must return. Pure over its inputs so the D-40/D-41 invariants are unit
/// testable without a live provider.
///
/// [`ERR_AI_BACKGROUND_DENIED`]: host_errors::ERR_AI_BACKGROUND_DENIED
/// [`ERR_AI_PERMISSION_DENIED`]: host_errors::ERR_AI_PERMISSION_DENIED
fn authorize_ai_request(
    user: &UserContext,
    ai_background_capability: bool,
    operation: &trovato_sdk::types::AiOperationType,
) -> Result<(), i32> {
    if user.is_background() {
        // Background principal: gated solely by the declared manifest capability
        // (D-41), never by the `use ai` permission plane.
        if ai_background_capability {
            return Ok(());
        }
        return Err(host_errors::ERR_AI_BACKGROUND_DENIED);
    }

    // Web / user plane — byte-for-byte the pre-P11c denial, keyed on the
    // `use ai` permission check (not the `authenticated` flag).
    if !user.has_permission("use ai") {
        return Err(host_errors::ERR_AI_PERMISSION_DENIED);
    }
    let op_perm = permission_for_operation(operation);
    if op_perm != "use ai" && !user.has_permission(op_perm) {
        return Err(host_errors::ERR_AI_PERMISSION_DENIED);
    }
    Ok(())
}

// =============================================================================
// HTTP request building
// =============================================================================

/// One outbound provider request, ready to send: `(url, body, headers)`.
type BuiltRequest = (String, String, Vec<(String, String)>);

/// Build and execute an HTTP request based on the provider protocol.
///
/// `timeout` is the per-request outbound timeout resolved from the AI timeout
/// config (P11c / D-43); it overrides the shared client's default so a
/// background/analyze-class call is not clipped at the request-scoped default,
/// and a request-scoped call is still cut at its (short) configured timeout.
async fn execute_ai_request(
    http: &reqwest::Client,
    resolved: &ResolvedProvider,
    request: &AiRequest,
    timeout: std::time::Duration,
) -> Result<(String, u16), (i32, String)> {
    // Branch on the *operation* first, then the protocol. Before K1 fix 2 this
    // branched on protocol alone, so `operation: Embedding` was posted to
    // `/chat/completions` with an empty `messages` array and `request.input` was
    // never read at all (G-AI-EMBED-UNROUTED, Argus M2).
    let (url, body, headers) = match request.operation {
        trovato_sdk::types::AiOperationType::Chat => match resolved.config.protocol {
            ProviderProtocol::OpenAiCompatible => build_openai_request(resolved, request),
            ProviderProtocol::Anthropic => build_anthropic_request(resolved, request),
        },
        trovato_sdk::types::AiOperationType::Embedding => {
            build_openai_embedding_request(resolved, request)?
        }
        other => {
            return Err((
                host_errors::ERR_AI_OPERATION_UNSUPPORTED,
                format!("the kernel serves no route for AI operation {other:?}"),
            ));
        }
    };

    let mut req = http.post(&url).timeout(timeout);
    for (key, value) in &headers {
        req = req.header(key.as_str(), value.as_str());
    }
    req = req.header("content-type", "application/json");
    req = req.body(body);

    let response = req.send().await.map_err(|e| {
        (
            host_errors::ERR_AI_REQUEST_FAILED,
            format!("HTTP request failed: {e}"),
        )
    })?;

    let status = response.status().as_u16();
    let body = response.text().await.map_err(|e| {
        (
            host_errors::ERR_AI_REQUEST_FAILED,
            format!("Failed to read response body: {e}"),
        )
    })?;

    // Map HTTP errors to specific error codes
    match status {
        200..=299 => {}
        401 | 403 => {
            return Err((
                host_errors::ERR_AI_AUTH_FAILED,
                format!("Authentication failed (HTTP {status})"),
            ));
        }
        429 => {
            return Err((
                host_errors::ERR_AI_RATE_LIMITED,
                "Rate limited by provider (HTTP 429)".to_string(),
            ));
        }
        _ => {
            let truncated = if body.len() > 200 {
                // Find a safe char boundary to avoid panicking on multi-byte UTF-8
                let mut end = 200;
                while end > 0 && !body.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &body[..end])
            } else {
                body
            };
            return Err((
                host_errors::ERR_AI_PROVIDER_ERROR,
                format!("Provider error (HTTP {status}): {truncated}"),
            ));
        }
    }

    Ok((body, status))
}

/// Build an OpenAI-compatible chat completions request.
fn build_openai_request(resolved: &ResolvedProvider, request: &AiRequest) -> BuiltRequest {
    let url = format!(
        "{}/chat/completions",
        resolved.config.base_url.trim_end_matches('/')
    );

    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content,
            })
        })
        .collect();

    let mut body = serde_json::json!({
        "model": resolved.model,
        "messages": messages,
    });

    if let Some(max_tokens) = request.options.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }
    if let Some(temperature) = request.options.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(top_p) = request.options.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(ref stop) = request.options.stop
        && !stop.is_empty()
    {
        body["stop"] = serde_json::json!(stop);
    }

    let mut headers = Vec::new();
    if let Some(ref key) = resolved.api_key {
        headers.push(("authorization".to_string(), format!("Bearer {key}")));
    }

    // Infallible: serde_json::Value serialization to string cannot fail.
    let body_str = serde_json::to_string(&body).unwrap_or_default();
    (url, body_str, headers)
}

/// Build an OpenAI-compatible **embeddings** request (K1 fix 2,
/// G-AI-EMBED-UNROUTED).
///
/// Reads [`AiRequest::input`] — the field the SDK has always carried for this
/// operation and which the chat builder never looked at — and posts it to
/// `{base_url}/embeddings`, the same endpoint and body shape the kernel's own
/// [`crate::services::ai_provider::AiProviderService::embed`] uses, so the two
/// callers cannot drift.
///
/// Input is capped at [`EMBEDDING_INPUT_MAX_CHARS`] by the same helper the
/// kernel path uses: over-budget input would otherwise be rejected by the
/// provider, and unbounded payloads are a cost surface.
///
/// Returns `Err` rather than a request when the operation cannot be served:
/// Anthropic exposes no embeddings API, and an embedding with no input is not
/// a request at all.
fn build_openai_embedding_request(
    resolved: &ResolvedProvider,
    request: &AiRequest,
) -> Result<BuiltRequest, (i32, String)> {
    if resolved.config.protocol != ProviderProtocol::OpenAiCompatible {
        return Err((
            host_errors::ERR_AI_OPERATION_UNSUPPORTED,
            format!(
                "provider protocol {} exposes no embeddings API",
                resolved.config.protocol
            ),
        ));
    }

    let input = request.input.as_deref().unwrap_or("");
    if input.trim().is_empty() {
        return Err((
            host_errors::ERR_AI_INVALID_REQUEST,
            "an Embedding request requires a non-empty `input`".to_string(),
        ));
    }

    let (input, truncated_from) = cap_embedding_input(input);
    if let Some(original) = truncated_from {
        warn!(
            original_chars = original,
            capped_chars = EMBEDDING_INPUT_MAX_CHARS,
            "plugin embedding input exceeds budget; truncating before request"
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

    // Infallible: serde_json::Value serialization to string cannot fail.
    let body_str = serde_json::to_string(&body).unwrap_or_default();
    Ok((url, body_str, headers))
}

/// Build an Anthropic Messages API request.
fn build_anthropic_request(resolved: &ResolvedProvider, request: &AiRequest) -> BuiltRequest {
    let url = format!(
        "{}/messages",
        resolved.config.base_url.trim_end_matches('/')
    );

    // Anthropic: system messages go in a separate "system" field.
    // Multiple system messages are concatenated with newlines.
    let mut system_parts: Vec<&str> = Vec::new();
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter_map(|m| {
            if m.role == "system" {
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
    let system_content = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };

    let mut body = serde_json::json!({
        "model": resolved.model,
        "messages": messages,
        "max_tokens": request.options.max_tokens.unwrap_or(1024),
    });

    if let Some(ref system) = system_content {
        body["system"] = serde_json::json!(system);
    }
    if let Some(temperature) = request.options.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(top_p) = request.options.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(ref stop) = request.options.stop
        && !stop.is_empty()
    {
        body["stop_sequences"] = serde_json::json!(stop);
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
// Response parsing
// =============================================================================

/// Parse an OpenAI-compatible response into a normalized `AiResponse`.
fn parse_openai_response(body: &str, latency_ms: u64) -> Result<AiResponse, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Failed to parse response: {e}"))?;

    let content = json["choices"]
        .get(0)
        .and_then(|c| c["message"]["content"].as_str())
        .unwrap_or("")
        .to_string();

    let model = json["model"].as_str().unwrap_or("unknown").to_string();

    let finish_reason = json["choices"]
        .get(0)
        .and_then(|c| c["finish_reason"].as_str())
        .map(|s| s.to_string());

    let usage = AiUsage {
        prompt_tokens: json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
        total_tokens: json["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32,
    };

    Ok(AiResponse::new(
        content,
        model,
        usage,
        latency_ms,
        finish_reason,
    ))
}

/// Parse an OpenAI-compatible **embeddings** response into a normalized
/// `AiResponse` (K1 fix 2, G-AI-EMBED-UNROUTED).
///
/// `AiResponse` carries no vector field — adding one would break the frozen SDK
/// type — so the vector travels in `content` as a JSON float array, which is
/// exactly what a plugin already parses it as (`plugins/argus/src/host_ports.rs`,
/// `HostProvider::embed`, written against that assumption in M1 and unusable
/// until now).
///
/// An embeddings response reports `usage.prompt_tokens` / `usage.total_tokens`
/// and no completion tokens, so `completion_tokens` is zero rather than absent,
/// and `ai_usage_log` records the operation with the real prompt count instead
/// of a chat call's shape.
fn parse_openai_embedding_response(body: &str, latency_ms: u64) -> Result<AiResponse, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Failed to parse response: {e}"))?;

    let vector = crate::services::ai_provider::parse_embedding_vector(&json)
        .map_err(|e| format!("embedding response had no usable vector: {e}"))?;

    let content =
        serde_json::to_string(&vector).map_err(|e| format!("failed to serialize vector: {e}"))?;

    let model = json["model"].as_str().unwrap_or("unknown").to_string();

    let prompt_tokens = json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    let total_tokens = json["usage"]["total_tokens"]
        .as_u64()
        .unwrap_or(u64::from(prompt_tokens)) as u32;
    let usage = AiUsage {
        prompt_tokens,
        completion_tokens: 0,
        total_tokens,
    };

    Ok(AiResponse::new(content, model, usage, latency_ms, None))
}

/// Parse an Anthropic Messages API response into a normalized `AiResponse`.
fn parse_anthropic_response(body: &str, latency_ms: u64) -> Result<AiResponse, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Failed to parse response: {e}"))?;

    // Concatenate all text content blocks (Anthropic may return multiple).
    let content = json["content"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    let model = json["model"].as_str().unwrap_or("unknown").to_string();

    let finish_reason = json["stop_reason"].as_str().map(|s| s.to_string());

    let usage = AiUsage {
        prompt_tokens: json["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: json["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
        total_tokens: (json["usage"]["input_tokens"].as_u64().unwrap_or(0)
            + json["usage"]["output_tokens"].as_u64().unwrap_or(0)) as u32,
    };

    Ok(AiResponse::new(
        content,
        model,
        usage,
        latency_ms,
        finish_reason,
    ))
}

// =============================================================================
// Host function registration
// =============================================================================

/// Register AI API host functions with the WASM linker.
///
/// Provides the `ai-request` function under `trovato:kernel/ai-api`.
pub fn register_ai_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker
        .func_wrap_async(
            "trovato:kernel/ai-api",
            "ai-request",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (req_ptr, req_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    // Read request JSON from WASM memory
                    let Ok(request_json) =
                        read_string_from_memory(&memory, &caller, req_ptr, req_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    // Get services
                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let Some(ref ai_svc) = services.ai_providers else {
                        return host_errors::ERR_AI_NO_PROVIDER;
                    };
                    let ai_svc = ai_svc.clone();
                    let plugin_name = caller.data().plugin_name.clone();

                    // Deserialize request
                    let request: AiRequest = match serde_json::from_str(&request_json) {
                        Ok(r) => r,
                        Err(e) => {
                            warn!(
                                plugin = %plugin_name,
                                error = %e,
                                "invalid AiRequest JSON from plugin"
                            );
                            return host_errors::ERR_AI_INVALID_REQUEST;
                        }
                    };

                    // Validate message roles before processing
                    const VALID_ROLES: &[&str] = &["system", "user", "assistant"];
                    for msg in &request.messages {
                        if !VALID_ROLES.contains(&msg.role.as_str()) {
                            warn!(
                                plugin = %plugin_name,
                                role = %msg.role,
                                "invalid message role in AiRequest"
                            );
                            return host_errors::ERR_AI_INVALID_REQUEST;
                        }
                    }

                    // Permission / principal check — before rate limit and budget
                    // (P11c / D-40, D-41). Web/user calls keep the exact pre-P11c
                    // `use ai` gate; a background principal is authorized only by
                    // the plugin's `ai_background` manifest capability.
                    {
                        let ai_background = caller.data().ai_background;
                        let user = &caller.data().request.user;
                        if let Err(code) =
                            authorize_ai_request(user, ai_background, &request.operation)
                        {
                            if code == host_errors::ERR_AI_BACKGROUND_DENIED {
                                warn!(
                                    plugin = %plugin_name,
                                    "AI request denied: background principal without 'ai_background' capability"
                                );
                            } else if !user.has_permission("use ai") && !user.authenticated {
                                warn!(
                                    plugin = %plugin_name,
                                    "AI request denied: anonymous user (authentication required)"
                                );
                            } else if !user.has_permission("use ai") {
                                warn!(
                                    plugin = %plugin_name,
                                    user_id = %user.id,
                                    "AI request denied: user lacks 'use ai' permission"
                                );
                            } else {
                                warn!(
                                    plugin = %plugin_name,
                                    user_id = %user.id,
                                    "AI request denied: user lacks operation permission"
                                );
                            }
                            return code;
                        }
                    }

                    // Convert SDK operation type to kernel operation type via serde
                    let op_json = serde_json::to_string(&request.operation).unwrap_or_default();
                    let kernel_op: crate::services::ai_provider::AiOperationType =
                        match serde_json::from_str(&op_json) {
                            Ok(op) => op,
                            Err(_) => return host_errors::ERR_AI_INVALID_REQUEST,
                        };

                    // Resolve provider
                    let resolved = match ai_svc
                        .resolve_provider(kernel_op, request.provider_id.as_deref())
                        .await
                    {
                        Ok(Some(r)) => r,
                        Ok(None) => return host_errors::ERR_AI_NO_PROVIDER,
                        Err(e) => {
                            warn!(
                                plugin = %plugin_name,
                                error = %e,
                                "failed to resolve AI provider"
                            );
                            return host_errors::ERR_AI_NO_PROVIDER;
                        }
                    };

                    // Apply model override if specified in the request
                    let mut resolved = resolved;
                    if let Some(ref model_override) = request.model {
                        resolved.model = model_override.clone();
                    }

                    // Check rate limit
                    if !check_rate_limit(&resolved.config.id, resolved.config.rate_limit_rpm) {
                        warn!(
                            plugin = %plugin_name,
                            provider = %resolved.config.label,
                            "AI rate limit exceeded"
                        );
                        return host_errors::ERR_AI_RATE_LIMITED;
                    }

                    // Check token budget. A background principal (P11c / D-42) is
                    // metered against its per-plugin background cap; a web/user
                    // call keeps the per-user/role budget path unchanged.
                    let user_id = caller.data().request.user.id;
                    let is_background = caller.data().request.user.is_background();
                    if let Some(ref budget_svc) = services.ai_budgets {
                        let budget_result = if is_background {
                            budget_svc
                                .check_plugin_budget(&services.db, &plugin_name, &resolved.config.id)
                                .await
                        } else {
                            budget_svc
                                .check_budget(&services.db, user_id, &resolved.config.id)
                                .await
                        };
                        match budget_result {
                            Ok(result) if !result.allowed => match result.action {
                                BudgetAction::Deny | BudgetAction::Queue => {
                                    warn!(
                                        plugin = %plugin_name,
                                        provider = %resolved.config.label,
                                        user = %user_id,
                                        used = result.used,
                                        limit = result.limit,
                                        "AI token budget exceeded"
                                    );
                                    return host_errors::ERR_AI_BUDGET_EXCEEDED;
                                }
                                BudgetAction::Warn => {
                                    warn!(
                                        plugin = %plugin_name,
                                        provider = %resolved.config.label,
                                        user = %user_id,
                                        used = result.used,
                                        limit = result.limit,
                                        "AI token budget exceeded (warn mode, allowing)"
                                    );
                                }
                            },
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    plugin = %plugin_name,
                                    "failed to check AI budget, allowing request"
                                );
                            }
                            _ => {}
                        }
                    }

                    // Additionally enforce a per-plugin currency budget for
                    // background calls (P11c / D-44), if one is configured.
                    if is_background
                        && let Some(ref budget_svc) = services.ai_budgets
                    {
                        match budget_svc
                            .check_plugin_cost_budget(
                                &services.db,
                                &plugin_name,
                                &resolved.config.id,
                            )
                            .await
                        {
                            Ok(result) if !result.allowed => match result.action {
                                BudgetAction::Deny | BudgetAction::Queue => {
                                    warn!(
                                        plugin = %plugin_name,
                                        provider = %resolved.config.label,
                                        used = result.used,
                                        limit = result.limit,
                                        "AI currency budget exceeded"
                                    );
                                    return host_errors::ERR_AI_BUDGET_EXCEEDED;
                                }
                                BudgetAction::Warn => {
                                    warn!(
                                        plugin = %plugin_name,
                                        provider = %resolved.config.label,
                                        used = result.used,
                                        limit = result.limit,
                                        "AI currency budget exceeded (warn mode, allowing)"
                                    );
                                }
                            },
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    plugin = %plugin_name,
                                    "failed to check AI currency budget, allowing request"
                                );
                            }
                            _ => {}
                        }
                    }

                    // Resolve the per-request outbound timeout and the
                    // per-operation breaker window from config (P11c / D-43).
                    // Degrade gracefully to defaults if the config read fails.
                    let timeout_cfg = ai_svc.get_timeout_config().await.unwrap_or_else(|e| {
                        warn!(
                            error = %e,
                            plugin = %plugin_name,
                            "failed to read AI timeout config, using defaults"
                        );
                        crate::services::ai_provider::AiTimeoutConfig::default()
                    });
                    let op_key = kernel_op.config_key();
                    let timeout =
                        timeout_cfg.resolve_timeout(op_key, &resolved.config.id, is_background);
                    let breaker =
                        ai_svc.breaker_for_operation(op_key, timeout_cfg.resolve_breaker_window(op_key));

                    let started = Instant::now();

                    // Execute the HTTP request through the per-operation circuit
                    // breaker. The epoch deadline cuts plugin CPU; the provider
                    // timeout cuts the HTTP call — both apply, the tighter wins.
                    let exec = breaker
                        .call(|| execute_ai_request(ai_svc.http(), &resolved, &request, timeout))
                        .await;
                    let (response_body, _status) = match exec {
                        Ok(r) => r,
                        Err(crate::circuit_breaker::CircuitBreakerError::Open) => {
                            warn!(
                                plugin = %plugin_name,
                                provider = %resolved.config.label,
                                operation = %op_key,
                                "AI request rejected: circuit breaker open for this operation"
                            );
                            return host_errors::ERR_AI_REQUEST_FAILED;
                        }
                        Err(crate::circuit_breaker::CircuitBreakerError::ServiceError((
                            code,
                            msg,
                        ))) => {
                            warn!(
                                plugin = %plugin_name,
                                provider = %resolved.config.label,
                                error = %msg,
                                "AI request failed"
                            );
                            return code;
                        }
                    };

                    let latency_ms = started.elapsed().as_millis() as u64;

                    // Parse the response with the parser matching the request
                    // that was actually built — by operation first, then
                    // protocol (K1 fix 2, G-AI-EMBED-UNROUTED). An embeddings
                    // response has no `choices[0].message.content`, so reading
                    // it as a chat completion is what used to hand a plugin an
                    // empty string where it expected a vector.
                    let ai_response = match request.operation {
                        trovato_sdk::types::AiOperationType::Embedding => {
                            parse_openai_embedding_response(&response_body, latency_ms)
                        }
                        _ => match resolved.config.protocol {
                            ProviderProtocol::OpenAiCompatible => {
                                parse_openai_response(&response_body, latency_ms)
                            }
                            ProviderProtocol::Anthropic => {
                                parse_anthropic_response(&response_body, latency_ms)
                            }
                        },
                    };

                    let mut ai_response = match ai_response {
                        Ok(r) => r,
                        Err(msg) => {
                            warn!(
                                plugin = %plugin_name,
                                error = %msg,
                                "failed to parse AI provider response"
                            );
                            return host_errors::ERR_AI_PROVIDER_ERROR;
                        }
                    };

                    // Log request details
                    info!(
                        plugin = %plugin_name,
                        operation = %kernel_op,
                        model = %ai_response.model,
                        prompt_tokens = ai_response.usage.prompt_tokens,
                        completion_tokens = ai_response.usage.completion_tokens,
                        latency_ms = latency_ms,
                        "ai_request completed"
                    );

                    // Record usage in ai_usage_log + surface cost to the plugin.
                    if let Some(ref budget_svc) = services.ai_budgets {
                        // Estimate cost (P11c / D-44) from the model + token
                        // counts; None for an unpriced model (tokens-only).
                        let cost_estimate = budget_svc
                            .estimate_cost(
                                &ai_response.model,
                                i64::from(ai_response.usage.prompt_tokens.min(i32::MAX as u32)),
                                i64::from(ai_response.usage.completion_tokens.min(i32::MAX as u32)),
                            )
                            .await;
                        // Surface the same figure to the plugin (G-COST-OPAQUE,
                        // p11j): the plugin now reads cost from the response rather
                        // than the kernel-owned ai_usage_log. `Option<f64>` is
                        // Copy, so it also feeds the log entry below.
                        ai_response.cost_estimate = cost_estimate;
                        let entry = UsageLogEntry {
                            user_id: if user_id.is_nil() {
                                None
                            } else {
                                Some(user_id)
                            },
                            plugin_name: plugin_name.clone(),
                            provider_id: resolved.config.id.clone(),
                            operation: kernel_op.to_string(),
                            model: ai_response.model.clone(),
                            prompt_tokens: ai_response.usage.prompt_tokens.min(i32::MAX as u32)
                                as i32,
                            completion_tokens: ai_response
                                .usage
                                .completion_tokens
                                .min(i32::MAX as u32)
                                as i32,
                            total_tokens: ai_response.usage.total_tokens.min(i32::MAX as u32)
                                as i32,
                            latency_ms: latency_ms as i64,
                            cost_estimate,
                        };
                        if let Err(e) = budget_svc.record_usage(&services.db, entry).await {
                            warn!(
                                error = %e,
                                plugin = %plugin_name,
                                "failed to record AI usage"
                            );
                        }
                    }

                    // Serialize response and write to WASM memory
                    let Ok(response_json) = serde_json::to_string(&ai_response) else {
                        return host_errors::ERR_SERIALIZE_FAILED;
                    };

                    // Guard against silent truncation — the SDK would get partial
                    // JSON and fail with a confusing deserialization error.
                    if response_json.len() > out_max_len as usize {
                        warn!(
                            plugin = %plugin_name,
                            response_len = response_json.len(),
                            buffer_max = out_max_len,
                            "AI response exceeds output buffer"
                        );
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    }

                    write_string_to_memory(
                        &memory,
                        &mut caller,
                        out_ptr,
                        out_max_len,
                        &response_json,
                    )
                    .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                })
            },
        )
        .into_anyhow()?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use trovato_sdk::types::{AiMessage, AiOperationType, AiRequestOptions};

    // -------------------------------------------------------------------------
    // P11c / D-40, D-41 — background-AI principal authorization invariants.
    // These pin the authorization decision directly (no provider needed): the
    // host call site returns exactly `authorize_ai_request`'s verdict.
    // -------------------------------------------------------------------------

    // -------------------------------------------------------------------------
    // P11c / D-43 — per-request outbound timeout applied by execute_ai_request.
    // Drives the real HTTP path against a local slow "provider" (no wiremock
    // dependency): a generous timeout completes; a short timeout cuts the call.
    // -------------------------------------------------------------------------

    /// Bind a local one-shot HTTP server that waits `delay` before responding
    /// 200. Returns its `http://addr` base URL.
    async fn slow_http_server(delay: std::time::Duration) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                tokio::time::sleep(delay).await;
                let body = r#"{"model":"m","choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    fn resolved_for(base_url: String) -> ResolvedProvider {
        ResolvedProvider {
            config: crate::services::ai_provider::AiProviderConfig {
                id: "test-provider".to_string(),
                label: "test".to_string(),
                protocol: ProviderProtocol::OpenAiCompatible,
                base_url,
                api_key_env: "TROVATO_TEST_UNSET_KEY".to_string(),
                models: vec![],
                rate_limit_rpm: 0,
                enabled: true,
            },
            api_key: None,
            model: "m".to_string(),
        }
    }

    fn chat_request() -> AiRequest {
        AiRequest {
            operation: AiOperationType::Chat,
            provider_id: None,
            model: None,
            messages: vec![AiMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            input: None,
            options: AiRequestOptions::default(),
        }
    }

    // -------------------------------------------------------------------------
    // K1 fix 2 / G-AI-EMBED-UNROUTED — an embedding request reaches an
    // embeddings endpoint and comes back as a vector.
    // -------------------------------------------------------------------------

    fn embedding_request(input: &str) -> AiRequest {
        AiRequest {
            operation: AiOperationType::Embedding,
            provider_id: None,
            model: None,
            messages: Vec::new(),
            input: Some(input.to_string()),
            options: AiRequestOptions::default(),
        }
    }

    /// A one-shot server that records the request line and body it received,
    /// then answers with an OpenAI-shaped embeddings response.
    async fn embedding_http_server() -> (String, Arc<Mutex<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let seen = Arc::new(Mutex::new(String::new()));
        let recorder = Arc::clone(&seen);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                *recorder.lock().unwrap_or_else(|e| e.into_inner()) =
                    String::from_utf8_lossy(&buf[..n]).to_string();
                let body = r#"{"object":"list","model":"text-embedding-3-small","data":[{"index":0,"embedding":[0.25,-0.5,0.75]}],"usage":{"prompt_tokens":7,"total_tokens":7}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn an_embedding_request_hits_the_embeddings_endpoint_with_its_input() {
        let (base, seen) = embedding_http_server().await;
        let resolved = resolved_for(base);
        let client = reqwest::Client::new();

        let (body, status) = execute_ai_request(
            &client,
            &resolved,
            &embedding_request("embed me"),
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("embedding request should be routed");
        assert_eq!(status, 200);

        let request = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // The defect, gone: the path is /embeddings, not /chat/completions...
        assert!(
            request.starts_with("POST /embeddings "),
            "expected the embeddings endpoint, got request:\n{request}"
        );
        assert!(
            !request.contains("chat/completions"),
            "still posting to chat completions:\n{request}"
        );
        // ...and `input` is on the wire, which the chat builder never read.
        assert!(
            request.contains(r#""input":"embed me""#),
            "request.input never reached the wire:\n{request}"
        );
        assert!(
            !request.contains(r#""messages""#),
            "an embeddings request must not carry a messages array:\n{request}"
        );

        // And it parses as a vector, which is what a plugin does with `content`.
        let parsed = parse_openai_embedding_response(&body, 12).expect("parse embedding response");
        let vector: Vec<f32> = serde_json::from_str(&parsed.content).expect("content is a vector");
        assert_eq!(vector, vec![0.25, -0.5, 0.75]);
        assert_eq!(parsed.model, "text-embedding-3-small");
        assert_eq!(parsed.usage.prompt_tokens, 7);
        assert_eq!(parsed.usage.completion_tokens, 0);
        assert_eq!(parsed.usage.total_tokens, 7);
    }

    #[test]
    fn an_embeddings_response_read_as_a_chat_completion_yields_nothing() {
        // Why the parser had to branch too: this is what a plugin used to get.
        let body = r#"{"model":"e","data":[{"embedding":[1.0,2.0]}],"usage":{"prompt_tokens":3,"total_tokens":3}}"#;
        let as_chat = parse_openai_response(body, 0).expect("chat parse is lenient");
        assert!(as_chat.content.is_empty(), "an empty string, not a vector");
        assert!(serde_json::from_str::<Vec<f32>>(&as_chat.content).is_err());
    }

    #[test]
    fn an_embedding_with_no_input_is_an_invalid_request() {
        let mut req = embedding_request("");
        req.input = None;
        let err = build_openai_embedding_request(&resolved_for("http://x.test".into()), &req)
            .expect_err("no input is not a request");
        assert_eq!(err.0, host_errors::ERR_AI_INVALID_REQUEST);

        let err = build_openai_embedding_request(
            &resolved_for("http://x.test".into()),
            &embedding_request("   "),
        )
        .expect_err("whitespace is not input either");
        assert_eq!(err.0, host_errors::ERR_AI_INVALID_REQUEST);
    }

    #[test]
    fn an_embedding_on_anthropic_is_refused_rather_than_mis_posted() {
        let mut resolved = resolved_for("http://x.test".into());
        resolved.config.protocol = ProviderProtocol::Anthropic;
        let err = build_openai_embedding_request(&resolved, &embedding_request("hi"))
            .expect_err("Anthropic has no embeddings API");
        assert_eq!(err.0, host_errors::ERR_AI_OPERATION_UNSUPPORTED);
    }

    #[tokio::test]
    async fn an_unrouted_operation_is_refused_rather_than_served_as_chat() {
        // The bad kind of quiet, closed: Moderation used to be posted to
        // /chat/completions and the caller got a plausible-looking response.
        let client = reqwest::Client::new();
        for op in [
            AiOperationType::ImageGeneration,
            AiOperationType::SpeechToText,
            AiOperationType::TextToSpeech,
            AiOperationType::Moderation,
        ] {
            let mut req = chat_request();
            req.operation = op;
            let err = execute_ai_request(
                &client,
                &resolved_for("http://127.0.0.1:1".into()),
                &req,
                std::time::Duration::from_millis(200),
            )
            .await
            .expect_err("{op:?} has no route");
            assert_eq!(
                err.0,
                host_errors::ERR_AI_OPERATION_UNSUPPORTED,
                "{op:?} must be refused with a distinct code, got {err:?}"
            );
        }
    }

    #[test]
    fn a_long_embedding_input_is_capped_before_it_leaves() {
        let long = "x".repeat(EMBEDDING_INPUT_MAX_CHARS + 1_000);
        let (_url, body, _headers) = build_openai_embedding_request(
            &resolved_for("http://x.test".into()),
            &embedding_request(&long),
        )
        .expect("over-budget input is capped, not refused");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["input"].as_str().unwrap().chars().count(),
            EMBEDDING_INPUT_MAX_CHARS
        );
    }

    #[tokio::test]
    async fn generous_timeout_completes_a_slow_call() {
        let base = slow_http_server(std::time::Duration::from_millis(200)).await;
        let resolved = resolved_for(base);
        let client = reqwest::Client::new();
        // A 5s timeout comfortably clears a 200ms-slow provider.
        let out = execute_ai_request(
            &client,
            &resolved,
            &chat_request(),
            std::time::Duration::from_secs(5),
        )
        .await;
        let (body, status) = out.expect("generous timeout should let the call complete");
        assert_eq!(status, 200);
        assert!(body.contains("ok"));
    }

    #[tokio::test]
    async fn short_timeout_cuts_a_slow_call() {
        let base = slow_http_server(std::time::Duration::from_secs(3)).await;
        let resolved = resolved_for(base);
        let client = reqwest::Client::new();
        // A 300ms timeout cuts a 3s-slow provider — the configured (short)
        // request-scoped timeout is honored per request.
        let out = execute_ai_request(
            &client,
            &resolved,
            &chat_request(),
            std::time::Duration::from_millis(300),
        )
        .await;
        let (code, _msg) = out.expect_err("short timeout must cut the slow call");
        assert_eq!(code, host_errors::ERR_AI_REQUEST_FAILED);
    }

    #[test]
    fn authz_anonymous_web_denied_via_permission_path() {
        // Invariant: an anonymous web caller (empty permission set, NOT a
        // background principal) is denied with the exact pre-P11c code, decided
        // by the `use ai` permission check — not the `authenticated` flag.
        let anon = UserContext::anonymous();
        assert!(!anon.is_background());
        assert_eq!(
            authorize_ai_request(&anon, false, &AiOperationType::Chat),
            Err(host_errors::ERR_AI_PERMISSION_DENIED)
        );
        // Even if a (nonsensical) capability flag were set, a non-background
        // context is still governed solely by the permission plane.
        assert_eq!(
            authorize_ai_request(&anon, true, &AiOperationType::Chat),
            Err(host_errors::ERR_AI_PERMISSION_DENIED)
        );
    }

    #[test]
    fn authz_authenticated_without_use_ai_denied() {
        // Invariant: an authenticated web user lacking `use ai` is still denied
        // (unchanged), via the permission-check path.
        let user = UserContext::authenticated(uuid::Uuid::now_v7(), vec!["edit".into()]);
        assert_eq!(
            authorize_ai_request(&user, false, &AiOperationType::Chat),
            Err(host_errors::ERR_AI_PERMISSION_DENIED)
        );
    }

    #[test]
    fn authz_web_user_with_use_ai_allowed_operation_perm_enforced() {
        // Base `use ai` allows an untyped operation; a typed operation still
        // requires its operation-specific permission (byte-for-byte prior gate).
        let base = UserContext::authenticated(uuid::Uuid::now_v7(), vec!["use ai".into()]);
        assert_eq!(
            authorize_ai_request(&base, false, &AiOperationType::Chat),
            Err(host_errors::ERR_AI_PERMISSION_DENIED),
            "Chat needs 'use ai chat' on top of 'use ai'"
        );
        let chat = UserContext::authenticated(
            uuid::Uuid::now_v7(),
            vec!["use ai".into(), "use ai chat".into()],
        );
        assert_eq!(
            authorize_ai_request(&chat, false, &AiOperationType::Chat),
            Ok(())
        );
    }

    #[test]
    fn authz_background_without_capability_denied() {
        // Invariant: a background principal whose plugin lacks `ai_background` is
        // denied with the distinct background-denied code — NOT the human
        // permission-denied code, so the two are separable in logs/tests.
        let bg = UserContext::background();
        assert!(bg.is_background());
        assert_eq!(
            authorize_ai_request(&bg, false, &AiOperationType::Chat),
            Err(host_errors::ERR_AI_BACKGROUND_DENIED)
        );
        // The background principal must not be authorized by the `use ai`
        // permission plane even if the operation is untyped.
        assert_eq!(
            authorize_ai_request(&bg, false, &AiOperationType::Embedding),
            Err(host_errors::ERR_AI_BACKGROUND_DENIED)
        );
    }

    #[test]
    fn authz_background_with_capability_allowed() {
        // Invariant: a background principal whose plugin holds `ai_background`
        // is authorized, without consulting the `use ai` permission plane and
        // regardless of operation type.
        let bg = UserContext::background();
        assert_eq!(
            authorize_ai_request(&bg, true, &AiOperationType::Chat),
            Ok(())
        );
        assert_eq!(
            authorize_ai_request(&bg, true, &AiOperationType::Embedding),
            Ok(())
        );
        assert_eq!(
            authorize_ai_request(&bg, true, &AiOperationType::ImageGeneration),
            Ok(())
        );
    }

    #[test]
    fn parse_openai_response_valid() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello world"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        }"#;

        let resp = parse_openai_response(json, 42).unwrap();
        assert_eq!(resp.content, "Hello world");
        assert_eq!(resp.model, "gpt-4o");
        assert_eq!(resp.usage.prompt_tokens, 10);
        assert_eq!(resp.usage.completion_tokens, 5);
        assert_eq!(resp.usage.total_tokens, 15);
        assert_eq!(resp.latency_ms, 42);
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn parse_anthropic_response_valid() {
        let json = r#"{
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [{"type": "text", "text": "Hello from Claude"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 8,
                "output_tokens": 4
            }
        }"#;

        let resp = parse_anthropic_response(json, 100).unwrap();
        assert_eq!(resp.content, "Hello from Claude");
        assert_eq!(resp.model, "claude-sonnet-4-20250514");
        assert_eq!(resp.usage.prompt_tokens, 8);
        assert_eq!(resp.usage.completion_tokens, 4);
        assert_eq!(resp.usage.total_tokens, 12);
        assert_eq!(resp.latency_ms, 100);
        assert_eq!(resp.finish_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn parse_openai_response_empty_choices() {
        let json = r#"{"choices": [], "model": "gpt-4o", "usage": {}}"#;
        let resp = parse_openai_response(json, 0).unwrap();
        assert_eq!(resp.content, "");
        assert_eq!(resp.finish_reason, None);
    }

    #[test]
    fn parse_anthropic_response_empty_content() {
        let json = r#"{"content": [], "model": "claude-3", "usage": {}}"#;
        let resp = parse_anthropic_response(json, 0).unwrap();
        assert_eq!(resp.content, "");
    }

    #[test]
    fn build_openai_request_format() {
        let resolved = ResolvedProvider {
            config: crate::services::ai_provider::AiProviderConfig {
                id: "test".to_string(),
                label: "Test".to_string(),
                protocol: ProviderProtocol::OpenAiCompatible,
                base_url: "https://api.openai.com/v1".to_string(),
                api_key_env: "OPENAI_API_KEY".to_string(),
                models: vec![],
                rate_limit_rpm: 60,
                enabled: true,
            },
            api_key: Some("sk-test-key".to_string()),
            model: "gpt-4o".to_string(),
        };

        let request = AiRequest {
            operation: trovato_sdk::types::AiOperationType::Chat,
            provider_id: None,
            model: None,
            messages: vec![
                AiMessage::system("You are helpful."),
                AiMessage::user("Hello"),
            ],
            input: None,
            options: AiRequestOptions {
                max_tokens: Some(100),
                temperature: Some(0.7),
                ..Default::default()
            },
        };

        let (url, body, headers) = build_openai_request(&resolved, &request);
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer sk-test-key")
        );

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["model"], "gpt-4o");
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["messages"][0]["role"], "system");
        assert_eq!(parsed["max_tokens"], 100);
        let temp = parsed["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 0.001, "temperature was {temp}");
    }

    #[test]
    fn build_anthropic_request_extracts_system() {
        let resolved = ResolvedProvider {
            config: crate::services::ai_provider::AiProviderConfig {
                id: "test".to_string(),
                label: "Test".to_string(),
                protocol: ProviderProtocol::Anthropic,
                base_url: "https://api.anthropic.com/v1".to_string(),
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                models: vec![],
                rate_limit_rpm: 60,
                enabled: true,
            },
            api_key: Some("sk-ant-test".to_string()),
            model: "claude-sonnet-4-20250514".to_string(),
        };

        let request = AiRequest {
            operation: trovato_sdk::types::AiOperationType::Chat,
            provider_id: None,
            model: None,
            messages: vec![
                AiMessage::system("You are a poet."),
                AiMessage::user("Write a haiku"),
            ],
            input: None,
            options: AiRequestOptions {
                max_tokens: Some(200),
                ..Default::default()
            },
        };

        let (url, body, headers) = build_anthropic_request(&resolved, &request);
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "x-api-key" && v == "sk-ant-test")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01")
        );

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["model"], "claude-sonnet-4-20250514");
        assert_eq!(parsed["system"], "You are a poet.");
        // System message should NOT be in the messages array
        let messages = parsed["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(parsed["max_tokens"], 200);
    }

    #[test]
    fn rate_limiter_allows_within_limit() {
        assert!(check_rate_limit("test-provider-1", 10));
        assert!(check_rate_limit("test-provider-1", 10));
    }

    #[test]
    fn rate_limiter_zero_means_unlimited() {
        assert!(check_rate_limit("test-provider-2", 0));
    }

    #[test]
    fn build_anthropic_concatenates_multiple_system_messages() {
        let resolved = ResolvedProvider {
            config: crate::services::ai_provider::AiProviderConfig {
                id: "test".to_string(),
                label: "Test".to_string(),
                protocol: ProviderProtocol::Anthropic,
                base_url: "https://api.anthropic.com/v1".to_string(),
                api_key_env: "KEY".to_string(),
                models: vec![],
                rate_limit_rpm: 60,
                enabled: true,
            },
            api_key: None,
            model: "claude-3".to_string(),
        };

        let request = AiRequest {
            operation: trovato_sdk::types::AiOperationType::Chat,
            provider_id: None,
            model: None,
            messages: vec![
                AiMessage::system("You are a poet."),
                AiMessage::system("Use haiku form only."),
                AiMessage::user("Write about rain"),
            ],
            input: None,
            options: AiRequestOptions::default(),
        };

        let (_url, body, _headers) = build_anthropic_request(&resolved, &request);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["system"], "You are a poet.\nUse haiku form only.");
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn parse_anthropic_response_concatenates_content_blocks() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "world!"}
            ],
            "model": "claude-3",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 2}
        }"#;

        let resp = parse_anthropic_response(json, 50).unwrap();
        assert_eq!(resp.content, "Hello world!");
    }

    #[test]
    fn rate_limiter_does_not_increment_on_rejection() {
        // Fill up the rate limit
        let provider = "test-provider-rejection";
        for _ in 0..3 {
            assert!(check_rate_limit(provider, 3));
        }
        // Should be rejected
        assert!(!check_rate_limit(provider, 3));
        assert!(!check_rate_limit(provider, 3));

        // Counter should still be at 3, not 5
        let map = RATE_LIMITS.lock().unwrap();
        let window = map.get(provider).unwrap();
        assert_eq!(window.count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn permission_for_operation_maps_correctly() {
        use trovato_sdk::types::AiOperationType;
        assert_eq!(
            permission_for_operation(&AiOperationType::Chat),
            "use ai chat"
        );
        assert_eq!(
            permission_for_operation(&AiOperationType::Embedding),
            "use ai embeddings"
        );
        assert_eq!(
            permission_for_operation(&AiOperationType::ImageGeneration),
            "use ai image generation"
        );
        // Other operations fall back to base permission
        assert_eq!(
            permission_for_operation(&AiOperationType::SpeechToText),
            "use ai"
        );
        assert_eq!(
            permission_for_operation(&AiOperationType::TextToSpeech),
            "use ai"
        );
        assert_eq!(
            permission_for_operation(&AiOperationType::Moderation),
            "use ai"
        );
    }

    #[test]
    fn operation_type_serde_compat_with_kernel() {
        // Verify SDK and kernel AiOperationType serialize identically
        let sdk_op = trovato_sdk::types::AiOperationType::Chat;
        let sdk_json = serde_json::to_string(&sdk_op).unwrap();

        let kernel_op: crate::services::ai_provider::AiOperationType =
            serde_json::from_str(&sdk_json).unwrap();
        assert_eq!(
            kernel_op,
            crate::services::ai_provider::AiOperationType::Chat
        );

        // And back
        let kernel_json = serde_json::to_string(&kernel_op).unwrap();
        let roundtrip: trovato_sdk::types::AiOperationType =
            serde_json::from_str(&kernel_json).unwrap();
        assert_eq!(roundtrip, trovato_sdk::types::AiOperationType::Chat);
    }
}
