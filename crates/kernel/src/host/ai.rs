//! AI API host function for WASM plugins.
//!
//! Provides `ai-request` under the `trovato:kernel/ai-api` WIT interface.
//! The kernel resolves the provider, injects the API key, makes the HTTP
//! request, and returns a normalized `AiResponse`. API keys never cross
//! the WASM boundary.

use anyhow::Result;
use tracing::{info, warn};
use wasmtime::Linker;

use super::trace::TracedLinker;

use crate::plugin::{PluginState, WasmtimeExt};
use crate::services::ai_provider::{
    AiCallError, AiCaller, AiProviderService, ChatRequest, EmbeddingResult, ResolvedProvider,
};
use crate::services::ai_tools::{ChatCompletion, ChatMessage};
use crate::tap::UserContext;
use trovato_sdk::host_errors;
use trovato_sdk::types::{AiRequest, AiResponse, AiUsage};

use super::{read_string_from_memory, write_string_to_memory};

// =============================================================================
// Rate limiter (per provider, shared through Redis)
// =============================================================================

/// The Redis rate limit category for a provider's requests per minute.
const AI_PROVIDER_RATE_CATEGORY: &str = "ai_provider";

/// Count one request against a provider's requests-per-minute limit.
///
/// Returns `true` if the request is allowed. The count lives in Redis under
/// `rate:ai_provider:{provider_id}`, so every server shares one budget per
/// provider. A limit of 0 means unlimited. With no limiter (a serviceless or
/// test context) or no Redis, the request is allowed, as the limiter fails
/// open everywhere else.
async fn check_rate_limit(
    limiter: Option<&crate::middleware::RateLimiter>,
    provider_id: &str,
    rpm_limit: u32,
) -> bool {
    if rpm_limit == 0 {
        return true;
    }
    let Some(limiter) = limiter else {
        return true;
    };
    limiter
        .check_limit(
            AI_PROVIDER_RATE_CATEGORY,
            provider_id,
            rpm_limit,
            std::time::Duration::from_secs(60),
        )
        .await
        .is_ok()
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
// The send, through the kernel's one send path
// =============================================================================

/// The send path's request for a plugin's chat `AiRequest`.
///
/// Message roles are already validated to `system|user|assistant`.
fn chat_request_for(request: &AiRequest) -> ChatRequest {
    ChatRequest {
        messages: request
            .messages
            .iter()
            .map(|m| match m.role.as_str() {
                "system" => ChatMessage::System(m.content.clone()),
                "assistant" => ChatMessage::Assistant {
                    text: Some(m.content.clone()),
                    tool_calls: Vec::new(),
                },
                _ => ChatMessage::User(m.content.clone()),
            })
            .collect(),
        max_tokens: request.options.max_tokens,
        temperature: request.options.temperature,
        top_p: request.options.top_p,
        stop: request.options.stop.clone().unwrap_or_default(),
        ..ChatRequest::default()
    }
}

/// The host error code a plugin gets for a failed send. The detail is in the
/// log; the plugin, like a person, gets no provider text.
fn host_error_code(error: &AiCallError) -> i32 {
    match error {
        AiCallError::Unauthorized => host_errors::ERR_AI_AUTH_FAILED,
        AiCallError::RateLimited => host_errors::ERR_AI_RATE_LIMITED,
        AiCallError::Provider { .. } | AiCallError::Malformed(_) => {
            host_errors::ERR_AI_PROVIDER_ERROR
        }
        AiCallError::Unsupported(_) => host_errors::ERR_AI_OPERATION_UNSUPPORTED,
        AiCallError::BudgetExceeded => host_errors::ERR_AI_BUDGET_EXCEEDED,
        AiCallError::BaseUrlRefused
        | AiCallError::Timeout
        | AiCallError::BreakerOpen
        | AiCallError::Transport(_) => host_errors::ERR_AI_REQUEST_FAILED,
    }
}

/// A chat completion in the shape a plugin reads.
fn chat_response(completion: ChatCompletion) -> AiResponse {
    let (prompt_tokens, completion_tokens, total_tokens) = completion.usage;
    let cost_estimate = completion.cost_estimate;
    let mut response = AiResponse::new(
        completion.text.unwrap_or_default(),
        completion.model,
        AiUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        },
        completion.latency_ms,
        completion.finish_reason,
    );
    // The figure the send path recorded, surfaced to the plugin (G-COST-OPAQUE,
    // p11j), so a plugin reads its cost here rather than from the kernel-owned
    // `ai_usage_log`.
    response.cost_estimate = cost_estimate;
    response
}

