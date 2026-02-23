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

use crate::plugin::PluginState;
use crate::services::ai_provider::{ProviderProtocol, ResolvedProvider};
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
// HTTP request building
// =============================================================================

/// Build and execute an HTTP request based on the provider protocol.
async fn execute_ai_request(
    http: &reqwest::Client,
    resolved: &ResolvedProvider,
    request: &AiRequest,
) -> Result<(String, u16), (i32, String)> {
    let (url, body, headers) = match resolved.config.protocol {
        ProviderProtocol::OpenAiCompatible => build_openai_request(resolved, request),
        ProviderProtocol::Anthropic => build_anthropic_request(resolved, request),
    };

    let mut req = http.post(&url);
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
fn build_openai_request(
    resolved: &ResolvedProvider,
    request: &AiRequest,
) -> (String, String, Vec<(String, String)>) {
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

/// Build an Anthropic Messages API request.
fn build_anthropic_request(
    resolved: &ResolvedProvider,
    request: &AiRequest,
) -> (String, String, Vec<(String, String)>) {
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

    Ok(AiResponse {
        content,
        model,
        usage,
        latency_ms,
        finish_reason,
    })
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

    Ok(AiResponse {
        content,
        model,
        usage,
        latency_ms,
        finish_reason,
    })
}

// =============================================================================
// Host function registration
// =============================================================================

/// Register AI API host functions with the WASM linker.
///
/// Provides the `ai-request` function under `trovato:kernel/ai-api`.
pub fn register_ai_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker.func_wrap_async(
        "trovato:kernel/ai-api",
        "ai-request",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (req_ptr, req_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return host_errors::ERR_MEMORY_MISSING;
                };

                // Read request JSON from WASM memory
                let Ok(request_json) = read_string_from_memory(&memory, &caller, req_ptr, req_len)
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

                let started = Instant::now();

                // Execute HTTP request
                let (response_body, _status) =
                    match execute_ai_request(ai_svc.http(), &resolved, &request).await {
                        Ok(r) => r,
                        Err((code, msg)) => {
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

                // Parse response based on protocol
                let ai_response = match resolved.config.protocol {
                    ProviderProtocol::OpenAiCompatible => {
                        parse_openai_response(&response_body, latency_ms)
                    }
                    ProviderProtocol::Anthropic => {
                        parse_anthropic_response(&response_body, latency_ms)
                    }
                };

                let ai_response = match ai_response {
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

                write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &response_json)
                    .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
            })
        },
    )?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use trovato_sdk::types::{AiMessage, AiRequestOptions};

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
