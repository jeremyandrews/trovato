#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Category system integration tests.
//!
//! Tests for categories, tags, and hierarchical queries.

use trovato_kernel::models::{
    Category, CreateCategory, CreateTag, Tag, TagWithDepth, UpdateCategory, UpdateTag,
};
use uuid::Uuid;

// -------------------------------------------------------------------------
// Category tests
// -------------------------------------------------------------------------

#[test]
fn category_creation() {
    let input = CreateCategory {
        id: "tags".to_string(),
        label: "Tags".to_string(),
        description: Some("Content tags".to_string()),
        hierarchy: Some(0),
        weight: Some(10),
    };

    assert_eq!(input.id, "tags");
    assert_eq!(input.label, "Tags");
    assert_eq!(input.hierarchy, Some(0));
}

#[test]
fn category_update() {
    let input = UpdateCategory {
        label: Some("Updated Tags".to_string()),
        description: None,
        hierarchy: Some(2),
        weight: None,
    };

    assert_eq!(input.label, Some("Updated Tags".to_string()));
    assert_eq!(input.hierarchy, Some(2));
}

#[test]
fn category_hierarchy_modes() {
    // 0 = no hierarchy (flat tags)
    let flat = Category {
        id: "tags".to_string(),
        label: "Tags".to_string(),
        description: None,
        hierarchy: 0,
        weight: 0,
    };
    assert_eq!(flat.hierarchy, 0);

    // 1 = single parent (tree)
    let tree = Category {
        id: "categories".to_string(),
        label: "Categories".to_string(),
        description: None,
        hierarchy: 1,
        weight: 0,
    };
    assert_eq!(tree.hierarchy, 1);

    // 2 = multiple parents (DAG)
    let dag = Category {
        id: "topics".to_string(),
        label: "Topics".to_string(),
        description: None,
        hierarchy: 2,
        weight: 0,
    };
    assert_eq!(dag.hierarchy, 2);
}

// -------------------------------------------------------------------------
// Tag tests
// -------------------------------------------------------------------------

#[test]
fn tag_creation() {
    let input = CreateTag {
        category_id: "categories".to_string(),
        label: "Technology".to_string(),
        description: Some("Technology articles".to_string()),
        weight: Some(10),
        parent_ids: None,
    };

    assert_eq!(input.category_id, "categories");
    assert_eq!(input.label, "Technology");
    assert!(input.parent_ids.is_none());
}

#[test]
fn tag_with_parents() {
    let parent_id = Uuid::now_v7();
    let input = CreateTag {
        category_id: "categories".to_string(),
        label: "Rust".to_string(),
        description: None,
        weight: None,
        parent_ids: Some(vec![parent_id]),
    };

    assert_eq!(input.parent_ids.as_ref().unwrap().len(), 1);
    assert_eq!(input.parent_ids.unwrap()[0], parent_id);
}

#[test]
fn tag_with_multiple_parents() {
    let parent1 = Uuid::now_v7();
    let parent2 = Uuid::now_v7();
    let input = CreateTag {
        category_id: "topics".to_string(),
        label: "Web Development".to_string(),
        description: None,
        weight: None,
        parent_ids: Some(vec![parent1, parent2]),
    };

    // DAG: tag can have multiple parents
    assert_eq!(input.parent_ids.as_ref().unwrap().len(), 2);
}

#[test]
fn tag_update() {
    let input = UpdateTag {
        label: Some("Updated Label".to_string()),
        description: Some("New description".to_string()),
        weight: Some(5),
    };

    assert_eq!(input.label, Some("Updated Label".to_string()));
    assert_eq!(input.weight, Some(5));
}

#[test]
fn tag_serialization() {
    let tag = Tag {
        id: Uuid::nil(),
        category_id: "tags".to_string(),
        label: "Rust".to_string(),
        description: Some("Rust programming".to_string()),
        weight: 0,
        created: 1000,
        changed: 2000,
    };

    let json = serde_json::to_string(&tag).unwrap();
    assert!(json.contains("Rust"));
    assert!(json.contains("Rust programming"));

    let parsed: Tag = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.label, "Rust");
    assert_eq!(parsed.created, 1000);
    assert_eq!(parsed.changed, 2000);
}

// -------------------------------------------------------------------------
// Hierarchy tests
// -------------------------------------------------------------------------

#[test]
fn tag_with_depth() {
    let tag = Tag {
        id: Uuid::nil(),
        category_id: "categories".to_string(),
        label: "Programming".to_string(),
        description: None,
        weight: 0,
        created: 1000,
        changed: 1000,
    };

    let with_depth = TagWithDepth { tag, depth: 2 };

    assert_eq!(with_depth.depth, 2);
    assert_eq!(with_depth.tag.label, "Programming");
}

#[test]
fn tag_with_depth_serialization() {
    let tag = Tag {
        id: Uuid::nil(),
        category_id: "categories".to_string(),
        label: "Technology".to_string(),
        description: None,
        weight: 0,
        created: 1000,
        changed: 1000,
    };

    let with_depth = TagWithDepth { tag, depth: 3 };

    let json = serde_json::to_string(&with_depth).unwrap();
    assert!(json.contains("\"depth\":3"));
    assert!(json.contains("Technology"));
}

// -------------------------------------------------------------------------
// Edge cases
// -------------------------------------------------------------------------

#[test]
fn category_with_special_characters() {
    let category = Category {
        id: "my-category_123".to_string(),
        label: "My Category with <special> & characters".to_string(),
        description: Some("Description with \"quotes\" and 'apostrophes'".to_string()),
        hierarchy: 0,
        weight: 0,
    };

    let json = serde_json::to_string(&category).unwrap();
    let parsed: Category = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, "my-category_123");
}

#[test]
fn tag_with_unicode() {
    let tag = Tag {
        id: Uuid::nil(),
        category_id: "i18n".to_string(),
        label: "日本語".to_string(),
        description: Some("Japanese tag".to_string()),
        weight: 0,
        created: 1000,
        changed: 1000,
    };

    let json = serde_json::to_string(&tag).unwrap();
    assert!(json.contains("日本語"));

    let parsed: Tag = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.label, "日本語");
}

#[test]
fn empty_parent_list_means_root() {
    let input = CreateTag {
        category_id: "categories".to_string(),
        label: "Root Tag".to_string(),
        description: None,
        weight: None,
        parent_ids: Some(vec![]), // Empty list = root
    };

    assert!(input.parent_ids.as_ref().unwrap().is_empty());
}
