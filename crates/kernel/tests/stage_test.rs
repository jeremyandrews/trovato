//! Integration tests for StageService.
//!
//! Tests the stage publishing framework against a real database.

use uuid::Uuid;

mod common;
use common::TestApp;

use trovato_kernel::{ConflictResolution, ConflictType, PublishPhase};

/// Test that publishing 'live' stage fails with appropriate error.
#[tokio::test]
async fn stage_publish_live_fails() {
    let app = TestApp::new().await;

    let result = app
        .stage()
        .publish("live")
        .await
        .expect("publish should return result");

    assert!(!result.success, "publishing 'live' stage should fail");
    assert_eq!(result.failed_phase, Some(PublishPhase::Items));
    assert!(result.error_message.is_some());
}

/// Test that publishing a stage with no changes succeeds with zero counts.
#[tokio::test]
async fn stage_publish_empty_stage() {
    let app = TestApp::new().await;

    // Use a unique stage ID that doesn't exist
    let stage_id = format!("test-stage-{}", &Uuid::now_v7().simple().to_string()[..8]);

    let result = app
        .stage()
        .publish(&stage_id)
        .await
        .expect("publish should succeed");

    assert!(result.success, "publishing empty stage should succeed");
    assert_eq!(result.items_published, 0);
    assert_eq!(result.items_deleted, 0);
}

/// Test publishing a stage with staged items moves them to live.
#[tokio::test]
async fn stage_publish_moves_items_to_live() {
    let app = TestApp::new().await;

    // Create a unique stage ID
    let stage_id = format!("pub-test-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Create a test item in the stage
    let item_id = Uuid::now_v7();
    let author_id = create_test_author(&app).await;
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id)
        VALUES ($1, NULL, 'page', 'Test Staged Item', $2, 1, $3, $3, 0, 0, '{}', $4)
        "#,
    )
    .bind(item_id)
    .bind(author_id)
    .bind(now)
    .bind(&stage_id)
    .execute(&app.db)
    .await
    .expect("failed to create test item");

    // Verify item is in the stage
    let staged_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE stage_id = $1")
        .bind(&stage_id)
        .fetch_one(&app.db)
        .await
        .expect("failed to count");

    assert_eq!(staged_count, 1, "should have 1 staged item");

    // Publish the stage
    let result = app
        .stage()
        .publish(&stage_id)
        .await
        .expect("publish should succeed");

    assert!(result.success, "publish should succeed");

    // Verify item was moved to live
    let live_item: Option<String> = sqlx::query_scalar("SELECT stage_id FROM item WHERE id = $1")
        .bind(item_id)
        .fetch_optional(&app.db)
        .await
        .expect("failed to query");

    assert_eq!(
        live_item,
        Some("live".to_string()),
        "item should be in live stage"
    );

    // Clean up
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
}

/// Test that has_changes returns true when stage has items.
#[tokio::test]
async fn stage_has_changes_with_items() {
    let app = TestApp::new().await;

    let stage_id = format!("chg-test-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Initially should have no changes
    let has_changes = app
        .stage()
        .has_changes(&stage_id)
        .await
        .expect("should check");
    assert!(!has_changes, "empty stage should have no changes");

    // Create a test item in the stage
    let item_id = Uuid::now_v7();
    let author_id = create_test_author(&app).await;
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id)
        VALUES ($1, NULL, 'page', 'Test Item', $2, 1, $3, $3, 0, 0, '{}', $4)
        "#,
    )
    .bind(item_id)
    .bind(author_id)
    .bind(now)
    .bind(&stage_id)
    .execute(&app.db)
    .await
    .expect("failed to create test item");

    // Now should have changes
    let has_changes = app
        .stage()
        .has_changes(&stage_id)
        .await
        .expect("should check");
    assert!(has_changes, "stage with item should have changes");

    // Clean up
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
}

