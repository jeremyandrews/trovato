// Tests are allowed to use unwrap/expect freely.
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for the Trovato MCP server.
//!
//! Tests exercise tool and resource handlers directly (no STDIO transport,
//! except the one wire-level `tools/list` check).
//! using a real kernel AppState with a test database.

mod common;

use common::{run_test, shared_app};
use rmcp::ServerHandler;
use rmcp::model::*;
use trovato_kernel::LIVE_STAGE_ID;
use trovato_kernel::models::Item;
use trovato_kernel::models::item::CreateItem;

use trovato_mcp::server::{
    CreateItemParams, DeleteItemParams, GetItemParams, ListItemsParams, SearchParams,
    UpdateItemParams,
};

// =============================================================================
// Tool list completeness (AC #9a)
// =============================================================================

#[test]
fn tool_list_includes_all_expected_tools() {
    run_test(async {
        let ctx = shared_app().await;
        let server = ctx.mcp_server();

        // Use get_info to verify capabilities
        let info = server.get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "Server should advertise tool capabilities"
        );

        // Verify each expected tool is registered
        let expected_tools = [
            "list_items",
            "get_item",
            "create_item",
            "update_item",
            "delete_item",
            "search",
            "list_content_types",
            "list_categories",
            "list_tags",
            "run_gather",
        ];

        for name in &expected_tools {
            assert!(
                server.get_tool(name).is_some(),
                "Expected tool '{name}' to be registered"
            );
        }
    });
}

// =============================================================================
// Content tools (AC #9b, #9c)
// =============================================================================

#[test]
fn get_item_returns_correct_data() {
    run_test(async {
        let ctx = shared_app().await;

        // Create a test item
        let item = Item::create(
            ctx.state.db(),
            CreateItem {
                item_type: "page".to_string(),
                title: "MCP Test Item".to_string(),
                author_id: ctx.admin_user.id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(serde_json::json!({"body": {"value": "Test body"}})),
                stage_id: Some(LIVE_STAGE_ID),
                language: None,
                log: Some("test".to_string()),
            },
        )
        .await
        .expect("create item");

        // Get it via MCP tool
        let result = trovato_mcp::tools::items::get_item(
            &ctx.state,
            &ctx.admin_user_ctx,
            GetItemParams {
                id: item.id.to_string(),
            },
        )
        .await
        .expect("get_item should succeed");

        let text = extract_text(&result);
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(json["title"], "MCP Test Item");
        // Item.item_type has #[serde(rename = "type")]
        assert_eq!(json["type"], "page");

        // Cleanup
        Item::delete(ctx.state.db(), item.id).await.ok();
    });
}

#[test]
fn list_items_returns_paginated_results() {
    run_test(async {
        let ctx = shared_app().await;

        let result = trovato_mcp::tools::items::list_items(
            &ctx.state,
            &ctx.admin_user_ctx,
            ListItemsParams {
                content_type: None,
                status: Some(1),
                author_id: None,
                page: Some(1),
                per_page: Some(5),
                limit: None,
                offset: None,
            },
        )
        .await
        .expect("list_items should succeed");

        let text = extract_text(&result);
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(json["data"].is_array(), "response should have a data array");
        assert!(json["total"].is_number(), "response should have total");
        assert_eq!(json["page"], 1);
        assert_eq!(json["per_page"], 5);
    });
}

#[test]
fn create_and_delete_item_via_tools() {
    run_test(async {
        let ctx = shared_app().await;

        // Create via MCP tool
        let result = trovato_mcp::tools::items::create_item(
            &ctx.state,
            &ctx.admin_user_ctx,
            CreateItemParams {
                content_type: "page".to_string(),
                title: "MCP Created Item".to_string(),
                status: Some(1),
                fields: None,
            },
        )
        .await
        .expect("create_item should succeed");

        let text = extract_text(&result);
        let created: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        let item_id = created["id"].as_str().expect("id field");

        // Delete via MCP tool
        let delete_result = trovato_mcp::tools::items::delete_item(
            &ctx.state,
            &ctx.admin_user_ctx,
            DeleteItemParams {
                id: item_id.to_string(),
            },
        )
        .await
        .expect("delete_item should succeed");

        let delete_text = extract_text(&delete_result);
        assert!(delete_text.contains("deleted successfully"));
    });
}

