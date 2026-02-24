//! MCP server implementation for Trovato CMS.
//!
//! Implements the [`rmcp::ServerHandler`] trait to expose Trovato content,
//! schema, and search capabilities via the Model Context Protocol.

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;

use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::resources;
use crate::tools;

// =============================================================================
// Parameter types (schemars derives JSON Schema for MCP tool descriptions)
// =============================================================================

/// Parameters for listing items.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListItemsParams {
    /// Filter by content type machine name (e.g. "article", "page").
    pub content_type: Option<String>,
    /// Filter by status: 1 = published, 0 = unpublished.
    pub status: Option<i16>,
    /// Filter by author user ID.
    pub author_id: Option<String>,
    /// Page number (1-indexed, default: 1).
    pub page: Option<u32>,
    /// Items per page (max 100, default: 20).
    pub per_page: Option<u32>,
}

/// Parameters for getting a single item.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetItemParams {
    /// The UUID of the item to retrieve.
    pub id: String,
}

/// Parameters for creating a new item.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateItemParams {
    /// Content type machine name (e.g. "article").
    pub content_type: String,
    /// Item title.
    pub title: String,
    /// Status: 1 = published (default), 0 = unpublished.
    pub status: Option<i16>,
    /// Dynamic fields as a JSON object (content-type specific).
    pub fields: Option<serde_json::Value>,
}

/// Parameters for updating an existing item.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateItemParams {
    /// The UUID of the item to update.
    pub id: String,
    /// New title (optional).
    pub title: Option<String>,
    /// New status (optional): 1 = published, 0 = unpublished.
    pub status: Option<i16>,
    /// Updated dynamic fields as a JSON object (optional).
    pub fields: Option<serde_json::Value>,
    /// Revision log message describing the change (optional).
    pub log: Option<String>,
}

/// Parameters for deleting an item.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteItemParams {
    /// The UUID of the item to delete.
    pub id: String,
}

/// Parameters for full-text search.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Search query string (max 1000 characters).
    #[schemars(length(max = 1000))]
    pub query: String,
    /// Maximum results to return (default: 20, max: 100).
    pub limit: Option<u32>,
    /// Number of results to skip for pagination (default: 0).
    pub offset: Option<u32>,
}

/// Parameters for listing tags in a category.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListTagsParams {
    /// Category machine name (e.g. "topics").
    pub category_id: String,
}

/// Parameters for executing a named Gather query.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunGatherParams {
    /// Gather query machine name.
    pub query_id: String,
    /// Page number (1-indexed, default: 1).
    pub page: Option<u32>,
    /// Exposed filter values as key-value pairs (optional).
    pub filters: Option<HashMap<String, serde_json::Value>>,
}

// =============================================================================
// MCP Server
// =============================================================================

/// How often to re-validate the API token and user status (in seconds).
///
/// During a long-running STDIO session, the API token may be revoked or the
/// user account deactivated. This interval controls how often the server
/// re-checks against the database.
const SESSION_REVALIDATION_SECS: u64 = 300;

/// Trovato MCP server exposing CMS content and schema to AI tools.
#[derive(Clone)]
pub struct TrovatoMcpServer {
    /// Kernel application state (DB, services, etc.).
    state: AppState,
    /// Raw API token for periodic session revalidation.
    raw_token: Arc<String>,
    /// User context with pre-loaded permissions (for `ItemService` calls).
    ///
    /// Permissions are loaded once at session start and cached for the
    /// session lifetime. Revalidation checks token validity and user
    /// active status but does **not** refresh permissions — an admin
    /// must revoke the API token itself for immediate effect.
    user_ctx: Arc<UserContext>,
    /// When the session was last validated against the database.
    validated_at: Arc<std::sync::Mutex<std::time::Instant>>,
    /// Tool router generated by `#[tool_router]`.
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for TrovatoMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrovatoMcpServer")
            .field("user_id", &self.user_ctx.id)
            .finish_non_exhaustive()
    }
}

#[tool_router]
impl TrovatoMcpServer {
    /// Create a new MCP server instance.
    ///
    /// `user_ctx` must be built from the same `user` via
    /// [`crate::auth::build_user_context`] so that the `ItemService` has
    /// the correct permissions for tap-based access control.
    ///
    /// `raw_token` is stored for periodic session revalidation — the server
    /// re-checks that the token is still valid and the user is still active
    /// every `SESSION_REVALIDATION_SECS` seconds.
    pub fn new(state: AppState, raw_token: String, user_ctx: UserContext) -> Self {
        Self {
            state,
            raw_token: Arc::new(raw_token),
            user_ctx: Arc::new(user_ctx),
            validated_at: Arc::new(std::sync::Mutex::new(std::time::Instant::now())),
            tool_router: Self::tool_router(),
        }
    }

