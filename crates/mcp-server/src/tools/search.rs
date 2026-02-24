//! Full-text search tool.
//!
//! Wraps the kernel's [`SearchService`](trovato_kernel::search::SearchService) for MCP access.

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::LIVE_STAGE_ID;
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::server::SearchParams;
use crate::tools::{internal_err, require_mcp_permission, to_json};

/// Maximum allowed search query length in characters.
const MAX_QUERY_LENGTH: usize = 1000;

/// Search published content.
pub async fn search(
    state: &AppState,
    user_ctx: &UserContext,
    params: SearchParams,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;

    let query = params.query.trim();
    if query.is_empty() {
        return Err(McpError::invalid_params(
            "query cannot be empty".to_string(),
            None,
        ));
    }
    if query.len() > MAX_QUERY_LENGTH {
        return Err(McpError::invalid_params(
            format!("query exceeds maximum length of {MAX_QUERY_LENGTH} characters"),
            None,
        ));
    }

    let limit = i64::from(params.limit.unwrap_or(20).clamp(1, 100));
    let offset = i64::from(params.offset.unwrap_or(0));

    let results = state
        .search()
        .search(query, &[LIVE_STAGE_ID], Some(user_ctx.id), limit, offset)
        .await
        .map_err(internal_err)?;

    let json = to_json(&results)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}
