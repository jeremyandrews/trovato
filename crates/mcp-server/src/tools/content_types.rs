//! Content type schema tool.
//!
//! Lists all registered content types and their field definitions.

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::tools::{require_mcp_permission, to_json};

/// List all content types with field definitions.
pub async fn list_content_types(
    state: &AppState,
    user_ctx: &UserContext,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;

    let types = state.content_types().list();
    let json = to_json(&types)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}