#[test]
fn update_item_via_tool() {
    run_test(async {
        let ctx = shared_app().await;

        // Create item first
        let item = Item::create(
            ctx.state.db(),
            CreateItem {
                item_type: "page".to_string(),
                title: "Before Update".to_string(),
                author_id: ctx.admin_user.id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: Some(LIVE_STAGE_ID),
                language: None,
                log: Some("test".to_string()),
            },
        )
        .await
        .expect("create item");

        // Update via MCP tool
        let result = trovato_mcp::tools::items::update_item(
            &ctx.state,
            &ctx.admin_user_ctx,
            UpdateItemParams {
                id: item.id.to_string(),
                title: Some("After Update".to_string()),
                status: None,
                fields: None,
                log: Some("MCP update test".to_string()),
            },
        )
        .await
        .expect("update_item should succeed");

        let text = extract_text(&result);
        let updated: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(updated["title"], "After Update");

        // Cleanup
        Item::delete(ctx.state.db(), item.id).await.ok();
    });
}

// =============================================================================
// Search tool (AC #9c)
// =============================================================================

#[test]
fn search_returns_results() {
    run_test(async {
        let ctx = shared_app().await;

        // Search for any content (may be empty in test DB, just verify no error)
        let result = trovato_mcp::tools::search::search(
            &ctx.state,
            &ctx.admin_user_ctx,
            SearchParams {
                query: "test".to_string(),
                page: None,
                per_page: None,
                limit: Some(5),
                offset: None,
            },
        )
        .await
        .expect("search should succeed");

        let text = extract_text(&result);
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(json["data"].is_array());
        assert_eq!(json["per_page"], 5, "limit is an alias of per_page");
    });
}

// =============================================================================
// Schema tools (AC #9d)
// =============================================================================

#[test]
fn list_content_types_returns_schema() {
    run_test(async {
        let ctx = shared_app().await;

        let result =
            trovato_mcp::tools::content_types::list_content_types(&ctx.state, &ctx.admin_user_ctx)
                .await
                .expect("list_content_types should succeed");

        let text = extract_text(&result);
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(
            json["data"].is_array(),
            "should return a page of content types"
        );
    });
}

#[test]
fn list_categories_returns_data() {
    run_test(async {
        let ctx = shared_app().await;

        let result =
            trovato_mcp::tools::categories::list_categories(&ctx.state, &ctx.admin_user_ctx)
                .await
                .expect("list_categories should succeed");

        let text = extract_text(&result);
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(json["data"].is_array());
    });
}

// =============================================================================
// Permission enforcement (AC #9e)
// =============================================================================

#[test]
fn permission_denied_delete_without_permission() {
    run_test(async {
        let ctx = shared_app().await;

        // Create an item as admin so there's something to try to delete
        let item = Item::create(
            ctx.state.db(),
            CreateItem {
                item_type: "page".to_string(),
                title: "Delete Perm Test".to_string(),
                author_id: ctx.admin_user.id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: Some(LIVE_STAGE_ID),
                language: None,
                log: Some("test".to_string()),
            },
        )
        .await
        .expect("create item");

        // Use a user without "delete content" permission — service layer
        // checks via tap_item_access + role-based fallback.
        // Access denied is mapped to "not found" to avoid revealing item existence.
        let result = trovato_mcp::tools::items::delete_item(
            &ctx.state,
            &ctx.unprivileged_user_ctx,
            DeleteItemParams {
                id: item.id.to_string(),
            },
        )
        .await;

        assert!(result.is_err(), "should deny delete without permission");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("not found"),
            "error should say 'not found' to hide item existence: {}",
            err.message
        );

        // Cleanup
        Item::delete(ctx.state.db(), item.id).await.ok();
    });
}

