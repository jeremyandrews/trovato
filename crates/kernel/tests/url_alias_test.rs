#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for URL alias system.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use trovato_kernel::models::{CreateUrlAlias, UrlAlias};

#[tokio::test]
async fn test_alias_create_and_lookup() {
    let app = TestApp::new().await;

    // Create an alias
    let input = CreateUrlAlias {
        source: "/item/test-123".to_string(),
        alias: "/about-us".to_string(),
        language: None,
        stage_id: None,
    };

    let created = UrlAlias::create(&app.db, input).await.unwrap();
    assert_eq!(created.source, "/item/test-123");
    assert_eq!(created.alias, "/about-us");
    assert_eq!(created.language, "en");
    assert_eq!(created.stage_id, "live");

    // Look up by alias
    let found = UrlAlias::find_by_alias(&app.db, "/about-us")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.source, "/item/test-123");

    // Look up by source
    let aliases = UrlAlias::find_by_source(&app.db, "/item/test-123")
        .await
        .unwrap();
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].alias, "/about-us");

    // Cleanup
    UrlAlias::delete(&app.db, created.id).await.unwrap();
}

#[tokio::test]
async fn test_alias_canonical_url() {
    let app = TestApp::new().await;

    // Create an alias
    let input = CreateUrlAlias {
        source: "/item/canonical-test".to_string(),
        alias: "/my-page".to_string(),
        language: None,
        stage_id: None,
    };

    let alias1 = UrlAlias::create(&app.db, input).await.unwrap();

    // Get canonical alias
    let canonical = UrlAlias::get_canonical_alias(&app.db, "/item/canonical-test")
        .await
        .unwrap();
    assert_eq!(canonical, Some("/my-page".to_string()));

    // No alias for non-existent source
    let no_alias = UrlAlias::get_canonical_alias(&app.db, "/item/non-existent")
        .await
        .unwrap();
    assert_eq!(no_alias, None);

    // Cleanup
    UrlAlias::delete(&app.db, alias1.id).await.unwrap();
}

#[tokio::test]
async fn test_alias_multiple_for_same_source() {
    let app = TestApp::new().await;

    // Generate unique source path for this test run
    let source = format!("/item/multi-test-{}", uuid::Uuid::new_v4());

    // Create first alias
    let input1 = CreateUrlAlias {
        source: source.clone(),
        alias: format!("/old-page-{}", uuid::Uuid::new_v4()),
        language: None,
        stage_id: None,
    };
    let alias1 = UrlAlias::create(&app.db, input1.clone()).await.unwrap();

    // Small delay to ensure different created timestamp (using millis in UUIDv7)
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Create second alias (newer)
    let input2 = CreateUrlAlias {
        source: source.clone(),
        alias: format!("/new-page-{}", uuid::Uuid::new_v4()),
        language: None,
        stage_id: None,
    };
    let alias2 = UrlAlias::create(&app.db, input2.clone()).await.unwrap();

    // Most recent alias should be canonical
    let canonical = UrlAlias::get_canonical_alias(&app.db, &source)
        .await
        .unwrap();
    assert_eq!(canonical, Some(input2.alias.clone()));

    // Both should be findable by source
    let aliases = UrlAlias::find_by_source(&app.db, &source).await.unwrap();
    assert_eq!(aliases.len(), 2);
    // Most recent should be first (ordered by created DESC, id DESC)
    assert_eq!(aliases[0].alias, input2.alias);
    assert_eq!(aliases[1].alias, input1.alias);

    // Cleanup
    UrlAlias::delete(&app.db, alias1.id).await.unwrap();
    UrlAlias::delete(&app.db, alias2.id).await.unwrap();
}

#[tokio::test]
async fn test_alias_update() {
    let app = TestApp::new().await;

    // Create an alias
    let input = CreateUrlAlias {
        source: "/item/update-test".to_string(),
        alias: "/original-path".to_string(),
        language: None,
        stage_id: None,
    };
    let created = UrlAlias::create(&app.db, input).await.unwrap();

    // Update the alias
    let update = trovato_kernel::models::UpdateUrlAlias {
        source: None,
        alias: Some("/updated-path".to_string()),
        language: None,
        stage_id: None,
    };
    let updated = UrlAlias::update(&app.db, created.id, update)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.alias, "/updated-path");
    assert_eq!(updated.source, "/item/update-test"); // Source unchanged

    // Old alias should not exist
    let old = UrlAlias::find_by_alias(&app.db, "/original-path")
        .await
        .unwrap();
    assert!(old.is_none());

    // New alias should work
    let found = UrlAlias::find_by_alias(&app.db, "/updated-path")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, created.id);

    // Cleanup
    UrlAlias::delete(&app.db, created.id).await.unwrap();
}

