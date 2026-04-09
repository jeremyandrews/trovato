//! Content item CRUD tools.
//!
//! Implements `list_items`, `get_item`, `create_item`, `update_item`,
//! and `delete_item` MCP tools via the kernel's [`ItemService`](trovato_kernel::content::ItemService), ensuring
//! all tap integrations (insert, update, delete, view, access) are invoked.

use rmcp::ErrorData as McpError;
use rmcp::model::*;
use uuid::Uuid;

use trovato_kernel::LIVE_STAGE_ID;
use trovato_kernel::models::Item;
use trovato_kernel::models::item::{CreateItem, UpdateItem};
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::server::{
    CreateItemParams, DeleteItemParams, GetItemParams, ListItemsParams, UpdateItemParams,
};
use crate::tools::{ACCESS_DENIED_MSG, internal_err, require_mcp_permission, to_json};

/// Maximum allowed length for item titles.
const MAX_TITLE_LENGTH: usize = 1000;

/// Parse a UUID string, returning an MCP invalid-params error on failure.
fn parse_uuid(s: &str) -> Result<Uuid, McpError> {
    s.parse::<Uuid>()
        .map_err(|_| McpError::invalid_params(format!("invalid UUID: {s}"), None))
}

/// Map an anyhow error from `ItemService` to an MCP error.
///
/// Access-denied errors are mapped to "item not found" to avoid revealing
/// item existence (consistent with `get_item`). All other errors become
/// generic internal errors.
fn map_service_err(e: anyhow::Error, id: Uuid) -> McpError {
    if e.to_string().contains(ACCESS_DENIED_MSG) {
        return McpError::invalid_params(format!("item not found: {id}"), None);
    }
    internal_err(e)
}

/// List items with optional filtering.
///
/// Uses [`ItemService::list_filtered`](trovato_kernel::content::ItemService::list_filtered) which returns `(Vec<Item>, i64)`
/// in a single logical operation. Non-admin users only see published items
/// unless they explicitly filter by status and have appropriate permissions.
pub async fn list_items(
    state: &AppState,
    user_ctx: &UserContext,
    params: ListItemsParams,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;

    let per_page = i64::from(params.per_page.unwrap_or(20).clamp(1, 100));
    let page = i64::from(params.page.unwrap_or(1).max(1));
    let offset = (page - 1) * per_page;

    let author_id = params.author_id.as_deref().map(parse_uuid).transpose()?;

    // Non-admin users can only see published items. If the caller requested
    // unpublished (status=0) or all (status=None), force published-only.
    let status = if user_ctx.is_admin() {
        params.status
    } else {
        Some(params.status.unwrap_or(1).max(1))
    };

    let (items, total) = state
        .items()
        .list_filtered(
            params.content_type.as_deref(),
            status,
            author_id,
            per_page,
            offset,
        )
        .await
        .map_err(internal_err)?;

    let result = serde_json::json!({
        "items": items.iter().map(item_summary).collect::<Vec<_>>(),
        "total": total,
        "page": page,
        "per_page": per_page,
    });

    Ok(CallToolResult::success(vec![Content::text(to_json(
        &result,
    )?)]))
}

