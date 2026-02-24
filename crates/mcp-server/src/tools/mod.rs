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