#[tokio::test]
async fn test_alias_delete_by_source() {
    let app = TestApp::new().await;

    // Create multiple aliases for the same source
    let input1 = CreateUrlAlias {
        source: "/item/delete-source-test".to_string(),
        alias: "/page-1".to_string(),
        language: None,
        stage_id: None,
    };
    UrlAlias::create(&app.db, input1).await.unwrap();

    let input2 = CreateUrlAlias {
        source: "/item/delete-source-test".to_string(),
        alias: "/page-2".to_string(),
        language: None,
        stage_id: None,
    };
    UrlAlias::create(&app.db, input2).await.unwrap();

    // Verify both exist
    let aliases = UrlAlias::find_by_source(&app.db, "/item/delete-source-test")
        .await
        .unwrap();
    assert_eq!(aliases.len(), 2);

    // Delete all aliases for this source
    let deleted = UrlAlias::delete_by_source(&app.db, "/item/delete-source-test")
        .await
        .unwrap();
    assert_eq!(deleted, 2);

    // Verify none exist
    let aliases = UrlAlias::find_by_source(&app.db, "/item/delete-source-test")
        .await
        .unwrap();
    assert!(aliases.is_empty());
}

#[tokio::test]
async fn test_alias_upsert() {
    let app = TestApp::new().await;

    // Upsert creates when no alias exists
    let alias1 =
        UrlAlias::upsert_for_source(&app.db, "/item/upsert-test", "/first-path", "live", "en")
            .await
            .unwrap();
    assert_eq!(alias1.alias, "/first-path");

    // Upsert updates existing alias
    let alias2 =
        UrlAlias::upsert_for_source(&app.db, "/item/upsert-test", "/second-path", "live", "en")
            .await
            .unwrap();
    assert_eq!(alias2.id, alias1.id); // Same record
    assert_eq!(alias2.alias, "/second-path");

    // Only one alias should exist
    let aliases = UrlAlias::find_by_source(&app.db, "/item/upsert-test")
        .await
        .unwrap();
    assert_eq!(aliases.len(), 1);

    // Cleanup
    UrlAlias::delete(&app.db, alias1.id).await.unwrap();
}

#[tokio::test]
async fn test_alias_list_and_count() {
    let app = TestApp::new().await;

    // Generate unique aliases for this test run
    let alias1_path = format!("/list-page-1-{}", uuid::Uuid::new_v4());
    let alias2_path = format!("/list-page-2-{}", uuid::Uuid::new_v4());

    // Create test aliases
    let input1 = CreateUrlAlias {
        source: format!("/item/list-test-1-{}", uuid::Uuid::new_v4()),
        alias: alias1_path.clone(),
        language: None,
        stage_id: None,
    };
    let alias1 = UrlAlias::create(&app.db, input1).await.unwrap();

    let input2 = CreateUrlAlias {
        source: format!("/item/list-test-2-{}", uuid::Uuid::new_v4()),
        alias: alias2_path.clone(),
        language: None,
        stage_id: None,
    };
    let alias2 = UrlAlias::create(&app.db, input2).await.unwrap();

    // Count should be at least 2 (our aliases exist)
    let count = UrlAlias::count_all(&app.db).await.unwrap();
    assert!(count >= 2, "expected at least 2 aliases, got {count}");

    // List should include our aliases
    let all = UrlAlias::list_all(&app.db, 100, 0).await.unwrap();
    assert!(
        all.iter().any(|a| a.alias == alias1_path),
        "alias1 not found in list"
    );
    assert!(
        all.iter().any(|a| a.alias == alias2_path),
        "alias2 not found in list"
    );

    // Cleanup
    UrlAlias::delete(&app.db, alias1.id).await.unwrap();
    UrlAlias::delete(&app.db, alias2.id).await.unwrap();
}

// =============================================================================
// End-to-End HTTP Tests
// =============================================================================