/// Test that has_changes returns true when stage has deletion records.
#[tokio::test]
async fn stage_has_changes_with_deletions() {
    let app = TestApp::new().await;

    let stage_id = format!("del-test-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Create a deletion record
    let item_id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO stage_deletion (stage_id, entity_type, entity_id, deleted_at) VALUES ($1, 'item', $2, $3)"
    )
    .bind(&stage_id)
    .bind(item_id.to_string())
    .bind(now)
    .execute(&app.db)
    .await
    .expect("failed to create deletion record");

    // Should have changes due to deletion
    let has_changes = app
        .stage()
        .has_changes(&stage_id)
        .await
        .expect("should check");
    assert!(has_changes, "stage with deletion should have changes");

    // Clean up
    sqlx::query("DELETE FROM stage_deletion WHERE stage_id = $1")
        .bind(&stage_id)
        .execute(&app.db)
        .await
        .ok();
}

/// Test that live stage never has changes.
#[tokio::test]
async fn stage_live_has_no_changes() {
    let app = TestApp::new().await;

    let has_changes = app.stage().has_changes("live").await.expect("should check");
    assert!(!has_changes, "live stage should never report changes");
}

/// Test that publish processes deletion records.
#[tokio::test]
async fn stage_publish_processes_deletions() {
    let app = TestApp::new().await;

    let stage_id = format!("pdel-test-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Create an item to be deleted
    let item_id = Uuid::now_v7();
    let author_id = create_test_author(&app).await;
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id)
        VALUES ($1, NULL, 'page', 'Item to Delete', $2, 1, $3, $3, 0, 0, '{}', 'live')
        "#,
    )
    .bind(item_id)
    .bind(author_id)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("failed to create test item");

    // Create a deletion record for this item
    sqlx::query(
        "INSERT INTO stage_deletion (stage_id, entity_type, entity_id, deleted_at) VALUES ($1, 'item', $2, $3)"
    )
    .bind(&stage_id)
    .bind(item_id.to_string())
    .bind(now)
    .execute(&app.db)
    .await
    .expect("failed to create deletion record");

    // Publish the stage
    let result = app
        .stage()
        .publish(&stage_id)
        .await
        .expect("publish should succeed");
    assert!(result.success);

    // Verify item was deleted
    let item_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM item WHERE id = $1)")
        .bind(item_id)
        .fetch_one(&app.db)
        .await
        .expect("failed to check");

    assert!(!item_exists, "item should be deleted after publish");

    // Verify deletion record was cleaned up
    let deletion_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM stage_deletion WHERE stage_id = $1 AND entity_id = $2)",
    )
    .bind(&stage_id)
    .bind(item_id.to_string())
    .fetch_one(&app.db)
    .await
    .expect("failed to check");

    assert!(
        !deletion_exists,
        "deletion record should be cleaned up after publish"
    );
}

/// Helper to create a test author user and return their ID.
async fn create_test_author(app: &TestApp) -> Uuid {
    let author_id = Uuid::now_v7();
    // Use full UUID to avoid collisions in parallel tests
    let username = format!("author_{}", author_id.simple());

    // Use INSERT without ON CONFLICT to get exact ID back
    sqlx::query(
        r#"
        INSERT INTO users (id, name, pass, mail, status, is_admin)
        VALUES ($1, $2, 'test', $3, 1, false)
        "#,
    )
    .bind(author_id)
    .bind(&username)
    .bind(format!("{username}@test.com"))
    .execute(&app.db)
    .await
    .expect("failed to create author");

    author_id
}

// ============================================================================
// Conflict Detection Tests
// ============================================================================

/// Test that detect_conflicts returns empty list for empty stage.
#[tokio::test]
async fn conflict_detection_empty_stage() {
    let app = TestApp::new().await;
    let stage_id = format!("conf-empty-{}", &Uuid::now_v7().simple().to_string()[..8]);

    let conflicts = app
        .stage()
        .detect_conflicts(&stage_id)
        .await
        .expect("detect should work");
    assert!(conflicts.is_empty(), "empty stage should have no conflicts");
}

