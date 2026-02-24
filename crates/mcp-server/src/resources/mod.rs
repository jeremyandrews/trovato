//! MCP resource implementations for Trovato CMS.
//!
//! Resources provide read-only context data via `trovato://` URIs.

pub mod content_types;
pub mod site;

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::tools::validate_machine_name;

/// List all available static resources.
pub fn list_resources() -> Result<ListResourcesResult, McpError> {
    let resources = vec![
        resource(
            "trovato://content-types",
            "content-types",
            "All content type definitions with field schemas",
        ),
        resource(
            "trovato://site-config",
            "site-config",
            "Public site configuration (name, slogan, language)",
        ),
        resource(
            "trovato://recent-items",
            "recent-items",
            "20 most recently published items",
        ),
    ];

    Ok(ListResourcesResult {
        resources,
        next_cursor: None,
        meta: None,
    })
}

/// List resource templates (URI patterns with placeholders).
pub fn list_resource_templates() -> Result<ListResourceTemplatesResult, McpError> {
    let templates = vec![
        RawResourceTemplate {
            uri_template: "trovato://content-type/{name}".into(),
            name: "content-type".into(),
            title: None,
            description: Some("Schema for a specific content type by machine name".into()),
            mime_type: Some("application/json".into()),
            icons: None,
        }
        .no_annotation(),
    ];

    Ok(ListResourceTemplatesResult {
        resource_templates: templates,
        next_cursor: None,
        meta: None,
    })
}

/// Read a resource by URI.
pub async fn read_resource(
    state: &AppState,
    user_ctx: &UserContext,
    request: ReadResourceRequestParams,
) -> Result<ReadResourceResult, McpError> {
    let uri = request.uri.as_str();

    // Check access content permission for all resources
    crate::tools::require_mcp_permission(user_ctx, "access content")?;

    match uri {
        "trovato://content-types" => content_types::read_all(state).await,
        "trovato://site-config" => site::read_site_config(state).await,
        "trovato://recent-items" => site::read_recent_items(state).await,
        _ if uri.starts_with("trovato://content-type/") => {
            let name = &uri["trovato://content-type/".len()..];
            validate_machine_name(name, "content type name")?;
            content_types::read_one(state, name).await
        }
        _ => Err(McpError::resource_not_found(
            format!("unknown resource: {uri}"),
            None,
        )),
    }
}

/// Helper to build a resource with description and JSON mime type.
fn resource(uri: &str, name: &str, description: &str) -> Resource {
    RawResource {
        uri: uri.into(),
        name: name.into(),
        title: None,
        description: Some(description.into()),
        mime_type: Some("application/json".into()),
        size: None,
        icons: None,
        meta: None,
    }
    .no_annotation()
}