/// An embedding in the shape a plugin reads (K1 fix 2, G-AI-EMBED-UNROUTED).
///
/// `AiResponse` carries no vector field — adding one would break the frozen SDK
/// type — so the vector travels in `content` as a JSON float array, which is
/// exactly what a plugin already parses it as (Argus's `HostProvider::embed`,
/// `plugins/argus/src/host_ports.rs` in the jeremyandrews/argus repository).
/// An embeddings response reports no completion tokens, so
/// `completion_tokens` is zero rather than absent.
fn embedding_response(result: EmbeddingResult) -> AiResponse {
    // Infallible: a Vec<f32> of finite floats always serializes.
    let content = serde_json::to_string(&result.vector).unwrap_or_default();
    let cost_estimate = result.cost_estimate;
    let mut response = AiResponse::new(
        content,
        result.served_model,
        AiUsage {
            prompt_tokens: result.prompt_tokens,
            completion_tokens: 0,
            total_tokens: result.total_tokens,
        },
        result.latency_ms,
        None,
    );
    response.cost_estimate = cost_estimate;
    response
}

/// Send a plugin's request through the kernel's one send path
/// ([`AiProviderService::chat_once`] or [`AiProviderService::embed_with`]),
/// which checks the base URL, applies the configured timeout and the
/// per-operation breaker, and logs a provider's own error text.
///
/// Branches on the *operation* first: before K1 fix 2 an `Embedding` request
/// was posted to `/chat/completions` with an empty `messages` array and
/// `request.input` was never read at all (G-AI-EMBED-UNROUTED, Argus M2).
async fn send_plugin_request(
    ai_svc: &AiProviderService,
    resolved: &ResolvedProvider,
    request: &AiRequest,
    caller: &AiCaller,
) -> Result<AiResponse, i32> {
    use trovato_sdk::types::AiOperationType;
    let sent = match request.operation {
        AiOperationType::Chat => ai_svc
            .chat_once(resolved, caller, &chat_request_for(request))
            .await
            .map(chat_response),
        AiOperationType::Embedding => {
            let input = request.input.as_deref().unwrap_or("");
            if input.trim().is_empty() {
                return Err(host_errors::ERR_AI_INVALID_REQUEST);
            }
            ai_svc
                .embed_with(resolved, input, caller)
                .await
                .map(embedding_response)
        }
        // The kernel serves no route for any other operation; refusing it is
        // better than posting it to `/chat/completions` and handing back a
        // plausible-looking answer.
        _ => return Err(host_errors::ERR_AI_OPERATION_UNSUPPORTED),
    };
    sent.map_err(|e| {
        warn!(
            plugin = %caller.source,
            provider = %resolved.config.label,
            error = %e,
            "AI request failed"
        );
        host_error_code(&e)
    })
}

// =============================================================================
// Host function registration
// =============================================================================

