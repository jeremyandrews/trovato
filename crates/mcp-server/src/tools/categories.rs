//! Category and tag tools.
//!
//! Exposes category vocabularies and their tags to MCP clients, by the same
//! read rule as the category API (B189): an owner's vocabulary only to a
//! caller who may see that owner.

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::content::owned_vocabulary;
use trovato_kernel::pagination::Page;
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::server::ListTagsParams;
use crate::tools::{internal_err, require_mcp_permission, to_json, validate_machine_name};

/// List the category vocabularies the caller may read.
pub async fn list_categories(
    state: &AppState,
    user_ctx: &UserContext,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;

    let categories = owned_vocabulary::visible_categories(state, user_ctx)
        .await
        .map_err(internal_err)?;

    let json = to_json(&Page::all(categories))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
}

/// List tags in a category. A vocabulary the caller may not read answers as
/// one that does not exist: no tags.
pub async fn list_tags(
    state: &AppState,
    user_ctx: &UserContext,
    params: ListTagsParams,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;
    validate_machine_name(&params.category_id, "category_id")?;

    let readable = owned_vocabulary::visible_category(state, user_ctx, &params.category_id)
        .await
        .map_err(internal_err)?
        .is_some();
    let tags = if readable {
        state
            .categories()
            .list_tags(&params.category_id)
            .await
            .map_err(internal_err)?
    } else {
        Vec::new()
    };

    let json = to_json(&Page::all(tags))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
}
