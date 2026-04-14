//! Route metadata for API documentation and versioning.
//!
//! Provides structured metadata about kernel routes that plugins can
//! use to generate OpenAPI specs, API explorers, or client SDKs.

use axum::http::Method;
use serde::Serialize;

/// Metadata describing an API route.
#[derive(Debug, Clone, Serialize)]
pub struct RouteMetadata {
    /// HTTP method.
    pub method: String,

    /// Route path pattern (e.g., "/api/v1/items/{id}").
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

/// Registry of all API route metadata.
///
/// Built at startup from kernel route definitions. Plugins can
/// add their own route metadata via `register_route_metadata` host function.
#[derive(Debug, Default)]
pub struct RouteRegistry {
    routes: Vec<RouteMetadata>,
}

impl RouteRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register kernel API route metadata.
    pub fn register_kernel_routes(&mut self) {
        // Register the core API v1 routes
        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/api/v1/items".to_string(),
            summary: "List content items with filters and pagination".to_string(),
            parameters: vec![
                ParamMeta {
                    name: "type".to_string(),
                    location: "query".to_string(),
                    required: false,
                    description: "Content type filter".to_string(),
                },
                ParamMeta {
                    name: "page".to_string(),
                    location: "query".to_string(),
                    required: false,
                    description: "Page number (default 1)".to_string(),
                },
                ParamMeta {
                    name: "per_page".to_string(),
                    location: "query".to_string(),
                    required: false,
                    description: "Items per page (default 25)".to_string(),
                },
            ],
            response_type: "application/json".to_string(),
            tags: vec!["content".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/api/v1/items/{id}".to_string(),
            summary: "Get a single content item by ID".to_string(),
            parameters: vec![ParamMeta {
                name: "id".to_string(),
                location: "path".to_string(),
                required: true,
                description: "Item UUID".to_string(),
            }],
            response_type: "application/json".to_string(),
            tags: vec!["content".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/api/v1/search".to_string(),
            summary: "Full-text search across content".to_string(),
            parameters: vec![ParamMeta {
                name: "q".to_string(),
                location: "query".to_string(),
                required: true,
                description: "Search query string".to_string(),
            }],
            response_type: "application/json".to_string(),
            tags: vec!["search".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/api/v1/user/export".to_string(),
            summary: "Export user data (GDPR Article 20)".to_string(),
            parameters: vec![ParamMeta {
                name: "user_id".to_string(),
                location: "query".to_string(),
                required: false,
                description: "User UUID (admin only, defaults to self)".to_string(),
            }],
            response_type: "application/json".to_string(),
            tags: vec!["user".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/api/v1/categories".to_string(),
            summary: "List categories".to_string(),
            parameters: vec![],
            response_type: "application/json".to_string(),
            tags: vec!["categories".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/api/v1/items/autocomplete".to_string(),
            summary: "Search items by type and title for autocomplete".to_string(),
            parameters: vec![
                ParamMeta {
                    name: "type".to_string(),
                    location: "query".to_string(),
                    required: true,
                    description: "Content type machine name".to_string(),
                },
                ParamMeta {
                    name: "q".to_string(),
                    location: "query".to_string(),
                    required: true,
                    description: "Title prefix search query".to_string(),
                },
                ParamMeta {
                    name: "limit".to_string(),
                    location: "query".to_string(),
                    required: false,
                    description: "Max results (default 10, max 50)".to_string(),
                },
            ],
            response_type: "application/json".to_string(),
            tags: vec!["content".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::POST.to_string(),
            path: "/api/v1/ai/assist".to_string(),
            summary: "AI text transformation (rewrite, expand, shorten, translate, tone)"
                .to_string(),
            parameters: vec![],
            response_type: "application/json".to_string(),
            tags: vec!["ai".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::POST.to_string(),
            path: "/api/v1/chat".to_string(),
            summary: "AI chatbot with SSE streaming response".to_string(),
            parameters: vec![],
            response_type: "text/event-stream".to_string(),
            tags: vec!["ai".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::POST.to_string(),
            path: "/api/v1/search/expand".to_string(),
            summary: "AI query expansion — returns alternative search terms".to_string(),
            parameters: vec![],
            response_type: "application/json".to_string(),
            tags: vec!["search".to_string(), "ai".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::POST.to_string(),
            path: "/api/v1/search/summarize".to_string(),
            summary: "AI summary of search results (SSE stream)".to_string(),
            parameters: vec![],
            response_type: "text/event-stream".to_string(),
            tags: vec!["search".to_string(), "ai".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::POST.to_string(),
            path: "/api/v1/search/followup".to_string(),
            summary: "Follow-up conversation about search results (SSE stream)".to_string(),
            parameters: vec![],
            response_type: "text/event-stream".to_string(),
            tags: vec!["search".to_string(), "ai".to_string()],
            deprecated: false,
        });

        self.routes.push(RouteMetadata {
            method: Method::GET.to_string(),
            path: "/health".to_string(),
            summary: "Health check endpoint".to_string(),
            parameters: vec![],
            response_type: "application/json".to_string(),
            tags: vec!["infrastructure".to_string()],
            deprecated: false,
        });
    }

    /// Register a plugin-provided route.
    pub fn register(&mut self, meta: RouteMetadata) {
        self.routes.push(meta);
    }

    /// Get all registered routes.
    pub fn routes(&self) -> &[RouteMetadata] {
        &self.routes
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
                "version": "1.0.0",
                "license": {
                    "name": "GPL-2.0-or-later",
                    "url": "https://www.gnu.org/licenses/gpl-2.0.html"
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