#[test]
fn permission_denied_create_without_create_content() {
    run_test(async {
        let ctx = shared_app().await;

        let result = trovato_mcp::tools::items::create_item(
            &ctx.state,
            &ctx.unprivileged_user_ctx,
            CreateItemParams {
                content_type: "page".to_string(),
                title: "Should Fail".to_string(),
                status: None,
                fields: None,
            },
        )
        .await;

        assert!(result.is_err(), "should deny create without permission");
    });
}

// =============================================================================
// Resources (AC #9f)
// =============================================================================

#[test]
fn resource_list_includes_all_resources() {
    run_test(async {
        let result = trovato_mcp::resources::list_resources().expect("list_resources");

        let names: Vec<&str> = result.resources.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"content-types"));
        assert!(names.contains(&"site-config"));
        assert!(names.contains(&"recent-items"));
    });
}

#[test]
fn read_site_config_resource() {
    run_test(async {
        let ctx = shared_app().await;

        let result = trovato_mcp::resources::read_resource(
            &ctx.state,
            &ctx.admin_user_ctx,
            ReadResourceRequestParams::new("trovato://site-config"),
        )
        .await
        .expect("read site-config");

        assert!(!result.contents.is_empty());
        if let ResourceContents::TextResourceContents { text, .. } = &result.contents[0] {
            let json: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
            assert!(json["site_name"].is_string());
            assert!(json["default_language"].is_string());
        } else {
            panic!("expected text resource");
        }
    });
}

#[test]
fn read_content_types_resource() {
    run_test(async {
        let ctx = shared_app().await;

        let result = trovato_mcp::resources::read_resource(
            &ctx.state,
            &ctx.admin_user_ctx,
            ReadResourceRequestParams::new("trovato://content-types"),
        )
        .await
        .expect("read content-types");

        assert!(!result.contents.is_empty());
    });
}

#[test]
fn read_recent_items_resource() {
    run_test(async {
        let ctx = shared_app().await;

        let result = trovato_mcp::resources::read_resource(
            &ctx.state,
            &ctx.admin_user_ctx,
            ReadResourceRequestParams::new("trovato://recent-items"),
        )
        .await
        .expect("read recent-items");

        assert!(!result.contents.is_empty());
        if let ResourceContents::TextResourceContents { text, .. } = &result.contents[0] {
            let json: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
            assert!(json.is_array());
        } else {
            panic!("expected text resource");
        }
    });
}

// =============================================================================
// Resource template (AC #5: content-type/{name})
// =============================================================================

#[test]
fn read_single_content_type_resource() {
    run_test(async {
        let ctx = shared_app().await;

        let result = trovato_mcp::resources::read_resource(
            &ctx.state,
            &ctx.admin_user_ctx,
            ReadResourceRequestParams::new("trovato://content-type/page"),
        )
        .await
        .expect("read content-type/page");

        assert!(!result.contents.is_empty());
        if let ResourceContents::TextResourceContents { text, .. } = &result.contents[0] {
            let json: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
            assert!(json.is_object(), "single content type should be an object");
        } else {
            panic!("expected text resource");
        }
    });
}

// =============================================================================
// Authentication (AC #9f: invalid token rejected)
// =============================================================================

#[test]
fn invalid_token_is_rejected() {
    run_test(async {
        let ctx = shared_app().await;

        let result =
            trovato_mcp::auth::resolve_token(&ctx.state, "trv_this_is_not_a_valid_token").await;

        assert!(result.is_err(), "invalid token should be rejected");
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("invalid") || err.contains("expired") || err.contains("failed"),
            "error should indicate invalid token: {err}"
        );
    });
}

// =============================================================================
// Helpers
// =============================================================================

