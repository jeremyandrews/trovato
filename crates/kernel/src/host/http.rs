//! HTTP request host function for WASM plugins.
//!
//! Provides `request` under the `trovato:kernel/http` WIT interface.
//! The kernel executes outbound HTTP requests on behalf of plugins,
//! enforcing timeouts and URL restrictions. Plugins cannot make direct
//! network calls from WASM.
//!
//! # Security
//!
//! No per-user permission check is enforced here because this function
//! is infrastructure: cron tasks run as anonymous, and user-facing
//! actions should gate on permissions at the plugin/route level. The
//! kernel enforces SSRF protections (private IP blocking, scheme
//! validation) and resource limits (timeout, body size) instead.

use std::net::IpAddr;
use std::time::Duration;

use anyhow::Result;
use tracing::warn;
use url::Url;
use wasmtime::Linker;

use crate::plugin::{PluginState, WasmtimeExt};
use trovato_sdk::host_errors;

use super::{read_string_from_memory, write_string_to_memory};

/// Maximum allowed timeout for plugin HTTP requests (60 seconds).
const MAX_TIMEOUT_MS: u32 = 60_000;

/// Default timeout for plugin HTTP requests (30 seconds).
const DEFAULT_TIMEOUT_MS: u32 = 30_000;

/// Maximum response body size (1 MB).
const MAX_RESPONSE_BODY: usize = 1_024 * 1_024;

/// Register HTTP host functions with the WASM linker.
///
/// Provides the `request` function under `trovato:kernel/http`.
pub fn register_http_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker
        .func_wrap_async(
            "trovato:kernel/http",
            "request",
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

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let http = services.http.clone();
                    let plugin_name = caller.data().plugin_name.clone();

                    // Deserialize request
                    let request: trovato_sdk::types::HttpRequest =
                        match serde_json::from_str(&request_json) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(
                                    plugin = %plugin_name,
                                    error = %e,
                                    "invalid HttpRequest JSON from plugin"
                                );
                                return host_errors::ERR_PARAM_DESERIALIZE;
                            }
                        };

                    // Validate URL: scheme, host, and SSRF protections
                    if let Err(code) = validate_url(&request.url, &plugin_name) {
                        return code;
                    }

                    // Execute request
                    let response = match execute_http_request(&http, &request, &plugin_name).await {
                        Ok(r) => r,
                        Err(code) => return code,
                    };

                    // Serialize response
                    let Ok(response_json) = serde_json::to_string(&response) else {
                        return host_errors::ERR_SERIALIZE_FAILED;
                    };

                    // Guard against silent truncation
                    if response_json.len() > out_max_len as usize {
                        warn!(
                            plugin = %plugin_name,
                            response_len = response_json.len(),
                            buffer_max = out_max_len,
                            "HTTP response exceeds output buffer"
                        );
                        return host_errors::ERR_HTTP_RESPONSE_TOO_LARGE;
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

/// Validate a URL for safe outbound use (scheme + SSRF protection).
///
/// Blocks non-HTTP(S) schemes, private/loopback IP literals, and
/// hostnames commonly used for internal services. DNS-based rebinding
/// is not fully mitigated here (TOCTOU between resolve and connect);
/// a future improvement could use a custom `reqwest::dns::Resolve`.
fn validate_url(raw_url: &str, plugin_name: &str) -> std::result::Result<(), i32> {
    let parsed = Url::parse(raw_url).map_err(|_| {
        warn!(plugin = %plugin_name, url = %raw_url, "malformed URL");
        host_errors::ERR_HTTP_INVALID_URL
    })?;

    // Scheme check
    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            warn!(
                plugin = %plugin_name,
                url = %raw_url,
                "blocked HTTP request with non-HTTP scheme"
            );
            return Err(host_errors::ERR_HTTP_INVALID_URL);
        }
    }

    let Some(host) = parsed.host_str() else {
        warn!(plugin = %plugin_name, url = %raw_url, "URL has no host");
        return Err(host_errors::ERR_HTTP_INVALID_URL);
    };

    // Block private hostnames
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost"
        || host_lower.ends_with(".local")
        || host_lower.ends_with(".internal")
        || host_lower.ends_with(".localhost")
    {
        warn!(
            plugin = %plugin_name,
            url = %raw_url,
            "blocked HTTP request to private hostname"
        );
        return Err(host_errors::ERR_HTTP_INVALID_URL);
    }

    // Block private/loopback IP literals
    if let Ok(ip) = host.parse::<IpAddr>()
        && is_private_ip(ip)
    {
        warn!(
            plugin = %plugin_name,
            url = %raw_url,
            "blocked HTTP request to private/loopback IP"
        );
        return Err(host_errors::ERR_HTTP_INVALID_URL);
    }

    Ok(())
}