/// Test that detect_conflicts returns empty for live stage.
#[tokio::test]
async fn conflict_detection_live_stage() {
    let app = TestApp::new().await;

    let conflicts = app
        .stage()
        .detect_conflicts("live")
        .await
        .expect("detect should work");
    assert!(conflicts.is_empty(), "live stage should have no conflicts");
}

/// Test cross-stage conflict detection for config entities.
#[tokio::test]
async fn conflict_detection_cross_stage_config() {
    let app = TestApp::new().await;

    let stage_a = format!("conf-a-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let stage_b = format!("conf-b-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Create a shared config entity revision
    let revision_id_a = Uuid::now_v7();
    let revision_id_b = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    let author_id = create_test_author(&app).await;

    // Create config revisions for both stages pointing to same entity
    sqlx::query(
        r#"
        INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id)
        VALUES ($1, 'item_type', 'shared_type', '{}', $2, $3)
        "#,
    )
    .bind(revision_id_a)
    .bind(now)
    .bind(author_id)
    .execute(&app.db)
    .await
    .expect("failed to create revision A");

    sqlx::query(
        r#"
        INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id)
        VALUES ($1, 'item_type', 'shared_type', '{}', $2, $3)
        "#,
    )
    .bind(revision_id_b)
    .bind(now + 1)
    .bind(author_id)
    .execute(&app.db)
    .await
    .expect("failed to create revision B");

    // Associate both stages with the same entity
    sqlx::query(
        "INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id) VALUES ($1, 'item_type', 'shared_type', $2)"
    )
    .bind(&stage_a)
    .bind(revision_id_a)
    .execute(&app.db)
    .await
    .expect("failed to create stage association A");

    sqlx::query(
        "INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id) VALUES ($1, 'item_type', 'shared_type', $2)"
    )
    .bind(&stage_b)
    .bind(revision_id_b)
    .execute(&app.db)
    .await
    .expect("failed to create stage association B");

    // Detect conflicts for stage A
    let conflicts = app
        .stage()
        .detect_conflicts(&stage_a)
        .await
        .expect("detect should work");

    assert_eq!(conflicts.len(), 1, "should detect 1 cross-stage conflict");
    assert_eq!(conflicts[0].entity_type, "item_type");
    assert_eq!(conflicts[0].entity_id, "shared_type");

    if let ConflictType::CrossStage { other_stages } = &conflicts[0].conflict_type {
        assert!(
            other_stages.contains(&stage_b),
            "should list stage_b as conflicting"
        );
    } else {
        panic!("expected CrossStage conflict type");
    }

    // Clean up
    sqlx::query("DELETE FROM config_stage_association WHERE stage_id IN ($1, $2)")
        .bind(&stage_a)
        .bind(&stage_b)
        .execute(&app.db)
        .await
        .ok();
    sqlx::query("DELETE FROM config_revision WHERE id IN ($1, $2)")
        .bind(revision_id_a)
        .bind(revision_id_b)
        .execute(&app.db)
        .await
        .ok();
}

