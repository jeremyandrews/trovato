//! Integration tests for StageAwareConfigStorage.
//!
//! Tests the stage-aware config storage implementation against a real database.

use uuid::Uuid;

mod common;
use common::TestApp;

use trovato_kernel::ConfigEntity;
use trovato_kernel::config_storage::entity_types;
use trovato_kernel::models::ItemType;

/// Test that live stage returns direct storage behavior.
#[tokio::test]
async fn stage_aware_live_returns_direct() {
    let app = TestApp::new().await;

    let live_storage = app.state.config_storage_for_stage("live");

    // Should be able to load the seeded "page" type
    let entity = live_storage
        .load(entity_types::ITEM_TYPE, "page")
        .await
        .expect("failed to load");

    assert!(entity.is_some(), "page type should exist in live");
}

/// Test creating a config entity in a stage.
#[tokio::test]
async fn stage_aware_create_in_stage() {
    let app = TestApp::new().await;

    let stage_id = format!("test-stage-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let stage_storage = app.state.config_storage_for_stage(&stage_id);
    let live_storage = app.state.config_storage_for_stage("live");

    // Create a unique type name
    let type_name = format!("st{}", &Uuid::now_v7().simple().to_string()[..8]);

    let item_type = ItemType {
        type_name: type_name.clone(),
        label: "Staged Type".to_string(),
        description: Some("Only in stage".to_string()),
        has_title: true,
        title_label: None,
        plugin: "test".to_string(),
        settings: serde_json::json!({}),
    };

    // Save in stage
    stage_storage
        .save(&ConfigEntity::ItemType(item_type))
        .await
        .expect("failed to save in stage");

    // Should exist in stage
    let in_stage = stage_storage
        .load(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to load from stage");

    assert!(in_stage.is_some(), "type should exist in stage");
    let staged_type = in_stage.unwrap().as_item_type().unwrap().clone();
    assert_eq!(staged_type.label, "Staged Type");

    // Should NOT exist in live
    let in_live = live_storage
        .load(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to load from live");

    assert!(in_live.is_none(), "type should NOT exist in live");

    // Clean up: delete stage-specific data
    cleanup_stage_data(&app, &stage_id).await;
}

/// Test that staged changes override live.
#[tokio::test]
async fn stage_aware_staged_overrides_live() {
    let app = TestApp::new().await;

    let stage_id = format!("override-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let stage_storage = app.state.config_storage_for_stage(&stage_id);
    let live_storage = app.state.config_storage_for_stage("live");

    // Create a type in live first
    let type_name = format!("ovr{}", &Uuid::now_v7().simple().to_string()[..8]);

    let live_type = ItemType {
        type_name: type_name.clone(),
        label: "Live Label".to_string(),
        description: None,
        has_title: true,
        title_label: None,
        plugin: "test".to_string(),
        settings: serde_json::json!({}),
    };

    live_storage
        .save(&ConfigEntity::ItemType(live_type))
        .await
        .expect("failed to save in live");

    // Modify in stage
    let staged_type = ItemType {
        type_name: type_name.clone(),
        label: "Staged Label".to_string(), // Changed
        description: Some("Staged description".to_string()),
        has_title: true,
        title_label: None,
        plugin: "test".to_string(),
        settings: serde_json::json!({}),
    };

    stage_storage
        .save(&ConfigEntity::ItemType(staged_type))
        .await
        .expect("failed to save in stage");

    // Reading from stage should get staged version
    let from_stage = stage_storage
        .load(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to load")
        .expect("should exist");

    assert_eq!(from_stage.as_item_type().unwrap().label, "Staged Label");

    // Reading from live should still get live version
    let from_live = live_storage
        .load(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to load")
        .expect("should exist");

    assert_eq!(from_live.as_item_type().unwrap().label, "Live Label");

    // Clean up
    live_storage
        .delete(entity_types::ITEM_TYPE, &type_name)
        .await
        .ok();
    cleanup_stage_data(&app, &stage_id).await;
}

/// Test deleting in stage (marks for deletion, doesn't actually delete live).
#[tokio::test]
async fn stage_aware_delete_marks_for_deletion() {
    let app = TestApp::new().await;

    let stage_id = format!("del-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let stage_storage = app.state.config_storage_for_stage(&stage_id);
    let live_storage = app.state.config_storage_for_stage("live");

    // Create a type in live
    let type_name = format!("del{}", &Uuid::now_v7().simple().to_string()[..8]);

    let item_type = ItemType {
        type_name: type_name.clone(),
        label: "To Delete".to_string(),
        description: None,
        has_title: true,
        title_label: None,
        plugin: "test".to_string(),
        settings: serde_json::json!({}),
    };

    live_storage
        .save(&ConfigEntity::ItemType(item_type))
        .await
        .expect("failed to save in live");

    // Delete in stage
    stage_storage
        .delete(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to delete in stage");

    // Should NOT exist in stage view
    let in_stage = stage_storage
        .load(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to load");

    assert!(
        in_stage.is_none(),
        "type should not exist in stage after deletion"
    );

    // Should STILL exist in live
    let in_live = live_storage
        .load(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to load");

    assert!(in_live.is_some(), "type should still exist in live");

    // Clean up
    live_storage
        .delete(entity_types::ITEM_TYPE, &type_name)
        .await
        .ok();
    cleanup_stage_data(&app, &stage_id).await;
}

/// Test listing merges stage and live correctly.
#[tokio::test]
async fn stage_aware_list_merges_stage_and_live() {
    let app = TestApp::new().await;

    let stage_id = format!("list-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let stage_storage = app.state.config_storage_for_stage(&stage_id);
    let live_storage = app.state.config_storage_for_stage("live");

    // Create unique plugin name for this test
    let short_id = &Uuid::now_v7().simple().to_string()[..6];
    let plugin_name = format!("lp{short_id}");

    // Create two types in live
    for i in 1..=2 {
        let item_type = ItemType {
            type_name: format!("lt{short_id}_{i}"),
            label: format!("Live Type {i}"),
            description: None,
            has_title: true,
            title_label: None,
            plugin: plugin_name.clone(),
            settings: serde_json::json!({}),
        };

        live_storage
            .save(&ConfigEntity::ItemType(item_type))
            .await
            .expect("failed to save");
    }

    // Create a type only in stage
    let staged_type = ItemType {
        type_name: format!("lt{short_id}_staged"),
        label: "Staged Only".to_string(),
        description: None,
        has_title: true,
        title_label: None,
        plugin: plugin_name.clone(),
        settings: serde_json::json!({}),
    };

    stage_storage
        .save(&ConfigEntity::ItemType(staged_type))
        .await
        .expect("failed to save");

    // Delete one live type in stage
    stage_storage
        .delete(entity_types::ITEM_TYPE, &format!("lt{}_{}", short_id, 1))
        .await
        .expect("failed to delete");

    // List from stage should show:
    // - Live Type 2 (still in live, not deleted in stage)
    // - Staged Only (only in stage)
    // NOT Live Type 1 (deleted in stage)
    let from_stage = stage_storage
        .list(entity_types::ITEM_TYPE, None)
        .await
        .expect("failed to list");

    let stage_types: Vec<String> = from_stage
        .iter()
        .filter_map(|e| e.as_item_type())
        .filter(|t| t.plugin == plugin_name)
        .map(|t| t.type_name.clone())
        .collect();

    assert_eq!(stage_types.len(), 2, "should have 2 types in stage view");
    assert!(stage_types.contains(&format!("lt{}_{}", short_id, 2)));
    assert!(stage_types.contains(&format!("lt{short_id}_staged")));
    assert!(!stage_types.contains(&format!("lt{}_{}", short_id, 1)));

    // Clean up
    for i in 1..=2 {
        live_storage
            .delete(entity_types::ITEM_TYPE, &format!("lt{short_id}_{i}"))
            .await
            .ok();
    }
    cleanup_stage_data(&app, &stage_id).await;
}

/// Helper to clean up stage-specific data.
async fn cleanup_stage_data(app: &TestApp, stage_id: &str) {
    sqlx::query("DELETE FROM config_stage_association WHERE stage_id = $1")
        .bind(stage_id)
        .execute(&app.db)
        .await
        .ok();

    sqlx::query("DELETE FROM stage_deletion WHERE stage_id = $1")
        .bind(stage_id)
        .execute(&app.db)
        .await
        .ok();

    // Note: config_revision rows are not deleted here as they may be shared
    // In a real system, orphaned revisions would be cleaned up by a maintenance job
}
