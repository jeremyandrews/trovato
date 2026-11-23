//! Route metadata for API documentation and versioning.
//!
//! Builds `/api/v1/openapi.json` from the kernel's API routes and every
//! enabled plugin's `api` menu entries. The kernel half is read from
//! [`kernel_api`](crate::routes::api_v1::kernel_api), the declarations the
//! kernel router is built from; the plugin half through
//! [`served_api_entries`], the same rule the plugin router mounts from. So the
//! document lists exactly the routes the server serves; a test requests every
//! listed path and requires the router to answer it.

use axum::http::Method;
use serde::Serialize;

use crate::menu::MenuDefinition;
use crate::routes::plugin_api::served_api_entries;

/// Metadata describing an API route.
#[derive(Debug, Clone, Serialize)]
pub struct RouteMetadata {
    /// HTTP method.
    pub method: String,

    /// Route path pattern (e.g., "/api/v1/item/{id}").
    pub path: String,

    /// Short summary of the route's purpose.
    pub summary: String,

    /// Parameter descriptions.
    pub parameters: Vec<ParamMeta>,

    /// Description of the response content type.
    pub response_type: String,

    /// Tags for grouping (e.g., "content", "admin").
    pub tags: Vec<String>,

    /// Whether this route is deprecated.
    pub deprecated: bool,

    /// Longer description, when there is something to add to the summary.
    pub description: Option<String>,
}

/// Metadata about a route parameter.
#[derive(Debug, Clone, Serialize)]
pub struct ParamMeta {
    /// Parameter name.
    pub name: String,

    /// Where the parameter comes from: "path", "query", "header".
    pub location: String,

    /// Whether the parameter is required.
    pub required: bool,

    /// Description of the parameter.
    pub description: String,
}

/// Registry of all API route metadata: the kernel's routes and the plugins'.
#[derive(Debug, Default)]
pub struct RouteRegistry {
    routes: Vec<RouteMetadata>,
}

impl RouteRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the kernel's API routes: every route of
    /// [`kernel_api`](crate::routes::api_v1::kernel_api), its versioned path
    /// and any deprecated alias, plus the health check.
    pub fn register_kernel_routes(&mut self) {
        self.routes
            .extend(crate::routes::api_v1::kernel_api().docs());
        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/health".to_string(),
            summary: "Health check endpoint".to_string(),
            parameters: vec![],
            response_type: "application/json".to_string(),
            tags: vec!["infrastructure".to_string()],
            deprecated: false,
            description: None,
        });
    }

    /// Register every plugin `api` menu entry the plugin router serves.
    ///
    /// The summary is the entry's title, the tag its plugin, the path
    /// parameters come from the path, and a required permission is stated in
    /// the description.
    pub fn register_plugin_api_routes(&mut self, menus: &[MenuDefinition]) {
        let (served, _skipped) = served_api_entries(menus);
        for entry in served {
            let parameters = entry
                .path
                .split('/')
                .filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
                .map(|name| ParamMeta {
                    name: name.to_string(),
                    location: "path".to_string(),
                    required: true,
                    description: String::new(),
                })
                .collect();
            let permission = entry.menu.permission.trim();
            self.routes.push(RouteMetadata {
                method: entry.method,
                path: entry.path,
                summary: entry.menu.title.clone(),
                parameters,
                response_type: "application/json".to_string(),
                tags: vec![entry.menu.plugin.clone()],
                deprecated: false,
                description: (!permission.is_empty())
                    .then(|| format!("Requires the `{permission}` permission.")),
            });
        }
    }

    /// Get all registered routes.
    pub fn routes(&self) -> &[RouteMetadata] {
        &self.routes
    }

    /// The whole document: kernel routes plus the plugin routes in `menus`.
    pub fn openapi_document(menus: &[MenuDefinition]) -> serde_json::Value {
        let mut registry = Self::new();
        registry.register_kernel_routes();
        registry.register_plugin_api_routes(menus);
        registry.to_openapi_json()
    }

    /// Generate an OpenAPI 3.0 spec from registered routes.
    pub fn to_openapi_json(&self) -> serde_json::Value {
        let mut paths = serde_json::Map::new();

        for route in &self.routes {
            let method = route.method.to_lowercase();

            // Build parameters array
            let params: Vec<serde_json::Value> = route
                .parameters
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "in": p.location,
                        "required": p.required,
                        "description": p.description,
                        "schema": { "type": "string" }
                    })
                })
                .collect();

            let response_type = &route.response_type;
            let mut operation = serde_json::json!({
                "summary": route.summary.clone(),
                "tags": route.tags.clone(),
                "responses": {
                    "200": {
                        "description": "Successful response",
                        "content": {
                            response_type: {
                                "schema": { "type": "object" }
                            }
                        }
                    }
                }
            });

            if !params.is_empty() {
                operation["parameters"] = serde_json::Value::Array(params);
            }

            if route.deprecated {
                operation["deprecated"] = serde_json::Value::Bool(true);
            }

            if let Some(description) = &route.description {
                operation["description"] = serde_json::Value::String(description.clone());
            }

            // Convert {id} to OpenAPI {id} format (already correct)
            let path_entry = paths
                .entry(route.path.clone())
                .or_insert_with(|| serde_json::json!({}));
            path_entry[method] = operation;
        }

        serde_json::json!({
            "openapi": "3.0.3",
            "info": {
                "title": "Trovato API",
                "description": "REST API for the Trovato content management system",
                "version": env!("CARGO_PKG_VERSION"),
                "license": {
                    "name": "MIT OR Apache-2.0",
                    "url": "https://github.com/jeremyandrews/trovato#license"
                }
            },
            "servers": [
                {
                    "url": "/",
                    "description": "Current server"
                }
            ],
            "paths": paths
        })
    }
}