/// Test publish_with_resolution cancels when conflicts exist and resolution is Cancel.
#[tokio::test]
async fn publish_with_resolution_cancel_on_conflict() {
    let app = TestApp::new().await;

    let stage_a = format!("res-a-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let stage_b = format!("res-b-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Set up cross-stage conflict (same as above)
    let revision_id_a = Uuid::now_v7();
    let revision_id_b = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    let author_id = create_test_author(&app).await;

    sqlx::query(
        r#"INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id) VALUES ($1, 'item_type', 'cancel_test', '{}', $2, $3)"#,
    )
    .bind(revision_id_a).bind(now).bind(author_id)
    .execute(&app.db).await.expect("create rev A");

    sqlx::query(
        r#"INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id) VALUES ($1, 'item_type', 'cancel_test', '{}', $2, $3)"#,
    )
    .bind(revision_id_b).bind(now + 1).bind(author_id)
    .execute(&app.db).await.expect("create rev B");

    sqlx::query("INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id) VALUES ($1, 'item_type', 'cancel_test', $2)")
        .bind(&stage_a).bind(revision_id_a)
        .execute(&app.db).await.expect("assoc A");

    sqlx::query("INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id) VALUES ($1, 'item_type', 'cancel_test', $2)")
        .bind(&stage_b).bind(revision_id_b)
        .execute(&app.db).await.expect("assoc B");

    // Try to publish with Cancel resolution
    let result = app
        .stage()
        .publish_with_resolution(&stage_a, ConflictResolution::Cancel)
        .await
        .expect("publish should return result");

    assert!(
        !result.success,
        "publish should fail with Cancel resolution"
    );
    assert!(result.has_conflicts(), "should report conflicts");
    assert!(result.error_message.is_some());

    // Clean up
    sqlx::query("DELETE FROM config_stage_association WHERE stage_id IN ($1, $2)")
        .bind(&stage_a)
        .bind(&stage_b)
        .execute(&app.db)
        .await
        .ok();
    sqlx::query("DELETE FROM config_revision WHERE id IN ($1, $2)")
        .bind(revision_id_a)
        .bind(revision_id_b)
        .execute(&app.db)
        .await
        .ok();
}

/// Test publish_with_resolution proceeds when resolution is OverwriteAll.
#[tokio::test]
async fn publish_with_resolution_overwrite_all() {
    let app = TestApp::new().await;

    let stage_a = format!("ow-a-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let stage_b = format!("ow-b-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Set up cross-stage conflict
    let revision_id_a = Uuid::now_v7();
    let revision_id_b = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    let author_id = create_test_author(&app).await;

    sqlx::query(
        r#"INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id) VALUES ($1, 'item_type', 'overwrite_test', '{}', $2, $3)"#,
    )
    .bind(revision_id_a).bind(now).bind(author_id)
    .execute(&app.db).await.expect("create rev A");

    sqlx::query(
        r#"INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id) VALUES ($1, 'item_type', 'overwrite_test', '{}', $2, $3)"#,
    )
    .bind(revision_id_b).bind(now + 1).bind(author_id)
    .execute(&app.db).await.expect("create rev B");

    sqlx::query("INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id) VALUES ($1, 'item_type', 'overwrite_test', $2)")
        .bind(&stage_a).bind(revision_id_a)
        .execute(&app.db).await.expect("assoc A");

    sqlx::query("INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id) VALUES ($1, 'item_type', 'overwrite_test', $2)")
        .bind(&stage_b).bind(revision_id_b)
        .execute(&app.db).await.expect("assoc B");

    // Publish with OverwriteAll - should succeed despite conflicts
    let result = app
        .stage()
        .publish_with_resolution(&stage_a, ConflictResolution::OverwriteAll)
        .await
        .expect("publish should return result");

    assert!(result.success, "publish should succeed with OverwriteAll");
    assert!(result.has_conflicts(), "should still report conflicts");

    // Clean up
    sqlx::query("DELETE FROM config_stage_association WHERE stage_id IN ($1, $2)")
        .bind(&stage_a)
        .bind(&stage_b)
        .execute(&app.db)
        .await
        .ok();
    sqlx::query("DELETE FROM config_revision WHERE id IN ($1, $2)")
        .bind(revision_id_a)
        .bind(revision_id_b)
        .execute(&app.db)
        .await
        .ok();
}

/// Test live-modified conflict detection for config entities.
/// Scenario: Config is staged at T1, then live is modified at T2 (T2 > T1).
#[tokio::test]
async fn conflict_detection_live_modified_config() {
    let app = TestApp::new().await;

    let stage_id = format!("lm-{}", &Uuid::now_v7().simple().to_string()[..8]);
    let author_id = create_test_author(&app).await;

    // Create a staged revision at T1
    let staged_revision_id = Uuid::now_v7();
    let t1 = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id)
           VALUES ($1, 'item_type', 'live_mod_test', '{"version": "staged"}', $2, $3)"#,
    )
    .bind(staged_revision_id)
    .bind(t1)
    .bind(author_id)
    .execute(&app.db)
    .await
    .expect("create staged revision");

    sqlx::query(
        "INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id)
         VALUES ($1, 'item_type', 'live_mod_test', $2)"
    )
    .bind(&stage_id)
    .bind(staged_revision_id)
    .execute(&app.db)
    .await
    .expect("create stage association");

    // Create a NEWER live revision at T2 (simulating live was modified after staging)
    let live_revision_id = Uuid::now_v7();
    let t2 = t1 + 100; // 100 seconds later

    sqlx::query(
        r#"INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id)
           VALUES ($1, 'item_type', 'live_mod_test', '{"version": "live_updated"}', $2, $3)"#,
    )
    .bind(live_revision_id)
    .bind(t2)
    .bind(author_id)
    .execute(&app.db)
    .await
    .expect("create live revision");

    sqlx::query(
        "INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id)
         VALUES ('live', 'item_type', 'live_mod_test', $1)"
    )
    .bind(live_revision_id)
    .execute(&app.db)
    .await
    .expect("create live association");

    // Detect conflicts - should find LiveModified conflict
    let conflicts = app
        .stage()
        .detect_conflicts(&stage_id)
        .await
        .expect("detect should work");

    assert_eq!(conflicts.len(), 1, "should detect 1 live-modified conflict");
    assert_eq!(conflicts[0].entity_type, "item_type");
    assert_eq!(conflicts[0].entity_id, "live_mod_test");

    if let trovato_kernel::ConflictType::LiveModified {
        staged_at,
        live_changed,
    } = &conflicts[0].conflict_type
    {
        assert_eq!(*staged_at, t1, "staged_at should be T1");
        assert_eq!(*live_changed, t2, "live_changed should be T2");
    } else {
        panic!(
            "expected LiveModified conflict type, got {:?}",
            conflicts[0].conflict_type
        );
    }

    // Clean up
    sqlx::query("DELETE FROM config_stage_association WHERE entity_id = 'live_mod_test'")
        .execute(&app.db)
        .await
        .ok();
    sqlx::query("DELETE FROM config_revision WHERE entity_id = 'live_mod_test'")
        .execute(&app.db)
        .await
        .ok();
}

