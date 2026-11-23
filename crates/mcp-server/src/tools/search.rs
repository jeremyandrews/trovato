//! Full-text search tool.
//!
//! Wraps the kernel's [`SearchService`](trovato_kernel::search::SearchService) for MCP access.

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::LIVE_STAGE_ID;
use trovato_kernel::pagination::Page;
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::server::SearchParams;
use crate::tools::{internal_err, require_mcp_permission, to_json};

/// Maximum allowed search query length: the kernel's, which the HTTP search
/// enforces too.
use trovato_kernel::search::MAX_QUERY_LENGTH;

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

    let window = crate::tools::window(params.page, params.per_page, params.limit, params.offset);

    // Unrecorded: search analytics counts what visitors type into the site's
    // search, and a tool call from an MCP client is not that.
    let mut results = state
        .search()
        .search_unrecorded(
            query,
            &[LIVE_STAGE_ID],
            Some(user_ctx.id),
            window.per_page,
            window.offset,
        )
        .await
        .map_err(internal_err)?;

    // FR-8 Story 3.7 — route result rows through the shared access seam: drop
    // items this viewer cannot see and redact restricted-field snippets.
    results.results = state
        .items()
        .filter_search_results(results.results, user_ctx)
        .await;

    let page = Page::new(
        results.results,
        u64::try_from(results.total).unwrap_or(0),
        window,
    );
    let json = to_json(&page)?;
    Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_query_length_is_1000() {
        assert_eq!(MAX_QUERY_LENGTH, 1000);
    }
}
