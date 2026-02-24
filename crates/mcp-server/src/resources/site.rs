//! Site configuration and recent items resources.
//!
//! - `trovato://site-config` — public site settings
//! - `trovato://recent-items` — 20 most recent published items

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::models::SiteConfig;
use trovato_kernel::state::AppState;

use crate::tools::to_json;

/// Read public site configuration.
pub async fn read_site_config(state: &AppState) -> Result<ReadResourceResult, McpError> {
    let site_name = SiteConfig::get(state.db(), "site_name")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "Trovato".to_string());

    let slogan = SiteConfig::get(state.db(), "site_slogan")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    let language = state.default_language().to_string();

    let config = serde_json::json!({
        "site_name": site_name,
        "slogan": slogan,
        "default_language": language,
    });

    let json = to_json(&config)?;

    Ok(ReadResourceResult {
        contents: vec![ResourceContents::text(json, "trovato://site-config")],
    })
}

/// Read the 20 most recently published items.
///
/// Uses [`ItemService::list_published`] for consistency with the service
/// layer, ensuring future tap integrations (e.g. `tap_item_view`) apply.
///
/// Note: this returns all published items without per-item `tap_item_access`
/// checks, consistent with how list endpoints work elsewhere in the kernel.
pub async fn read_recent_items(state: &AppState) -> Result<ReadResourceResult, McpError> {
    let items = state
        .items()
        .list_published(20, 0)
        .await
        .map_err(crate::tools::internal_err)?;

    let summaries: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "id": item.id,
                "title": item.title,
                "type": item.item_type,
                "created": item.created,
                "changed": item.changed,
            })
        })
        .collect();

    let json = to_json(&summaries)?;

    Ok(ReadResourceResult {
        contents: vec![ResourceContents::text(json, "trovato://recent-items")],
    })
}