/// Extract the text content from a `CallToolResult`.
fn extract_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| {
            if let ContentBlock::Text(t) = c {
                Some(t.text.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// The tool list as a client sees it over the wire: the server is served on
/// one end of an in-process byte pipe, exactly as `main.rs` serves it on
/// STDIO, and the test speaks raw JSON-RPC on the other end. This is the
/// check that survives an rmcp upgrade, because it does not go through any
/// rmcp API on the client side: if a new rmcp renamed, dropped or added a
/// tool, or stopped answering `tools/list` with the handshake a client sends,
/// this fails where the handler-level test above would not.
#[test]
fn tools_list_over_stdio_pipe_matches_the_published_tool_names() {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn send(w: &mut (impl AsyncWriteExt + Unpin), msg: serde_json::Value) {
        let mut line = serde_json::to_vec(&msg).unwrap();
        line.push(b'\n');
        w.write_all(&line).await.unwrap();
        w.flush().await.unwrap();
    }

    async fn response(r: &mut (impl AsyncBufReadExt + Unpin), id: u64) -> serde_json::Value {
        loop {
            let mut line = String::new();
            let n = r.read_line(&mut line).await.unwrap();
            assert!(
                n > 0,
                "server closed the pipe before answering request {id}"
            );
            let msg: serde_json::Value = serde_json::from_str(&line).unwrap();
            if msg.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                return msg;
            }
        }
    }

    run_test(async {
        let ctx = shared_app().await;
        let (client_end, server_end) = tokio::io::duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_end);
        let server = tokio::spawn(async move {
            let running = ctx
                .mcp_server()
                .serve((server_read, server_write))
                .await
                .expect("server should complete the handshake");
            let _ = running.waiting().await;
        });

        let (client_read, mut client_write) = tokio::io::split(client_end);
        let mut client_read = BufReader::new(client_read);

        send(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "trovato-mcp-test", "version": "0"}
                }
            }),
        )
        .await;
        let init = response(&mut client_read, 1).await;
        assert!(
            init.get("result").is_some(),
            "initialize must succeed, got {init}"
        );
        send(
            &mut client_write,
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        send(
            &mut client_write,
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )
        .await;
        let list = response(&mut client_read, 2).await;

        let tools = list["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list must return a tool array, got {list}"));
        let mut names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().expect("every tool has a name"))
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "create_item",
                "delete_item",
                "get_item",
                "list_categories",
                "list_content_types",
                "list_items",
                "list_tags",
                "run_gather",
                "search",
                "update_item",
            ]
        );
        for tool in tools {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "every tool takes a JSON object, got {tool}"
            );
        }

        // The schema form the server has always published: the parameter
        // type's title at the root, and an optional parameter as a nullable
        // single type rather than rmcp 3's `["integer", "null"]` union. See
        // `published_input_schema`.
        let list_items = tools
            .iter()
            .find(|t| t["name"] == "list_items")
            .expect("list_items is listed");
        let schema = &list_items["inputSchema"];
        assert_eq!(schema["title"], "ListItemsParams", "got {schema}");
        assert_eq!(
            schema["properties"]["page"]["type"], "integer",
            "got {schema}"
        );
        assert_eq!(
            schema["properties"]["page"]["nullable"], true,
            "got {schema}"
        );

        drop(client_write);
        drop(client_read);
        server.await.unwrap();
    });
}

// =============================================================================
// S4 — the publish permission on the MCP surface
//
// `create_item` and `update_item` passed `params.status` straight through, so an
// MCP client holding `create content` could put an item on the live site. The
// gate lives in `ItemService`, so the MCP tools inherit it; these pin that the
// refusal reaches the client as a permission error rather than as "not found"
// (which is how an item-level denial is reported, to avoid leaking existence)
// or as a generic internal error.
// =============================================================================

/// `unprivileged_user_ctx` plus the named permissions, which is a real user id
/// with a permission set of this test's choosing.
fn user_with(ctx: &common::TestContext, permissions: &[&str]) -> trovato_kernel::tap::UserContext {
    let mut user = ctx.unprivileged_user_ctx.clone();
    for permission in permissions {
        user.permissions.push((*permission).to_string());
    }
    user
}

