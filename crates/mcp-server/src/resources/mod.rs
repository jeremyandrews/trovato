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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // =========================================================================
    // list_resources
    // =========================================================================

    #[test]
    fn list_resources_returns_three_resources() {
        let result = list_resources().expect("list_resources should succeed");
        assert_eq!(result.resources.len(), 3);
    }

    #[test]
    fn list_resources_includes_content_types() {
        let result = list_resources().expect("list_resources");
        let names: Vec<&str> = result.resources.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"content-types"));
    }

    #[test]
    fn list_resources_includes_site_config() {
        let result = list_resources().expect("list_resources");
        let names: Vec<&str> = result.resources.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"site-config"));
    }

    #[test]
    fn list_resources_includes_recent_items() {
        let result = list_resources().expect("list_resources");
        let names: Vec<&str> = result.resources.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"recent-items"));
    }

    #[test]
    fn list_resources_all_have_trovato_uri_scheme() {
        let result = list_resources().expect("list_resources");
        for resource in &result.resources {
            assert!(
                resource.uri.as_str().starts_with("trovato://"),
                "resource '{}' should use trovato:// URI scheme, got: {}",
                resource.name,
                resource.uri
            );
        }
    }

    #[test]
    fn list_resources_all_have_json_mime_type() {
        let result = list_resources().expect("list_resources");
        for resource in &result.resources {
            assert_eq!(
                resource.mime_type.as_deref(),
                Some("application/json"),
                "resource '{}' should have JSON mime type",
                resource.name
            );
        }
    }

    #[test]
    fn list_resources_all_have_descriptions() {
        let result = list_resources().expect("list_resources");
        for resource in &result.resources {
            assert!(
                resource.description.is_some(),
                "resource '{}' should have a description",
                resource.name
            );
            assert!(
                !resource.description.as_ref().unwrap().is_empty(),
                "resource '{}' description should not be empty",
                resource.name
            );
        }
    }

    #[test]
    fn list_resources_has_no_pagination_cursor() {
        let result = list_resources().expect("list_resources");
        assert!(
            result.next_cursor.is_none(),
            "static resource list should not have a pagination cursor"
        );
    }

    // =========================================================================
    // list_resource_templates
    // =========================================================================

    #[test]
    fn list_resource_templates_returns_content_type_template() {
        let result = list_resource_templates().expect("list_resource_templates");
        assert_eq!(result.resource_templates.len(), 1);
        let tmpl = &result.resource_templates[0];
        assert_eq!(tmpl.uri_template, "trovato://content-type/{name}");
        assert_eq!(tmpl.name, "content-type");
    }

    #[test]
    fn list_resource_templates_template_has_json_mime_type() {
        let result = list_resource_templates().expect("list_resource_templates");
        let tmpl = &result.resource_templates[0];
        assert_eq!(tmpl.mime_type.as_deref(), Some("application/json"));
    }

    #[test]
    fn list_resource_templates_template_has_description() {
        let result = list_resource_templates().expect("list_resource_templates");
        let tmpl = &result.resource_templates[0];
        assert!(tmpl.description.is_some());
    }

    #[test]
    fn list_resource_templates_has_no_pagination_cursor() {
        let result = list_resource_templates().expect("list_resource_templates");
        assert!(result.next_cursor.is_none());
    }

    // =========================================================================
    // resource helper
    // =========================================================================

    #[test]
    fn resource_helper_builds_correct_resource() {
        let r = resource("trovato://test", "test", "A test resource");
        assert_eq!(r.uri.as_str(), "trovato://test");
        assert_eq!(r.name, "test");
        assert_eq!(r.description.as_deref(), Some("A test resource"));
        assert_eq!(r.mime_type.as_deref(), Some("application/json"));
    }
}
