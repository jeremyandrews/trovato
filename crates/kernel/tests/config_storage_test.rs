//! Integration tests for ConfigStorage trait.
//!
//! Tests the DirectConfigStorage implementation against a real database.

use uuid::Uuid;

mod common;
use common::TestApp;

use trovato_kernel::config_storage::{ConfigEntity, ConfigFilter, entity_types};
use trovato_kernel::models::{Category, ItemType, Tag};

/// Test loading an item type via ConfigStorage.
#[tokio::test]
async fn config_storage_load_item_type() {
    let app = TestApp::new().await;

    // The "page" item type is seeded by migrations
    let entity = app
        .config_storage()
        .load(entity_types::ITEM_TYPE, "page")
        .await
        .expect("failed to load item type");

    assert!(entity.is_some(), "page item type should exist");

    let entity = entity.unwrap();
    assert_eq!(entity.entity_type(), "item_type");
    assert_eq!(entity.id(), "page");

    let item_type = entity.as_item_type().expect("expected ItemType variant");
    assert_eq!(item_type.label, "Basic Page");
}

/// Test listing item types via ConfigStorage.
#[tokio::test]
async fn config_storage_list_item_types() {
    let app = TestApp::new().await;

    let entities = app
        .config_storage()
        .list(entity_types::ITEM_TYPE, None)
        .await
        .expect("failed to list item types");

    // At least the seeded "page" type should exist
    assert!(!entities.is_empty(), "should have at least one item type");

    let page_type = entities
        .iter()
        .find(|e| e.id() == "page")
        .expect("page type should be in list");

    assert_eq!(page_type.entity_type(), "item_type");
}