/// Get a single item by ID.
///
/// Uses [`ItemService::load`](trovato_kernel::content::ItemService::load) + [`ItemService::check_access`](trovato_kernel::content::ItemService::check_access) for
/// tap-integrated access control. Unpublished items require per-type
/// view permission or plugin-granted access via `tap_item_access`.
pub async fn get_item(
    state: &AppState,
    user_ctx: &UserContext,
    params: GetItemParams,
) -> Result<CallToolResult, McpError> {
    let id = parse_uuid(&params.id)?;

    let item = state
        .items()
        .load(id)
        .await
        .map_err(internal_err)?
        .ok_or_else(|| McpError::invalid_params(format!("item not found: {id}"), None))?;

    // Use ItemService::check_access for tap-integrated permission checking.
    // This invokes tap_item_access plugins and falls back to role-based
    // permissions (e.g. "view page content" for unpublished pages).
    if !state
        .items()
        .check_access(&item, "view", user_ctx)
        .await
        .map_err(internal_err)?
    {
        // Return "not found" to avoid revealing item existence
        return Err(McpError::invalid_params(
            format!("item not found: {id}"),
            None,
        ));
    }

    let json = to_json(&item)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

/// Create a new item.
///
/// Uses [`ItemService::create`](trovato_kernel::content::ItemService::create) which invokes `tap_item_insert` after
/// the database insert, allowing plugins to react to new content.
pub async fn create_item(
    state: &AppState,
    user_ctx: &UserContext,
    params: CreateItemParams,
) -> Result<CallToolResult, McpError> {
    // ItemService::create does not check create permissions (route handlers do)
    require_mcp_permission(user_ctx, "create content")?;

    // Validate title length
    if params.title.len() > MAX_TITLE_LENGTH {
        return Err(McpError::invalid_params(
            format!("title exceeds maximum length of {MAX_TITLE_LENGTH} characters"),
            None,
        ));
    }

    // Validate content type exists
    if state.content_types().get(&params.content_type).is_none() {
        return Err(McpError::invalid_params(
            format!("unknown content type: {}", params.content_type),
            None,
        ));
    }

    let input = CreateItem {
        item_type: params.content_type,
        title: params.title,
        author_id: user_ctx.id,
        status: params.status,
        promote: None,
        sticky: None,
        fields: params.fields,
        stage_id: Some(LIVE_STAGE_ID),
        language: None,
        log: Some("Created via MCP".to_string()),
    };

    let item = state
        .items()
        .create(input, user_ctx)
        .await
        .map_err(internal_err)?;

    let json = to_json(&item)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

/// Update an existing item.
///
/// Uses [`ItemService::update`](trovato_kernel::content::ItemService::update) which checks edit access via
/// `tap_item_access` and invokes `tap_item_update` after the write.
pub async fn update_item(
    state: &AppState,
    user_ctx: &UserContext,
    params: UpdateItemParams,
) -> Result<CallToolResult, McpError> {
    // Validate title length if provided
    if let Some(ref title) = params.title
        && title.len() > MAX_TITLE_LENGTH
    {
        return Err(McpError::invalid_params(
            format!("title exceeds maximum length of {MAX_TITLE_LENGTH} characters"),
            None,
        ));
    }

    let id = parse_uuid(&params.id)?;

    let input = UpdateItem {
        title: params.title,
        status: params.status,
        promote: None,
        sticky: None,
        fields: params.fields,
        log: params.log.or(Some("Updated via MCP".to_string())),
    };

    // ItemService::update loads the item, checks "edit" access via
    // check_access (tap_item_access + role fallback), updates, and
    // invokes tap_item_update. Returns bail!("access denied") on denial.
    // Access denied is mapped to "not found" to avoid revealing item existence.
    let item = state
        .items()
        .update(id, input, user_ctx)
        .await
        .map_err(|e| map_service_err(e, id))?
        .ok_or_else(|| McpError::invalid_params(format!("item not found: {id}"), None))?;

    let json = to_json(&item)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

/// Delete an item.
///
/// Uses [`ItemService::delete`](trovato_kernel::content::ItemService::delete) which checks delete access via
/// `tap_item_access` and invokes `tap_item_delete` before the write.
pub async fn delete_item(
    state: &AppState,
    user_ctx: &UserContext,
    params: DeleteItemParams,
) -> Result<CallToolResult, McpError> {
    let id = parse_uuid(&params.id)?;

    // ItemService::delete loads the item, checks "delete" access via
    // check_access (tap_item_access + role fallback), invokes
    // tap_item_delete, then deletes. Returns bail!("access denied") on denial.
    // Access denied is mapped to "not found" to avoid revealing item existence.
    let deleted = state
        .items()
        .delete(id, user_ctx)
        .await
        .map_err(|e| map_service_err(e, id))?;

    if deleted {
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Item {id} deleted successfully"
        ))]))
    } else {
        Err(McpError::invalid_params(
            format!("item not found: {id}"),
            None,
        ))
    }
}

