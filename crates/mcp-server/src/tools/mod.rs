//! MCP tool implementations for Trovato CMS.
//!
//! Each submodule implements one or more MCP tools that are registered
//! via the `#[tool_router]` macro on [`crate::server::TrovatoMcpServer`].

pub mod categories;
pub mod content_types;
pub mod gather;
pub mod items;
pub mod search;

use rmcp::ErrorData as McpError;
use trovato_kernel::tap::UserContext;

/// Error substring used by `ItemService` for access denied.
///
/// **Coupling:** must match the `bail!("access denied")` string in
/// `crates/kernel/src/content/item_service.rs`. The kernel routes
/// (`routes/item.rs`) use the same pattern.
pub const ACCESS_DENIED_MSG: &str = "access denied";

/// Serialize a value to pretty-printed JSON, returning an MCP error on failure.
pub fn to_json(value: &impl serde::Serialize) -> Result<String, McpError> {
    serde_json::to_string_pretty(value).map_err(|e| {
        tracing::error!(error = %e, "JSON serialization failed");
        McpError::internal_error("serialization error".to_string(), None)
    })
}

/// Convert an anyhow error into an MCP internal error.
///
/// Logs the full error server-side but returns a generic message to the client
/// to avoid leaking implementation details (DB errors, file paths, etc.).
pub fn internal_err(e: anyhow::Error) -> McpError {
    tracing::error!(error = %e, "MCP tool error");
    McpError::internal_error("internal error".to_string(), None)
}

/// Build an MCP error for a permission denial.
pub fn permission_denied(perm: &str) -> McpError {
    McpError::invalid_request(format!("permission denied: {perm}"), None)
}

/// Check a permission against the pre-loaded [`UserContext`].
///
/// Uses the in-memory permission set loaded at session start, avoiding
/// a database round-trip on every tool invocation.
pub fn require_mcp_permission(user_ctx: &UserContext, permission: &str) -> Result<(), McpError> {
    if user_ctx.is_admin() || user_ctx.has_permission(permission) {
        Ok(())
    } else {
        Err(permission_denied(permission))
    }
}