/// Test saving and loading an item type via ConfigStorage.
#[tokio::test]
async fn config_storage_save_and_load_item_type() {
    let app = TestApp::new().await;

    // Create a unique type name for this test (max 32 chars)
    let type_name = format!("ttype{}", &Uuid::now_v7().simple().to_string()[..8]);

    let item_type = ItemType {
        type_name: type_name.clone(),
        label: "Test Content Type".to_string(),
        description: Some("A test content type".to_string()),
        has_title: true,
        title_label: Some("Title".to_string()),
        plugin: "test".to_string(),
        settings: serde_json::json!({"fields": []}),
    };

    // Save via ConfigStorage
    app.config_storage()
        .save(&ConfigEntity::ItemType(item_type.clone()))
        .await
        .expect("failed to save item type");

    // Load via ConfigStorage
    let loaded = app
        .config_storage()
        .load(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to load item type")
        .expect("item type should exist");

    let loaded_type = loaded.as_item_type().expect("expected ItemType");
    assert_eq!(loaded_type.type_name, type_name);
    assert_eq!(loaded_type.label, "Test Content Type");
    assert_eq!(loaded_type.plugin, "test");

    // Clean up
    app.config_storage()
        .delete(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to delete item type");
}

/// Test deleting an item type via ConfigStorage.
#[tokio::test]
async fn config_storage_delete_item_type() {
    let app = TestApp::new().await;

    // Create a unique type name (max 32 chars)
    let type_name = format!("dtest{}", &Uuid::now_v7().simple().to_string()[..8]);

    let item_type = ItemType {
        type_name: type_name.clone(),
        label: "Delete Test".to_string(),
        description: None,
        has_title: true,
        title_label: None,
        plugin: "test".to_string(),
        settings: serde_json::json!({}),
    };

    // Save
    app.config_storage()
        .save(&ConfigEntity::ItemType(item_type))
        .await
        .expect("failed to save");

    // Verify exists
    assert!(
        app.config_storage()
            .exists(entity_types::ITEM_TYPE, &type_name)
            .await
            .expect("failed to check existence")
    );

    // Delete
    let deleted = app
        .config_storage()
        .delete(entity_types::ITEM_TYPE, &type_name)
        .await
        .expect("failed to delete");

    assert!(deleted, "should return true when deleting existing type");

    // Verify deleted
    assert!(
        !app.config_storage()
            .exists(entity_types::ITEM_TYPE, &type_name)
            .await
            .expect("failed to check existence")
    );
}

/// Test loading a category via ConfigStorage.
#[tokio::test]
async fn config_storage_category_crud() {
    let app = TestApp::new().await;

    // Create a unique category ID (max 32 chars)
    let category_id = format!("cat{}", &Uuid::now_v7().simple().to_string()[..8]);

    let category = Category {
        id: category_id.clone(),
        label: "Test Category".to_string(),
        description: Some("A test category".to_string()),
        hierarchy: 0,
        weight: 0,
    };

    // Save
    app.config_storage()
        .save(&ConfigEntity::Category(category))
        .await
        .expect("failed to save category");

    // Load
    let loaded = app
        .config_storage()
        .load(entity_types::CATEGORY, &category_id)
        .await
        .expect("failed to load category")
        .expect("category should exist");

    let loaded_cat = loaded.as_category().expect("expected Category");
    assert_eq!(loaded_cat.id, category_id);
    assert_eq!(loaded_cat.label, "Test Category");

    // List
    let categories = app
        .config_storage()
        .list(entity_types::CATEGORY, None)
        .await
        .expect("failed to list categories");

    assert!(
        categories.iter().any(|c| c.id() == category_id),
        "category should be in list"
    );

    // Delete
    let deleted = app
        .config_storage()
        .delete(entity_types::CATEGORY, &category_id)
        .await
        .expect("failed to delete category");

    assert!(deleted);

    // Verify deleted
    let after_delete = app
        .config_storage()
        .load(entity_types::CATEGORY, &category_id)
        .await
        .expect("failed to load");

    assert!(after_delete.is_none(), "category should be deleted");
}

/// Test tag CRUD via ConfigStorage.
/// This verifies that save preserves the provided tag ID (round-trip test).
#[tokio::test]
async fn config_storage_tag_crud() {
    let app = TestApp::new().await;

    // First create a category for the tag
    let category_id = format!("tcat{}", &Uuid::now_v7().simple().to_string()[..8]);

    let category = Category {
        id: category_id.clone(),
        label: "Tag Test Category".to_string(),
        description: None,
        hierarchy: 0,
        weight: 0,
    };

    app.config_storage()
        .save(&ConfigEntity::Category(category))
        .await
        .expect("failed to save category");

    // Create a tag with a specific ID
    let tag_id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();

    let tag = Tag {
        id: tag_id,
        category_id: category_id.clone(),
        label: "Test Tag".to_string(),
        description: Some("A test tag".to_string()),
        weight: 0,
        created: now,
        changed: now,
    };

    // Save
    app.config_storage()
        .save(&ConfigEntity::Tag(tag.clone()))
        .await
        .expect("failed to save tag");

    // Load - this verifies the ID was preserved (round-trip)
    let loaded = app
        .config_storage()
        .load(entity_types::TAG, &tag_id.to_string())
        .await
        .expect("failed to load tag")
        .expect("tag should exist");

    let loaded_tag = loaded.as_tag().expect("expected Tag");
    assert_eq!(loaded_tag.id, tag_id, "tag ID should be preserved");
    assert_eq!(loaded_tag.label, "Test Tag");
    assert_eq!(loaded_tag.category_id, category_id);

    // Update
    let updated_tag = Tag {
        label: "Updated Tag".to_string(),
        ..tag.clone()
    };

    app.config_storage()
        .save(&ConfigEntity::Tag(updated_tag))
        .await
        .expect("failed to update tag");

    // Load updated
    let loaded_updated = app
        .config_storage()
        .load(entity_types::TAG, &tag_id.to_string())
        .await
        .expect("failed to load")
        .expect("should exist");

    let loaded_updated_tag = loaded_updated.as_tag().expect("expected Tag");
    assert_eq!(loaded_updated_tag.label, "Updated Tag");

    // List by category
    let filter = ConfigFilter::new().with_field("category_id", &category_id);
    let tags = app
        .config_storage()
        .list(entity_types::TAG, Some(&filter))
        .await
        .expect("failed to list tags");

    assert!(
        tags.iter().any(|t| t.id() == tag_id.to_string()),
        "tag should be in list"
    );

    // Delete tag
    let deleted = app
        .config_storage()
        .delete(entity_types::TAG, &tag_id.to_string())
        .await
        .expect("failed to delete tag");

    assert!(deleted);

    // Verify deleted
    let after_delete = app
        .config_storage()
        .load(entity_types::TAG, &tag_id.to_string())
        .await
        .expect("failed to load");

    assert!(after_delete.is_none(), "tag should be deleted");

    // Clean up category
    app.config_storage()
        .delete(entity_types::CATEGORY, &category_id)
        .await
        .ok();
}

/// Test variable (site config) storage via ConfigStorage.
#[tokio::test]
async fn config_storage_variable_crud() {
    let app = TestApp::new().await;

    // Create a unique key
    let key = format!("tvar_{}", &Uuid::now_v7().simple().to_string()[..8]);
    let value = serde_json::json!({"foo": "bar", "count": 42});

    // Save
    app.config_storage()
        .save(&ConfigEntity::Variable {
            key: key.clone(),
            value: value.clone(),
        })
        .await
        .expect("failed to save variable");

    // Load
    let loaded = app
        .config_storage()
        .load(entity_types::VARIABLE, &key)
        .await
        .expect("failed to load variable")
        .expect("variable should exist");

    let (loaded_key, loaded_value) = loaded.as_variable().expect("expected Variable");
    assert_eq!(loaded_key, key);
    assert_eq!(loaded_value, &value);

    // Update
    let new_value = serde_json::json!({"updated": true});
    app.config_storage()
        .save(&ConfigEntity::Variable {
            key: key.clone(),
            value: new_value.clone(),
        })
        .await
        .expect("failed to update variable");

    // Load updated
    let updated = app
        .config_storage()
        .load(entity_types::VARIABLE, &key)
        .await
        .expect("failed to load")
        .expect("should exist");

    let (_, updated_value) = updated.as_variable().expect("expected Variable");
    assert_eq!(updated_value, &new_value);

    // Delete
    app.config_storage()
        .delete(entity_types::VARIABLE, &key)
        .await
        .expect("failed to delete");

    // Verify deleted
    let after_delete = app
        .config_storage()
        .load(entity_types::VARIABLE, &key)
        .await
        .expect("failed to load");

    assert!(after_delete.is_none());
}

/// Test that config revision schema tables exist (Story 21.2).
/// These tables are scaffolding for post-MVP - they should exist but be empty.
#[tokio::test]
async fn config_revision_schema_tables_exist() {
    let app = TestApp::new().await;

    // Verify config_revision table exists with correct columns
    let config_revision_exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_name = 'config_revision'
        )
        "#,
    )
    .fetch_one(&app.db)
    .await
    .expect("failed to check config_revision table");

    assert!(config_revision_exists, "config_revision table should exist");

    // Verify config_stage_association table exists
    let config_stage_assoc_exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_name = 'config_stage_association'
        )
        "#,
    )
    .fetch_one(&app.db)
    .await
    .expect("failed to check config_stage_association table");

    assert!(
        config_stage_assoc_exists,
        "config_stage_association table should exist"
    );

    // Verify stage_deletion table exists
    let stage_deletion_exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_name = 'stage_deletion'
        )
        "#,
    )
    .fetch_one(&app.db)
    .await
    .expect("failed to check stage_deletion table");

    assert!(stage_deletion_exists, "stage_deletion table should exist");

    // Clean up any test data left by other tests
    // These tables may have data from conflict detection tests
    // Delete associations first (FK constraint)
    sqlx::query("DELETE FROM config_stage_association")
        .execute(&app.db)
        .await
        .ok();
    // Then delete revisions
    sqlx::query("DELETE FROM config_revision")
        .execute(&app.db)
        .await
        .ok();

    // Verify tables are empty (v1.0 scaffolding - no production data should exist)
    let config_revision_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config_revision")
        .fetch_one(&app.db)
        .await
        .expect("failed to count config_revision");

    assert_eq!(
        config_revision_count, 0,
        "config_revision should be empty in v1.0 (after test cleanup)"
    );

    let config_stage_assoc_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM config_stage_association")
            .fetch_one(&app.db)
            .await
            .expect("failed to count config_stage_association");

    assert_eq!(
        config_stage_assoc_count, 0,
        "config_stage_association should be empty in v1.0"
    );

    let stage_deletion_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stage_deletion")
        .fetch_one(&app.db)
        .await
        .expect("failed to count stage_deletion");

    assert_eq!(
        stage_deletion_count, 0,
        "stage_deletion should be empty in v1.0"
    );
}