/// Check if an IP address is private, loopback, link-local, or otherwise
/// internal (RFC 1918, RFC 4193, cloud metadata endpoints, etc.).
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()             // 127.0.0.0/8
                || v4.is_private()       // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local()    // 169.254.0.0/16 (includes metadata)
                || v4.is_broadcast()     // 255.255.255.255
                || v4.is_unspecified()   // 0.0.0.0
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64/10 (CGNAT)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()             // ::1
                || v6.is_unspecified()   // ::
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
        }
    }
}

/// Execute an HTTP request with timeout and size restrictions.
async fn execute_http_request(
    http: &reqwest::Client,
    request: &trovato_sdk::types::HttpRequest,
    plugin_name: &str,
) -> std::result::Result<trovato_sdk::types::HttpResponse, i32> {
    let method: reqwest::Method = request.method.parse().map_err(|_| {
        warn!(
            plugin = %plugin_name,
            method = %request.method,
            "invalid HTTP method"
        );
        host_errors::ERR_PARAM_DESERIALIZE
    })?;

    let timeout_ms = request
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let mut req = http
        .request(method, &request.url)
        .timeout(Duration::from_millis(u64::from(timeout_ms)));

    // Add headers
    for (key, value) in &request.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    // Add body if present
    if let Some(ref body) = request.body {
        req = req.body(body.clone());
    }

    let response = req.send().await.map_err(|e| {
        if e.is_timeout() {
            warn!(
                plugin = %plugin_name,
                url = %request.url,
                "HTTP request timed out"
            );
            host_errors::ERR_HTTP_TIMEOUT
        } else {
            warn!(
                plugin = %plugin_name,
                url = %request.url,
                error = %e,
                "HTTP request failed"
            );
            host_errors::ERR_HTTP_REQUEST_FAILED
        }
    })?;

    // Pre-flight size check via Content-Length header (avoids buffering
    // an oversized body before rejecting it).
    if let Some(content_length) = response.content_length()
        && content_length > MAX_RESPONSE_BODY as u64
    {
        warn!(
            plugin = %plugin_name,
            url = %request.url,
            content_length = content_length,
            max = MAX_RESPONSE_BODY,
            "HTTP response Content-Length exceeds limit"
        );
        return Err(host_errors::ERR_HTTP_RESPONSE_TOO_LARGE);
    }

    let status = response.status().as_u16();
    let headers: std::collections::HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    // Read body with size limit (still needed: Content-Length may be absent
    // or the server may lie about it).
    let body = response.bytes().await.map_err(|e| {
        warn!(
            plugin = %plugin_name,
            url = %request.url,
            error = %e,
            "failed to read HTTP response body"
        );
        host_errors::ERR_HTTP_REQUEST_FAILED
    })?;

    if body.len() > MAX_RESPONSE_BODY {
        warn!(
            plugin = %plugin_name,
            url = %request.url,
            body_len = body.len(),
            max = MAX_RESPONSE_BODY,
            "HTTP response body too large"
        );
        return Err(host_errors::ERR_HTTP_RESPONSE_TOO_LARGE);
    }

    let body_str = String::from_utf8_lossy(&body).into_owned();

    Ok(trovato_sdk::types::HttpResponse {
        status,
        headers,
        body: body_str,
    })
}