#[test]
fn create_asking_to_publish_is_refused_without_publish_content() {
    run_test(async {
        let ctx = shared_app().await;
        let creator = user_with(ctx, &["create content"]);

        let result = trovato_mcp::tools::items::create_item(
            &ctx.state,
            &creator,
            CreateItemParams {
                content_type: "page".to_string(),
                title: "MCP publish attempt".to_string(),
                status: Some(1),
                fields: None,
            },
        )
        .await;

        let err = result.expect_err("`status: 1` without `publish content` must be refused");
        assert!(
            err.to_string().contains("permission denied"),
            "the client should be told it is a permission problem, got {err}"
        );

        let landed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM item WHERE title = 'MCP publish attempt')",
        )
        .fetch_one(ctx.state.db())
        .await
        .unwrap();
        assert!(!landed, "the refused create must not have landed");
    });
}

#[test]
fn create_asking_to_publish_succeeds_with_publish_content() {
    run_test(async {
        let ctx = shared_app().await;
        let publisher = user_with(ctx, &["create content", "publish content"]);

        let result = trovato_mcp::tools::items::create_item(
            &ctx.state,
            &publisher,
            CreateItemParams {
                content_type: "page".to_string(),
                title: "MCP publish allowed".to_string(),
                status: Some(1),
                fields: None,
            },
        )
        .await
        .expect("a holder of `publish content` may publish");

        let text = extract_text(&result);
        let created: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(created["status"], 1);

        let id: uuid::Uuid = serde_json::from_value(created["id"].clone()).unwrap();
        Item::delete(ctx.state.db(), id).await.ok();
    });
}

#[test]
fn a_create_that_says_nothing_about_status_is_a_draft_without_the_permission() {
    run_test(async {
        let ctx = shared_app().await;
        let creator = user_with(ctx, &["create content"]);

        let result = trovato_mcp::tools::items::create_item(
            &ctx.state,
            &creator,
            CreateItemParams {
                content_type: "page".to_string(),
                title: "MCP silent create".to_string(),
                status: None,
                fields: None,
            },
        )
        .await
        .expect("saying nothing is not a refusal");

        // `Item::create` defaults a missing status to published, so this is the
        // case where the gate has to answer rather than pass the silence on.
        let created: serde_json::Value =
            serde_json::from_str(&extract_text(&result)).expect("valid JSON");
        assert_eq!(
            created["status"], 0,
            "a creator without the permission drafts"
        );

        let id: uuid::Uuid = serde_json::from_value(created["id"].clone()).unwrap();
        Item::delete(ctx.state.db(), id).await.ok();
    });
}

#[test]
fn update_changing_the_status_is_refused_without_publish_content() {
    run_test(async {
        let ctx = shared_app().await;

        let item = Item::create(
            ctx.state.db(),
            CreateItem {
                item_type: "page".to_string(),
                title: "MCP published page".to_string(),
                author_id: ctx.admin_user.id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: Some(LIVE_STAGE_ID),
                language: None,
                log: Some("test".to_string()),
            },
        )
        .await
        .expect("create item");

        let editor = user_with(ctx, &["edit any content"]);

        let err = trovato_mcp::tools::items::update_item(
            &ctx.state,
            &editor,
            UpdateItemParams {
                id: item.id.to_string(),
                title: None,
                status: Some(0),
                fields: None,
                log: None,
            },
        )
        .await
        .expect_err("unpublishing without `publish content` must be refused");
        assert!(
            err.to_string().contains("permission denied"),
            "a gate refusal must not be reported as 'not found', got {err}"
        );

        let status: i16 = sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
            .bind(item.id)
            .fetch_one(ctx.state.db())
            .await
            .unwrap();
        assert_eq!(status, 1, "the page must still be published");

        // The same edit that leaves the status alone still saves.
        trovato_mcp::tools::items::update_item(
            &ctx.state,
            &editor,
            UpdateItemParams {
                id: item.id.to_string(),
                title: Some("MCP edited page".to_string()),
                status: None,
                fields: None,
                log: None,
            },
        )
        .await
        .expect("an ordinary edit is untouched by the publish gate");

        Item::delete(ctx.state.db(), item.id).await.ok();
    });
}