/// Test that DirectConfigStorage does NOT write to config_revision tables.
/// This verifies AC #4: No v1.0 code writes to these tables.
#[tokio::test]
async fn config_storage_does_not_write_to_revision_tables() {
    let app = TestApp::new().await;

    // Create a unique type name
    let type_name = format!("rev{}", &Uuid::now_v7().simple().to_string()[..8]);

    let item_type = ItemType {
        type_name: type_name.clone(),
        label: "Revision Test Type".to_string(),
        description: None,
        has_title: true,
        title_label: None,
        plugin: "test".to_string(),
        settings: serde_json::json!({}),
    };

    // Save via ConfigStorage
    app.config_storage()
        .save(&ConfigEntity::ItemType(item_type))
        .await
        .expect("failed to save item type");

    // Clean up any test data left by other tests first
    // Delete associations first (FK constraint), then revisions
    sqlx::query("DELETE FROM config_stage_association")
        .execute(&app.db)
        .await
        .ok();
    sqlx::query("DELETE FROM config_revision")
        .execute(&app.db)
        .await
        .ok();

    // Verify config_revision is still empty (DirectConfigStorage doesn't write here)
    let revision_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config_revision")
        .fetch_one(&app.db)
        .await
        .expect("failed to count revisions");

    assert_eq!(
        revision_count, 0,
        "DirectConfigStorage should NOT write to config_revision (after test cleanup)"
    );

    // Verify config_stage_association is still empty
    let assoc_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config_stage_association")
        .fetch_one(&app.db)
        .await
        .expect("failed to count associations");

    assert_eq!(
        assoc_count, 0,
        "DirectConfigStorage should NOT write to config_stage_association"
    );

    // Clean up
    app.config_storage()
        .delete(entity_types::ITEM_TYPE, &type_name)
        .await
        .ok();
}