/// Register AI API host functions with the WASM linker.
///
/// Provides the `ai-request` function under `trovato:kernel/ai-api`.
pub fn register_ai_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker
        .func_wrap_async_traced(
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
                    if !check_rate_limit(
                        services.rate_limiter.as_deref(),
                        &resolved.config.id,
                        resolved.config.rate_limit_rpm,
                    )
                    .await
                    {
                        warn!(
                            plugin = %plugin_name,
                            provider = %resolved.config.label,
                            "AI rate limit exceeded"
                        );
                        return host_errors::ERR_AI_RATE_LIMITED;
                    }

                    let user_id = caller.data().request.user.id;
                    let is_background = caller.data().request.user.is_background();

                    // Send through the kernel's one send path: base URL
                    // check, the caller's budget (a background principal's
                    // per-plugin token and currency caps, P11c / D-42, D-44; a
                    // web call's per-user or anonymous cap), configured
                    // timeout, per-operation breaker, and the usage row with
                    // its cost. The epoch deadline cuts plugin CPU; the
                    // provider timeout cuts the HTTP call — both apply, the
                    // tighter wins.
                    let ai_caller = if is_background {
                        AiCaller::plugin_background(plugin_name.clone())
                    } else {
                        AiCaller::user(user_id, plugin_name.clone())
                    };
                    let ai_response =
                        match send_plugin_request(&ai_svc, &resolved, &request, &ai_caller).await {
                            Ok(response) => response,
                            Err(code) => return code,
                        };
                    let latency_ms = ai_response.latency_ms;

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
    use crate::services::ai_provider::ProviderProtocol;
    use std::sync::Arc;
    use std::sync::Mutex;
    use trovato_sdk::types::{AiMessage, AiOperationType, AiRequestOptions};

    // -------------------------------------------------------------------------
    // P11c / D-40, D-41 — background-AI principal authorization invariants.
    // These pin the authorization decision directly (no provider needed): the
    // host call site returns exactly `authorize_ai_request`'s verdict.
    // -------------------------------------------------------------------------

    // -------------------------------------------------------------------------
    // P11c / D-43 — the per-request outbound timeout, applied by the send path.
    // Drives the real HTTP path against a local slow "provider" (no wiremock
    // dependency): a generous timeout completes; a short timeout cuts the call.
    // -------------------------------------------------------------------------

    /// Bind a local one-shot HTTP server that waits `delay` before responding
    /// 200. Returns its `http://addr` base URL, allowed past the send path's
    /// loopback refusal.
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
        let base = format!("http://{addr}");
        crate::services::ai_provider::allow_loopback_provider_for_tests(&base);
        base
    }

    /// A provider service whose database is unreachable, so the `ai_timeouts`
    /// read falls back to defaults quickly.
    fn provider_service() -> AiProviderService {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_millis(50))
            .connect_lazy("postgres://trovato:unused@127.0.0.1:1/unused")
            .expect("a lazy pool never connects here");
        AiProviderService::new(pool)
    }

    fn caller() -> AiCaller {
        AiCaller::plugin_background("host_ai_test")
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
        let base = format!("http://{addr}");
        crate::services::ai_provider::allow_loopback_provider_for_tests(&base);
        (base, seen)
    }

    #[tokio::test]
    async fn an_embedding_request_hits_the_embeddings_endpoint_with_its_input() {
        let (base, seen) = embedding_http_server().await;
        let resolved = resolved_for(base);

        let parsed = send_plugin_request(
            &provider_service(),
            &resolved,
            &embedding_request("embed me"),
            &caller(),
        )
        .await
        .expect("embedding request should be routed");

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

        // And it comes back as a vector, which is what a plugin does with `content`.
        let vector: Vec<f32> = serde_json::from_str(&parsed.content).expect("content is a vector");
        assert_eq!(vector, vec![0.25, -0.5, 0.75]);
        assert_eq!(parsed.model, "text-embedding-3-small");
        assert_eq!(parsed.usage.prompt_tokens, 7);
        assert_eq!(parsed.usage.completion_tokens, 0);
        assert_eq!(parsed.usage.total_tokens, 7);
    }

    #[test]
    fn an_embeddings_response_read_as_a_chat_completion_yields_nothing() {
        // Why the send had to branch too: an embeddings response is not a
        // completion, and reading it as one is what a plugin used to get.
        let body = serde_json::json!({"model":"e","data":[{"embedding":[1.0,2.0]}],"usage":{"prompt_tokens":3,"total_tokens":3}});
        assert!(
            crate::services::ai_tools::parse_chat_response(
                ProviderProtocol::OpenAiCompatible,
                &body
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn an_embedding_with_no_input_is_an_invalid_request() {
        let service = provider_service();
        let resolved = resolved_for("http://x.test".into());
        let mut req = embedding_request("");
        req.input = None;
        let err = send_plugin_request(&service, &resolved, &req, &caller())
            .await
            .expect_err("no input is not a request");
        assert_eq!(err, host_errors::ERR_AI_INVALID_REQUEST);

        let err = send_plugin_request(&service, &resolved, &embedding_request("   "), &caller())
            .await
            .expect_err("whitespace is not input either");
        assert_eq!(err, host_errors::ERR_AI_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn an_embedding_on_anthropic_is_refused_rather_than_mis_posted() {
        let mut resolved = resolved_for("http://x.test".into());
        resolved.config.protocol = ProviderProtocol::Anthropic;
        let err = send_plugin_request(
            &provider_service(),
            &resolved,
            &embedding_request("hi"),
            &caller(),
        )
        .await
        .expect_err("Anthropic has no embeddings API");
        assert_eq!(err, host_errors::ERR_AI_OPERATION_UNSUPPORTED);
    }

    #[tokio::test]
    async fn an_unrouted_operation_is_refused_rather_than_served_as_chat() {
        // The bad kind of quiet, closed: Moderation used to be posted to
        // /chat/completions and the caller got a plausible-looking response.
        let service = provider_service();
        for op in [
            AiOperationType::ImageGeneration,
            AiOperationType::SpeechToText,
            AiOperationType::TextToSpeech,
            AiOperationType::Moderation,
        ] {
            let mut req = chat_request();
            req.operation = op;
            let err = send_plugin_request(
                &service,
                &resolved_for("https://provider.example".into()),
                &req,
                &caller(),
            )
            .await
            .expect_err("{op:?} has no route");
            assert_eq!(
                err,
                host_errors::ERR_AI_OPERATION_UNSUPPORTED,
                "{op:?} must be refused with a distinct code, got {err:?}"
            );
        }
    }

    /// **B50.** A private IP literal is refused at send time, before any byte
    /// leaves: the row may have reached `site_config` without the admin form.
    #[tokio::test]
    async fn a_private_ip_literal_is_refused_before_sending() {
        for base in [
            "http://10.0.0.1/v1",
            "http://192.168.1.1",
            "http://127.0.0.1:1",
        ] {
            let err = send_plugin_request(
                &provider_service(),
                &resolved_for(base.into()),
                &chat_request(),
                &caller(),
            )
            .await
            .expect_err("a private literal is refused");
            assert_eq!(err, host_errors::ERR_AI_REQUEST_FAILED, "{base}");
        }
    }

    async fn chat_with_timeout(
        base: String,
        timeout: std::time::Duration,
    ) -> Result<AiResponse, i32> {
        let request = ChatRequest {
            timeout: Some(timeout),
            ..chat_request_for(&chat_request())
        };
        provider_service()
            .chat_once(&resolved_for(base), &caller(), &request)
            .await
            .map(chat_response)
            .map_err(|e| host_error_code(&e))
    }

    #[tokio::test]
    async fn generous_timeout_completes_a_slow_call() {
        let base = slow_http_server(std::time::Duration::from_millis(200)).await;
        // A 5s timeout comfortably clears a 200ms-slow provider.
        let out = chat_with_timeout(base, std::time::Duration::from_secs(5)).await;
        let response = out.expect("generous timeout should let the call complete");
        assert_eq!(response.content, "ok");
    }

    #[tokio::test]
    async fn short_timeout_cuts_a_slow_call() {
        let base = slow_http_server(std::time::Duration::from_secs(3)).await;
        // A 300ms timeout cuts a 3s-slow provider — the configured (short)
        // request-scoped timeout is honored per request.
        let out = chat_with_timeout(base, std::time::Duration::from_millis(300)).await;
        let code = out.expect_err("short timeout must cut the slow call");
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

    /// Parse a provider response the way the send path does and hand it back
    /// in the shape a plugin reads.
    fn parse(protocol: ProviderProtocol, json: &str, latency_ms: u64) -> AiResponse {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut completion =
            crate::services::ai_tools::parse_chat_response(protocol, &value).unwrap();
        completion.latency_ms = latency_ms;
        chat_response(completion)
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

        let resp = parse(ProviderProtocol::OpenAiCompatible, json, 42);
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

        let resp = parse(ProviderProtocol::Anthropic, json, 100);
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
        // No choice is no completion: the plugin gets a provider error rather
        // than an empty string that reads as an answer.
        let json = serde_json::json!({"choices": [], "model": "gpt-4o", "usage": {}});
        assert!(
            crate::services::ai_tools::parse_chat_response(
                ProviderProtocol::OpenAiCompatible,
                &json
            )
            .is_none()
        );
    }

    #[test]
    fn parse_anthropic_response_empty_content() {
        let json = r#"{"content": [], "model": "claude-3", "usage": {}}"#;
        let resp = parse(ProviderProtocol::Anthropic, json, 0);
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

        let (url, parsed, headers) = crate::services::ai_tools::build_chat_request(
            &resolved,
            &chat_request_for(&request),
            false,
        );
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer sk-test-key")
        );

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

        let (url, parsed, headers) = crate::services::ai_tools::build_chat_request(
            &resolved,
            &chat_request_for(&request),
            false,
        );
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

        assert_eq!(parsed["model"], "claude-sonnet-4-20250514");
        assert_eq!(parsed["system"], "You are a poet.");
        // System message should NOT be in the messages array
        let messages = parsed["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(parsed["max_tokens"], 200);
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

        let (_url, parsed, _headers) = crate::services::ai_tools::build_chat_request(
            &resolved,
            &chat_request_for(&request),
            false,
        );
        assert_eq!(parsed["system"], "You are a poet.\n\nUse haiku form only.");
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

        let resp = parse(ProviderProtocol::Anthropic, json, 50);
        assert_eq!(resp.content, "Hello world!");
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
