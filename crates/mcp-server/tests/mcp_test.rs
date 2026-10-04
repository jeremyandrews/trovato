// Tests are allowed to use unwrap/expect freely.
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for the Trovato MCP server.
//!
//! Tests exercise tool and resource handlers directly (no STDIO transport)
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
            },
        )
        .await
        .expect("list_items should succeed");

        let text = extract_text(&result);
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(json["items"].is_array(), "response should have items array");
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
                limit: Some(5),
                offset: None,
            },
        )
        .await
        .expect("search should succeed");

        let text = extract_text(&result);
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(json["results"].is_array());
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
        assert!(json.is_array(), "should return array of content types");
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
        assert!(json.is_array());
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
            ReadResourceRequestParams {
                uri: "trovato://site-config".into(),
                meta: None,
            },
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
            ReadResourceRequestParams {
                uri: "trovato://content-types".into(),
                meta: None,
            },
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
            ReadResourceRequestParams {
                uri: "trovato://recent-items".into(),
                meta: None,
            },
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
            ReadResourceRequestParams {
                uri: "trovato://content-type/page".into(),
                meta: None,
            },
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
            if let RawContent::Text(t) = &c.raw {
                Some(t.text.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
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