    /// Re-validate the API token and user status if the revalidation
    /// interval has elapsed.
    ///
    /// This catches token revocation and user deactivation during
    /// long-running STDIO sessions.
    async fn revalidate_session(&self) -> Result<(), McpError> {
        let needs_check = self.validated_at.lock().ok().is_none_or(|v| {
            v.elapsed() >= std::time::Duration::from_secs(SESSION_REVALIDATION_SECS)
        });

        if !needs_check {
            return Ok(());
        }

        // Re-validate token against DB (checks expiry, user active status)
        crate::auth::resolve_token(&self.state, &self.raw_token)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "session revalidation failed");
                McpError::invalid_request(
                    "session expired: token invalid or user inactive".to_string(),
                    None,
                )
            })?;

        if let Ok(mut validated) = self.validated_at.lock() {
            *validated = std::time::Instant::now();
        }

        Ok(())
    }

    // =========================================================================
    // Content tools (AC #3)
    // =========================================================================

    /// List content items with optional filtering by type, status, and author.
    /// Returns paginated results.
    #[tool(
        description = "List content items with optional filtering by content type, status, and author. Returns paginated results with item ID, title, type, status, and timestamps."
    )]
    async fn list_items(
        &self,
        Parameters(params): Parameters<ListItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::items::list_items(&self.state, &self.user_ctx, params).await
    }

    /// Get a single content item by its UUID, including all fields.
    #[tool(
        description = "Get a single content item by its UUID. Returns the full item including title, type, status, author, timestamps, and all dynamic fields."
    )]
    async fn get_item(
        &self,
        Parameters(params): Parameters<GetItemParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::items::get_item(&self.state, &self.user_ctx, params).await
    }

    /// Create a new content item.
    #[tool(
        description = "Create a new content item. Requires specifying the content type and title. Dynamic fields depend on the content type schema. Returns the created item."
    )]
    async fn create_item(
        &self,
        Parameters(params): Parameters<CreateItemParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::items::create_item(&self.state, &self.user_ctx, params).await
    }

    /// Update an existing content item by UUID.
    #[tool(
        description = "Update an existing content item by UUID. All fields are optional — only provided fields will be changed. Returns the updated item."
    )]
    async fn update_item(
        &self,
        Parameters(params): Parameters<UpdateItemParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::items::update_item(&self.state, &self.user_ctx, params).await
    }

    /// Delete a content item by UUID.
    #[tool(
        description = "Delete a content item by UUID. This is permanent and cannot be undone. Returns success or error."
    )]
    async fn delete_item(
        &self,
        Parameters(params): Parameters<DeleteItemParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::items::delete_item(&self.state, &self.user_ctx, params).await
    }

    /// Full-text search across all published content.
    #[tool(
        description = "Full-text search across all published content. Returns matching items with relevance scores and text snippets highlighting matches."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::search::search(&self.state, &self.user_ctx, params).await
    }

    // =========================================================================
    // Schema & category tools (AC #4)
    // =========================================================================

    /// List all content type definitions with their field schemas.
    #[tool(
        description = "List all content type definitions. Returns each type's machine name, label, description, and field definitions including field name, type, label, and whether it's required."
    )]
    async fn list_content_types(&self) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::content_types::list_content_types(&self.state, &self.user_ctx).await
    }

    /// List all category vocabularies.
    #[tool(
        description = "List all category vocabularies (taxonomies). Returns each vocabulary's machine name, label, description, and hierarchy type."
    )]
    async fn list_categories(&self) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::categories::list_categories(&self.state, &self.user_ctx).await
    }

    /// List tags in a specific category vocabulary.
    #[tool(
        description = "List all tags within a specific category vocabulary. Provide the category machine name. Returns tag ID, label, description, and weight."
    )]
    async fn list_tags(
        &self,
        Parameters(params): Parameters<ListTagsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::categories::list_tags(&self.state, &self.user_ctx, params).await
    }

    /// Execute a named Gather query.
    #[tool(
        description = "Execute a named Gather query definition. Gather queries are pre-defined content queries with filters, sorts, and pagination. Returns the query results as JSON."
    )]
    async fn run_gather(
        &self,
        Parameters(params): Parameters<RunGatherParams>,
    ) -> Result<CallToolResult, McpError> {
        self.revalidate_session().await?;
        tools::gather::run_gather(&self.state, &self.user_ctx, params).await
    }
}

#[tool_handler]
impl ServerHandler for TrovatoMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Trovato CMS MCP server. Provides tools for content management \
                 (CRUD, search, categories) and resources for schema introspection. \
                 All operations respect the authenticated user's permissions."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            // server_info defaults to Implementation::from_build_env()
            // which uses CARGO_CRATE_NAME ("trovato_mcp") and CARGO_PKG_VERSION
            ..Default::default()
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        resources::list_resources()
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        resources::list_resource_templates()
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        resources::read_resource(&self.state, &self.user_ctx, request).await
    }
}