// =============================================================================
// Problem 7 — `list_items` returned rows, not answers.
//
// It forced `status >= 1` for a non-administrator and then handed back
// `list_filtered` unchanged: no stage filter, no `tap_item_access`, no role
// check. `get_item` in the same file already routes through
// `load_for_view_filtered`, so the two tools disagreed about the same item — one
// would hand over a published item on an internal stage that the other reported
// as missing. `total` stays the pre-filter count, as the REST list endpoint
// already documents.
// =============================================================================

/// A fixture content type of its own, so the assertions can be about exactly
/// two items rather than about whatever else the database holds.
const ACCESS_PROBE: &str = "mcp_access_probe";

async fn ensure_access_probe_type(ctx: &common::TestContext) {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'MCP Access Probe', '', true, 'Title', 'mcp_test', '{\"fields\": []}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ACCESS_PROBE)
    .execute(ctx.state.db())
    .await
    .expect("seed the probe type");

    ctx.state
        .content_types()
        .replace(
            ACCESS_PROBE,
            "MCP Access Probe",
            None,
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();
}

/// A stage of this test's own. `visibility: None` takes the column default,
/// which is internal.
async fn internal_stage(ctx: &common::TestContext) -> uuid::Uuid {
    use trovato_kernel::models::stage::{CreateStage, Stage};

    let suffix = &uuid::Uuid::now_v7().simple().to_string()[..8];
    Stage::create(
        ctx.state.db(),
        CreateStage {
            label: format!("MCP access {suffix}"),
            machine_name: format!("mcp_access_{suffix}"),
            description: None,
            visibility: None,
            is_default: None,
            weight: None,
        },
    )
    .await
    .expect("create an internal stage")
    .id
}

#[test]
fn list_items_hides_what_get_item_would_refuse() {
    run_test(async {
        let ctx = shared_app().await;
        ensure_access_probe_type(ctx).await;

        // The probe type is this test's own, and `total` below is asserted
        // exactly, so start from an empty set: a previous failing run of this
        // test leaves its two items behind.
        sqlx::query("DELETE FROM item WHERE type = $1")
            .bind(ACCESS_PROBE)
            .execute(ctx.state.db())
            .await
            .expect("clear the probe type");

        // Both published; one on an internal stage.
        let stage = internal_stage(ctx).await;
        let hidden = Item::create(
            ctx.state.db(),
            CreateItem {
                item_type: ACCESS_PROBE.to_string(),
                title: "MCP internal stage item".to_string(),
                author_id: ctx.admin_user.id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: Some(stage),
                language: None,
                log: Some("access probe".to_string()),
            },
        )
        .await
        .expect("create the internal-stage item");

        let shown = Item::create(
            ctx.state.db(),
            CreateItem {
                item_type: ACCESS_PROBE.to_string(),
                title: "MCP live stage item".to_string(),
                author_id: ctx.admin_user.id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: Some(LIVE_STAGE_ID),
                language: None,
                log: Some("access probe".to_string()),
            },
        )
        .await
        .expect("create the live-stage item");

        let reader = user_with(ctx, &["access content"]);

        // `get_item` already refuses the internal-stage item.
        let refused = trovato_mcp::tools::items::get_item(
            &ctx.state,
            &reader,
            GetItemParams {
                id: hidden.id.to_string(),
            },
        )
        .await;
        assert!(
            refused.is_err(),
            "get_item must refuse the internal-stage item, so list_items has to agree"
        );

        let result = trovato_mcp::tools::items::list_items(
            &ctx.state,
            &reader,
            ListItemsParams {
                content_type: Some(ACCESS_PROBE.to_string()),
                status: None,
                author_id: None,
                page: Some(1),
                per_page: Some(50),
                limit: None,
                offset: None,
            },
        )
        .await
        .expect("list_items should succeed");

        let json: serde_json::Value =
            serde_json::from_str(&extract_text(&result)).expect("valid JSON");
        let body = json.to_string();
        assert!(
            body.contains("MCP live stage item"),
            "the published live item must still be listed, got {body}"
        );
        assert!(
            !body.contains("MCP internal stage item"),
            "the item get_item refuses must not be listed either, got {body}"
        );
        assert_eq!(
            json["total"], 2,
            "total stays the pre-filter count, as the REST list endpoint documents"
        );

        // An administrator still sees both.
        let result = trovato_mcp::tools::items::list_items(
            &ctx.state,
            &ctx.admin_user_ctx,
            ListItemsParams {
                content_type: Some(ACCESS_PROBE.to_string()),
                status: None,
                author_id: None,
                page: Some(1),
                per_page: Some(50),
                limit: None,
                offset: None,
            },
        )
        .await
        .expect("list_items should succeed");
        let body = extract_text(&result);
        assert!(
            body.contains("MCP internal stage item") && body.contains("MCP live stage item"),
            "an administrator sees both, got {body}"
        );

        Item::delete(ctx.state.db(), hidden.id).await.ok();
        Item::delete(ctx.state.db(), shown.id).await.ok();
    });
}

/// The category tools follow the category API's visibility rule (B189): an
/// owned vocabulary, here of an owner type no plugin declares, is neither
/// listed to nor readable by a caller who may not see its owner, and reads
/// exactly like a vocabulary that does not exist; an administrator sees it.
#[test]
fn the_category_tools_hide_an_owned_vocabulary() {
    run_test(async {
        let ctx = shared_app().await;
        let id = format!("b189_mcp_{}", &uuid::Uuid::now_v7().simple().to_string()[20..]);
        sqlx::query(
            "INSERT INTO category (id, label, owner_type, owner_id) \
             VALUES ($1, 'Owned', 'b189_nobody', 'o1')",
        )
        .bind(&id)
        .execute(ctx.state.db())
        .await
        .unwrap();
        ctx.state
            .categories()
            .create_tag(trovato_kernel::models::CreateTag {
                category_id: id.clone(),
                label: "Hidden term".to_string(),
                description: None,
                slug: None,
                weight: Some(0),
                parent_ids: None,
            })
            .await
            .unwrap();

        let reader = user_with(ctx, &["access content"]);
        let listed = |user: trovato_kernel::tap::UserContext| {
            let state = ctx.state.clone();
            async move {
                let result = trovato_mcp::tools::categories::list_categories(&state, &user)
                    .await
                    .unwrap();
                extract_text(&result)
            }
        };
        let tags = |user: trovato_kernel::tap::UserContext, category: String| {
            let state = ctx.state.clone();
            async move {
                let params: trovato_mcp::server::ListTagsParams =
                    serde_json::from_value(serde_json::json!({ "category_id": category }))
                        .unwrap();
                let result = trovato_mcp::tools::categories::list_tags(&state, &user, params)
                    .await
                    .unwrap();
                extract_text(&result)
            }
        };

        let text = listed(reader.clone()).await;
        assert!(!text.contains(&id), "the reader is not listed it: {text}");
        assert!(text.contains("\"tags\""), "the reader still lists tags");
        let hidden = tags(reader.clone(), id.clone()).await;
        assert!(!hidden.contains("Hidden term"), "{hidden}");
        assert_eq!(
            hidden,
            tags(reader.clone(), "b189_no_such_vocabulary".to_string()).await,
            "a hidden vocabulary reads like a missing one"
        );

        let text = listed(ctx.admin_user_ctx.clone()).await;
        assert!(text.contains(&id), "the administrator is listed it");
        let shown = tags(ctx.admin_user_ctx.clone(), id.clone()).await;
        assert!(shown.contains("Hidden term"), "{shown}");

        sqlx::query("DELETE FROM category WHERE id = $1")
            .bind(&id)
            .execute(ctx.state.db())
            .await
            .unwrap();
    });
}
