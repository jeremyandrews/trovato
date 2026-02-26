//! Host function bindings for calling kernel services from WASM plugins.
//!
//! These functions are only usable when compiled for `wasm32` targets.
//! On native targets, stub implementations are provided for testing.

/// Maximum output buffer size for query results (256KB).
#[cfg(target_arch = "wasm32")]
const MAX_OUTPUT_BUFFER: usize = 256 * 1024;

// --------------------------------------------------------------------------
// WASM extern declarations — available only when compiling for wasm32
// --------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/db")]
unsafe extern "C" {
    #[link_name = "execute-raw"]
    fn __db_execute_raw(sql_ptr: i32, sql_len: i32, params_ptr: i32, params_len: i32) -> i64;

    #[link_name = "query-raw"]
    fn __db_query_raw(
        sql_ptr: i32,
        sql_len: i32,
        params_ptr: i32,
        params_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/ai-api")]
unsafe extern "C" {
    #[link_name = "ai-request"]
    fn __ai_request(req_ptr: i32, req_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/http")]
unsafe extern "C" {
    #[link_name = "request"]
    fn __http_request(req_ptr: i32, req_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/logging")]
unsafe extern "C" {
    #[link_name = "log"]
    fn __log(
        level_ptr: i32,
        level_len: i32,
        plugin_ptr: i32,
        plugin_len: i32,
        msg_ptr: i32,
        msg_len: i32,
    );
}

// --------------------------------------------------------------------------
// Ergonomic wrappers
// --------------------------------------------------------------------------

/// Execute a DML statement (INSERT, UPDATE, DELETE), return rows affected.
///
/// The kernel rejects DDL statements (CREATE, DROP, ALTER, TRUNCATE, GRANT, REVOKE).
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure.
#[cfg(target_arch = "wasm32")]
pub fn execute_raw(sql: &str, params: &[serde_json::Value]) -> Result<u64, i32> {
    let params_json =
        serde_json::to_string(params).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let result = unsafe {
        __db_execute_raw(
            sql.as_ptr() as i32,
            sql.len() as i32,
            params_json.as_ptr() as i32,
            params_json.len() as i32,
        )
    };
    if result < 0 {
        Err(result as i32)
    } else {
        Ok(result as u64)
    }
}

/// Execute a SELECT query, return JSON result string.
///
/// The kernel only allows SELECT and WITH statements.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure.
#[cfg(target_arch = "wasm32")]
pub fn query_raw(sql: &str, params: &[serde_json::Value]) -> Result<String, i32> {
    let params_json =
        serde_json::to_string(params).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __db_query_raw(
            sql.as_ptr() as i32,
            sql.len() as i32,
            params_json.as_ptr() as i32,
            params_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        buf.truncate(result as usize);
        String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)
    }
}

/// Make an outbound HTTP request through the kernel.
///
/// The kernel executes the request on the plugin's behalf, enforcing
/// timeouts and security restrictions. Plugins cannot make direct
/// network calls from WASM.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure. See
/// [`crate::host_errors`] for HTTP-specific error codes (`ERR_HTTP_*`).
#[cfg(target_arch = "wasm32")]
pub fn http_request(
    request: &crate::types::HttpRequest,
) -> Result<crate::types::HttpResponse, i32> {
    let request_json =
        serde_json::to_string(request).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __http_request(
            request_json.as_ptr() as i32,
            request_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        buf.truncate(result as usize);
        let json = String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)?;
        serde_json::from_str(&json).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
    }
}

/// Log a message through the kernel's tracing system.
///
/// Valid levels: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`.
#[cfg(target_arch = "wasm32")]
pub fn log(level: &str, plugin_name: &str, message: &str) {
    unsafe {
        __log(
            level.as_ptr() as i32,
            level.len() as i32,
            plugin_name.as_ptr() as i32,
            plugin_name.len() as i32,
            message.as_ptr() as i32,
            message.len() as i32,
        );
    }
}

/// Log a message (stub for native testing, prints to stderr).
#[cfg(not(target_arch = "wasm32"))]
pub fn log(level: &str, plugin_name: &str, message: &str) {
    eprintln!("[{level}] {plugin_name}: {message}");
}

/// Make an AI request through the kernel's provider registry.
///
/// The kernel resolves the provider, injects the API key, makes the HTTP
/// request, and returns a normalized response. API keys never cross the
/// WASM boundary.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure. See
/// [`crate::host_errors`] for AI-specific error codes.
#[cfg(target_arch = "wasm32")]
pub fn ai_request(request: &crate::types::AiRequest) -> Result<crate::types::AiResponse, i32> {
    let request_json =
        serde_json::to_string(request).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __ai_request(
            request_json.as_ptr() as i32,
            request_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        buf.truncate(result as usize);
        let json = String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)?;
        serde_json::from_str(&json).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
    }
}

// --------------------------------------------------------------------------
// Native stubs for testing — no actual DB access
// --------------------------------------------------------------------------

/// Make an outbound HTTP request (stub for native testing, returns mock 200).
#[cfg(not(target_arch = "wasm32"))]
pub fn http_request(
    _request: &crate::types::HttpRequest,
) -> Result<crate::types::HttpResponse, i32> {
    Ok(crate::types::HttpResponse {
        status: 200,
        headers: std::collections::HashMap::new(),
        body: "[]".to_string(),
    })
}

/// Execute a DML statement (stub for native testing, always returns 0).
#[cfg(not(target_arch = "wasm32"))]
pub fn execute_raw(_sql: &str, _params: &[serde_json::Value]) -> Result<u64, i32> {
    Ok(0)
}

/// Execute a SELECT query (stub for native testing, always returns empty array).
#[cfg(not(target_arch = "wasm32"))]
pub fn query_raw(_sql: &str, _params: &[serde_json::Value]) -> Result<String, i32> {
    Ok("[]".to_string())
}

/// Make an AI request (stub for native testing, returns a mock response).
#[cfg(not(target_arch = "wasm32"))]
pub fn ai_request(_request: &crate::types::AiRequest) -> Result<crate::types::AiResponse, i32> {
    Ok(crate::types::AiResponse {
        content: "Mock AI response".to_string(),
        model: "test-model".to_string(),
        usage: crate::types::AiUsage::default(),
        latency_ms: 0,
        finish_reason: Some("stop".to_string()),
    })
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn http_request_stub_returns_mock() {
        let request = crate::types::HttpRequest::get("https://example.com");
        let response = http_request(&request).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "[]");
    }

    #[test]
    fn execute_raw_stub_returns_zero() {
        let result = execute_raw("UPDATE item SET status = 1", &[]);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn query_raw_stub_returns_empty() {
        let result = query_raw("SELECT 1", &[]);
        assert_eq!(result.unwrap(), "[]");
    }

    #[test]
    fn execute_raw_with_params() {
        let params = vec![serde_json::json!(42), serde_json::json!("hello")];
        let result = execute_raw("UPDATE foo SET bar = $1 WHERE name = $2", &params);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn ai_request_stub_returns_mock() {
        use crate::types::{AiMessage, AiOperationType, AiRequest, AiRequestOptions};

        let request = AiRequest {
            operation: AiOperationType::Chat,
            provider_id: None,
            model: None,
            messages: vec![AiMessage::user("Hello")],
            input: None,
            options: AiRequestOptions::default(),
        };

        let response = ai_request(&request).unwrap();
        assert_eq!(response.content, "Mock AI response");
        assert_eq!(response.model, "test-model");
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    }
}
