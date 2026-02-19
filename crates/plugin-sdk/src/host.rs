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
extern "C" {
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
    let params_json = serde_json::to_string(params).map_err(|_| -100)?;
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
    let params_json = serde_json::to_string(params).map_err(|_| -100)?;
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
        String::from_utf8(buf).map_err(|_| -101)
    }
}

// --------------------------------------------------------------------------
// Native stubs for testing — no actual DB access
// --------------------------------------------------------------------------

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

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
}