/// Test has_changes detects staged config.
#[tokio::test]
async fn stage_has_changes_with_config() {
    let app = TestApp::new().await;
    let stage_id = format!("cfg-chg-{}", &Uuid::now_v7().simple().to_string()[..8]);

    // Initially no changes
    let has_changes = app.stage().has_changes(&stage_id).await.expect("check");
    assert!(!has_changes, "empty stage should have no changes");

    // Add a config revision
    let revision_id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    let author_id = create_test_author(&app).await;

    sqlx::query(
        r#"INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id) VALUES ($1, 'item_type', 'has_changes_test', '{}', $2, $3)"#,
    )
    .bind(revision_id).bind(now).bind(author_id)
    .execute(&app.db).await.expect("create rev");

    sqlx::query("INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id) VALUES ($1, 'item_type', 'has_changes_test', $2)")
        .bind(&stage_id).bind(revision_id)
        .execute(&app.db).await.expect("assoc");

    // Now should have changes
    let has_changes = app.stage().has_changes(&stage_id).await.expect("check");
    assert!(has_changes, "stage with config should have changes");

    // Clean up
    sqlx::query("DELETE FROM config_stage_association WHERE stage_id = $1")
        .bind(&stage_id)
        .execute(&app.db)
        .await
        .ok();
    sqlx::query("DELETE FROM config_revision WHERE id = $1")
        .bind(revision_id)
        .execute(&app.db)
        .await
        .ok();
}