/// Build a summary JSON object for an item (used in list results).
fn item_summary(item: &Item) -> serde_json::Value {
    serde_json::json!({
        "id": item.id,
        "title": item.title,
        "type": item.item_type,
        "status": item.status,
        "created": item.created,
        "changed": item.changed,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;

    // =========================================================================
    // parse_uuid
    // =========================================================================

    #[test]
    fn parse_uuid_accepts_valid_uuid() {
        let id = Uuid::new_v4();
        let result = parse_uuid(&id.to_string());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), id);
    }

    #[test]
    fn parse_uuid_accepts_nil_uuid() {
        let result = parse_uuid("00000000-0000-0000-0000-000000000000");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Uuid::nil());
    }

    #[test]
    fn parse_uuid_rejects_empty_string() {
        let result = parse_uuid("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("invalid UUID"));
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        let result = parse_uuid("not-a-uuid");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn parse_uuid_rejects_partial_uuid() {
        let result = parse_uuid("550e8400-e29b-41d4-a716");
        assert!(result.is_err());
    }

    #[test]
    fn parse_uuid_error_includes_input() {
        let result = parse_uuid("bad-input");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("bad-input"),
            "error should include the invalid input"
        );
    }

    // =========================================================================
    // map_service_err
    // =========================================================================

    #[test]
    fn map_service_err_converts_access_denied_to_not_found() {
        let id = Uuid::new_v4();
        let err = map_service_err(anyhow::anyhow!("access denied"), id);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(
            err.message.contains("item not found"),
            "access denied should be mapped to 'not found'"
        );
        assert!(
            err.message.contains(&id.to_string()),
            "error should include the item ID"
        );
    }

    #[test]
    fn map_service_err_converts_access_denied_substring() {
        let id = Uuid::new_v4();
        // The kernel error may have additional context around "access denied"
        let err = map_service_err(
            anyhow::anyhow!("item operation failed: access denied for user"),
            id,
        );
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("item not found"));
    }

    #[test]
    fn map_service_err_converts_other_errors_to_internal() {
        let id = Uuid::new_v4();
        let err = map_service_err(anyhow::anyhow!("database connection timeout"), id);
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
        // Should NOT leak the original error message
        assert_eq!(err.message.as_ref(), "internal error");
    }

    // =========================================================================
    // item_summary
    // =========================================================================

    /// Build a test `Item` with the given overrides.
    fn test_item(id: Uuid, title: &str, item_type: &str) -> Item {
        Item {
            id,
            current_revision_id: None,
            item_type: item_type.to_string(),
            title: title.to_string(),
            author_id: Uuid::new_v4(),
            status: 1,
            created: 1_700_000_000,
            changed: 1_700_000_100,
            promote: 0,
            sticky: 0,
            fields: serde_json::json!({}),
            stage_id: trovato_kernel::LIVE_STAGE_ID,
            language: "en".to_string(),
            item_group_id: Uuid::new_v4(),
            retention_days: None,
        }
    }

    #[test]
    fn item_summary_includes_required_fields() {
        let id = Uuid::new_v4();
        let item = test_item(id, "Test Article", "article");

        let summary = item_summary(&item);
        assert_eq!(summary["id"], id.to_string());
        assert_eq!(summary["title"], "Test Article");
        // item_summary uses json! macro with "type" key directly
        assert_eq!(summary["type"], "article");
        assert_eq!(summary["status"], 1);
        assert_eq!(summary["created"], 1_700_000_000);
        assert_eq!(summary["changed"], 1_700_000_100);
    }

    #[test]
    fn item_summary_does_not_include_fields() {
        let mut item = test_item(Uuid::new_v4(), "Page", "page");
        item.fields = serde_json::json!({"body": "secret data"});

        let summary = item_summary(&item);
        assert!(
            summary.get("fields").is_none(),
            "summary should not include fields (use get_item for full data)"
        );
        assert!(summary.get("author_id").is_none());
        assert!(summary.get("promote").is_none());
        assert!(summary.get("sticky").is_none());
    }

    // =========================================================================
    // MAX_TITLE_LENGTH
    // =========================================================================

    #[test]
    fn max_title_length_is_reasonable() {
        assert_eq!(MAX_TITLE_LENGTH, 1000);
    }
}
