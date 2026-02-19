#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Theme engine tests.

use std::collections::BTreeMap;
use trovato_kernel::theme::{RenderTreeConsumer, ThemeEngine};
use trovato_sdk::render::RenderElement;

#[test]
fn test_theme_engine_empty() {
    let engine = ThemeEngine::empty().unwrap();
    assert!(engine.resolve_template(&["nonexistent"]).is_none());
}

#[test]
fn test_is_admin_path() {
    assert!(ThemeEngine::is_admin_path("/admin"));
    assert!(ThemeEngine::is_admin_path("/admin/structure/types"));
    assert!(ThemeEngine::is_admin_path("/admin/content"));
    assert!(!ThemeEngine::is_admin_path("/item/123"));
    assert!(!ThemeEngine::is_admin_path("/"));
    assert!(!ThemeEngine::is_admin_path("/user/login"));
}

#[test]
fn test_page_suggestions_admin() {
    let suggestions = ThemeEngine::page_suggestions("/admin/structure/types");
    assert_eq!(
        suggestions,
        vec!["page--admin--structure--types", "page--admin", "page"]
    );
}

#[test]
fn test_page_suggestions_frontend() {
    let suggestions = ThemeEngine::page_suggestions("/item/123");
    assert_eq!(suggestions, vec!["page--item--123", "page"]);
}

#[test]
fn test_page_suggestions_root() {
    let suggestions = ThemeEngine::page_suggestions("/");
    // Root path normalizes to empty, so just page
    assert!(suggestions.contains(&"page".to_string()));
}

#[test]
fn test_render_tree_consumer_markup() {
    let _consumer = RenderTreeConsumer::new();

    let element = RenderElement {
        element_type: "markup".to_string(),
        weight: None,
        tag: Some("p".to_string()),
        value: Some("Hello world".to_string()),
        format: Some("plain_text".to_string()),
        attributes: None,
        children: BTreeMap::new(),
    };

    // Would need a Tera instance to fully test render
    // This just ensures the struct can be created
    assert_eq!(element.element_type, "markup");
    assert_eq!(element.tag, Some("p".to_string()));
}

#[test]
fn test_render_element_with_class() {
    let mut attrs = serde_json::Map::new();
    attrs.insert(
        "class".to_string(),
        serde_json::Value::Array(vec![
            serde_json::Value::String("foo".to_string()),
            serde_json::Value::String("bar".to_string()),
        ]),
    );

    let element = RenderElement {
        element_type: "container".to_string(),
        weight: Some(10),
        tag: None,
        value: None,
        format: None,
        attributes: Some(serde_json::Value::Object(attrs)),
        children: BTreeMap::new(),
    };

    assert_eq!(element.element_type, "container");
    assert_eq!(element.weight, Some(10));

    if let Some(attrs) = &element.attributes {
        let classes = attrs.get("class").unwrap();
        assert!(classes.is_array());
        assert_eq!(classes.as_array().unwrap().len(), 2);
    }
}

#[test]
fn test_render_element_children() {
    let child = RenderElement {
        element_type: "markup".to_string(),
        weight: None,
        tag: Some("span".to_string()),
        value: Some("Child".to_string()),
        format: None,
        attributes: None,
        children: BTreeMap::new(),
    };

    let mut children = BTreeMap::new();
    children.insert("child1".to_string(), child);

    let parent = RenderElement {
        element_type: "container".to_string(),
        weight: None,
        tag: None,
        value: None,
        format: None,
        attributes: None,
        children,
    };

    assert_eq!(parent.children.len(), 1);
    assert!(parent.children.contains_key("child1"));
}

#[test]
fn test_render_element_serialization() {
    let element = RenderElement {
        element_type: "markup".to_string(),
        weight: Some(5),
        tag: Some("div".to_string()),
        value: Some("content".to_string()),
        format: Some("plain_text".to_string()),
        attributes: None,
        children: BTreeMap::new(),
    };

    let json = serde_json::to_string(&element).unwrap();
    assert!(json.contains("\"#type\":\"markup\""));
    assert!(json.contains("\"#weight\":5"));
    assert!(json.contains("\"#tag\":\"div\""));
    assert!(json.contains("\"#value\":\"content\""));

    let parsed: RenderElement = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.element_type, "markup");
    assert_eq!(parsed.weight, Some(5));
}