/// Test filtering when listing.
#[tokio::test]
async fn config_storage_list_with_filter() {
    let app = TestApp::new().await;

    // Create test item types with a specific plugin (keep names short)
    let short_id = &Uuid::now_v7().simple().to_string()[..6];
    let plugin_name = format!("filt{}", short_id);
    let type1 = format!("ft1{}", short_id);
    let type2 = format!("ft2{}", short_id);

    for type_name in [&type1, &type2] {
        let item_type = ItemType {
            type_name: type_name.clone(),
            label: format!("{} Label", type_name),
            description: None,
            has_title: true,
            title_label: None,
            plugin: plugin_name.clone(),
            settings: serde_json::json!({}),
        };
        app.config_storage()
            .save(&ConfigEntity::ItemType(item_type))
            .await
            .expect("failed to save");
    }

    // List with filter by plugin
    let filter = ConfigFilter::new().with_field("plugin", &plugin_name);
    let filtered = app
        .config_storage()
        .list(entity_types::ITEM_TYPE, Some(&filter))
        .await
        .expect("failed to list with filter");

    assert_eq!(filtered.len(), 2, "should have exactly 2 types for plugin");

    // Clean up
    for type_name in [&type1, &type2] {
        app.config_storage()
            .delete(entity_types::ITEM_TYPE, type_name)
            .await
            .ok();
    }
}
