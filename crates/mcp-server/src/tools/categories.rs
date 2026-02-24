//! Category and tag tools.
//!
//! Exposes category vocabularies and their tags to MCP clients.

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::server::ListTagsParams;
use crate::tools::{internal_err, require_mcp_permission, to_json, validate_machine_name};

/// List all category vocabularies.
pub async fn list_categories(
    state: &AppState,
    user_ctx: &UserContext,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;

    let categories = state
        .categories()
        .list_categories()
        .await
        .map_err(internal_err)?;

    let json = to_json(&categories)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

/// List tags in a category.
pub async fn list_tags(
    state: &AppState,
    user_ctx: &UserContext,
    params: ListTagsParams,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;
    validate_machine_name(&params.category_id, "category_id")?;

    let tags = state
        .categories()
        .list_tags(&params.category_id)
        .await
        .map_err(internal_err)?;

    let json = to_json(&tags)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}