/// Test that the path alias middleware rewrites alias URLs to source paths.
/// We verify the rewrite by checking that requesting an alias path returns
/// the same status as requesting the source path directly.
#[tokio::test]
async fn test_e2e_middleware_rewrites_alias_to_source() {
    let app = TestApp::new().await;

    // Create an item directly in the database to avoid content type issues
    let item_id = uuid::Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO item (id, type, title, author_id, status, promote, sticky, fields, created, changed, stage_id)
        VALUES ($1, 'page', 'Test Page for Alias', $2, 1, 0, 0, '{}', $3, $3, 'live')
        "#,
    )
    .bind(item_id)
    .bind(uuid::Uuid::nil())
    .bind(now)
    .execute(&app.db)
    .await
    .expect("failed to create test item");

    // Create an alias for this item
    let alias_path = format!("/test-alias-page-{}", uuid::Uuid::new_v4());
    let source_path = format!("/item/{item_id}");
    let alias = UrlAlias::create(
        &app.db,
        CreateUrlAlias {
            source: source_path.clone(),
            alias: alias_path.clone(),
            language: None,
            stage_id: None,
        },
    )
    .await
    .unwrap();

    // Request the direct source path first to see what status it returns
    let direct_response = app
        .request(Request::get(&source_path).body(Body::empty()).unwrap())
        .await;
    let direct_status = direct_response.status();

    // Request the alias path - middleware should rewrite to source
    let alias_response = app
        .request(Request::get(&alias_path).body(Body::empty()).unwrap())
        .await;

    // Both paths should return the same status (middleware rewrote alias to source)
    assert_eq!(
        alias_response.status(),
        direct_status,
        "alias path should return same status as source path after middleware rewrite"
    );

    // Cleanup
    UrlAlias::delete(&app.db, alias.id).await.unwrap();
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
}

/// Test that non-aliased paths still work (middleware passes through).
#[tokio::test]
async fn test_e2e_middleware_passthrough_for_non_alias() {
    let app = TestApp::new().await;

    // Request a path that has no alias - should get 404 (no matching route)
    let response = app
        .request(
            Request::get("/nonexistent-page-xyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    // Should get 404 since no alias exists and no route matches
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "non-aliased path should return 404"
    );
}

/// Test that system paths are skipped by middleware (no DB lookup).
#[tokio::test]
async fn test_e2e_middleware_skips_system_paths() {
    let app = TestApp::new().await;

    // /health should always return 200
    let response = app
        .request(Request::get("/health").body(Body::empty()).unwrap())
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/health should return 200"
    );
}

/// Test admin alias list page requires authentication.
#[tokio::test]
async fn test_e2e_admin_alias_list_requires_auth() {
    let app = TestApp::new().await;

    // Request without login - should redirect to login
    let response = app
        .request(
            Request::get("/admin/structure/aliases")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "admin page should redirect when not logged in"
    );
}

/// Test that alias is preserved through query strings.
#[tokio::test]
async fn test_e2e_middleware_preserves_query_string() {
    let app = TestApp::new().await;

    // Create an item directly in the database
    let item_id = uuid::Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO item (id, type, title, author_id, status, promote, sticky, fields, created, changed, stage_id)
        VALUES ($1, 'page', 'Query String Test Page', $2, 1, 0, 0, '{}', $3, $3, 'live')
        "#,
    )
    .bind(item_id)
    .bind(uuid::Uuid::nil())
    .bind(now)
    .execute(&app.db)
    .await
    .expect("failed to create test item");

    // Create an alias
    let alias_path = format!("/query-test-{}", uuid::Uuid::new_v4());
    let source_path = format!("/item/{item_id}");
    let alias = UrlAlias::create(
        &app.db,
        CreateUrlAlias {
            source: source_path.clone(),
            alias: alias_path.clone(),
            language: None,
            stage_id: None,
        },
    )
    .await
    .unwrap();

    // Request source path with query string to get baseline
    let direct_response = app
        .request(
            Request::get(format!("{source_path}?foo=bar&baz=qux"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let direct_status = direct_response.status();

    // Request alias with query string
    let alias_response = app
        .request(
            Request::get(format!("{alias_path}?foo=bar&baz=qux"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    // Both should return the same status (query string preserved through rewrite)
    assert_eq!(
        alias_response.status(),
        direct_status,
        "alias with query string should return same status as source with query string"
    );

    // Cleanup
    UrlAlias::delete(&app.db, alias.id).await.unwrap();
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
}