/// Validate a machine-name parameter (query_id, category_id, etc.).
///
/// Re-exports the kernel's canonical validation to keep the rule in one place.
pub fn validate_machine_name(name: &str, param_name: &str) -> Result<(), McpError> {
    if !trovato_kernel::routes::helpers::is_valid_machine_name(name) {
        return Err(McpError::invalid_params(
            format!("invalid {param_name}: must be lowercase letters, digits, and underscores"),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;
    use trovato_kernel::tap::UserContext;
    use uuid::Uuid;

    // =========================================================================
    // to_json
    // =========================================================================

    #[test]
    fn to_json_serializes_simple_value() {
        let val = serde_json::json!({"name": "test", "count": 42});
        let result = to_json(&val).expect("should serialize");
        assert!(result.contains("\"name\": \"test\""));
        assert!(result.contains("\"count\": 42"));
    }

    #[test]
    fn to_json_produces_pretty_printed_output() {
        let val = serde_json::json!({"a": 1});
        let result = to_json(&val).expect("should serialize");
        // Pretty-printed JSON has newlines and indentation
        assert!(result.contains('\n'), "output should be pretty-printed");
    }

    #[test]
    fn to_json_serializes_empty_array() {
        let val: Vec<String> = vec![];
        let result = to_json(&val).expect("should serialize empty array");
        assert_eq!(result.trim(), "[]");
    }

    // =========================================================================
    // internal_err
    // =========================================================================

    #[test]
    fn internal_err_returns_internal_error_code() {
        let err = internal_err(anyhow::anyhow!("database connection lost"));
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
        // Should NOT leak the original error message
        assert_eq!(err.message.as_ref(), "internal error");
    }

    #[test]
    fn internal_err_does_not_leak_error_details() {
        let err = internal_err(anyhow::anyhow!("connection to 192.168.1.100:5432 refused"));
        assert!(
            !err.message.contains("192.168.1.100"),
            "should not leak connection details"
        );
    }

    // =========================================================================
    // permission_denied
    // =========================================================================

    #[test]
    fn permission_denied_returns_invalid_request_code() {
        let err = permission_denied("access content");
        assert_eq!(err.code, ErrorCode::INVALID_REQUEST);
        assert!(err.message.contains("permission denied"));
        assert!(err.message.contains("access content"));
    }

    #[test]
    fn permission_denied_includes_permission_name() {
        let err = permission_denied("create content");
        assert!(
            err.message.contains("create content"),
            "error should name the missing permission"
        );
    }

    // =========================================================================
    // require_mcp_permission
    // =========================================================================

    #[test]
    fn require_mcp_permission_allows_admin() {
        let admin = UserContext::authenticated(Uuid::new_v4(), vec!["administer site".to_string()]);
        assert!(require_mcp_permission(&admin, "access content").is_ok());
        assert!(require_mcp_permission(&admin, "create content").is_ok());
        assert!(require_mcp_permission(&admin, "any permission").is_ok());
    }

    #[test]
    fn require_mcp_permission_allows_user_with_permission() {
        let user = UserContext::authenticated(Uuid::new_v4(), vec!["access content".to_string()]);
        assert!(require_mcp_permission(&user, "access content").is_ok());
    }

    #[test]
    fn require_mcp_permission_denies_user_without_permission() {
        let user = UserContext::authenticated(Uuid::new_v4(), vec!["access content".to_string()]);
        let result = require_mcp_permission(&user, "create content");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_REQUEST);
        assert!(err.message.contains("permission denied"));
    }

    #[test]
    fn require_mcp_permission_denies_anonymous() {
        let anon = UserContext::anonymous();
        let result = require_mcp_permission(&anon, "access content");
        assert!(result.is_err());
    }

    #[test]
    fn require_mcp_permission_denies_user_with_empty_permissions() {
        let user = UserContext::authenticated(Uuid::new_v4(), vec![]);
        let result = require_mcp_permission(&user, "access content");
        assert!(result.is_err());
    }

    // =========================================================================
    // validate_machine_name
    // =========================================================================

    #[test]
    fn validate_machine_name_accepts_valid_names() {
        assert!(validate_machine_name("article", "type").is_ok());
        assert!(validate_machine_name("content_type", "type").is_ok());
        assert!(validate_machine_name("page123", "type").is_ok());
        assert!(validate_machine_name("a", "type").is_ok());
    }

    #[test]
    fn validate_machine_name_rejects_empty_string() {
        let result = validate_machine_name("", "type");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn validate_machine_name_rejects_uppercase() {
        assert!(validate_machine_name("Article", "type").is_err());
        assert!(validate_machine_name("UPPER", "type").is_err());
    }

    #[test]
    fn validate_machine_name_rejects_leading_digit() {
        assert!(validate_machine_name("123abc", "type").is_err());
    }

    #[test]
    fn validate_machine_name_rejects_leading_underscore() {
        assert!(validate_machine_name("_private", "type").is_err());
    }

    #[test]
    fn validate_machine_name_rejects_hyphens() {
        assert!(validate_machine_name("my-type", "type").is_err());
    }

    #[test]
    fn validate_machine_name_rejects_spaces() {
        assert!(validate_machine_name("my type", "type").is_err());
    }

    #[test]
    fn validate_machine_name_includes_param_name_in_error() {
        let result = validate_machine_name("BAD!", "category_id");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("category_id"),
            "error should reference the parameter name"
        );
    }

    // =========================================================================
    // ACCESS_DENIED_MSG constant
    // =========================================================================

    #[test]
    fn access_denied_msg_matches_expected_value() {
        assert_eq!(ACCESS_DENIED_MSG, "access denied");
    }
}
