//! Blog series navigation plugin for Trovato.
//!
//! When viewing a blog item that belongs to a series (identified by
//! `field_series_title`), injects series navigation data including
//! previous/next links and position within the series.

use serde::Deserialize;
use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// Row returned from the series sibling query.
#[derive(Debug, Deserialize)]
struct SeriesSibling {
    id: String,
    title: String,
}

/// Register the series navigation permission.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "view series navigation",
        "View series navigation links on blog posts",
    )]
}

/// Inject series navigation data when viewing a blog item with a series title.
///
/// Queries for sibling items that share the same `field_series_title`,
/// ordered by creation date. Returns a JSON string with series metadata
/// (title, position, total, previous/next links) or empty string if the
/// item is not part of a series.
#[plugin_tap]
pub fn tap_item_view(item: Item) -> String {
    // Only process blog items
    if item.item_type != "blog" {
        return String::new();
    }

    // Check if the item has a series title
    let series_title = match item.fields.get("field_series_title") {
        Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return String::new(),
    };

    // Query for all items in the same series, ordered by created date
    let siblings_json = match host::query_raw(
        "SELECT id::text, title FROM item \
         WHERE item_type = 'blog' \
         AND status = 1 \
         AND fields->>'field_series_title' = $1 \
         ORDER BY created ASC",
        &[serde_json::json!(series_title)],
    ) {
        Ok(json) => json,
        Err(code) => {
            host::log(
                "warn",
                "trovato_series",
                &format!("Series query failed with code {code}"),
            );
            return String::new();
        }
    };

    let siblings: Vec<SeriesSibling> = match serde_json::from_str(&siblings_json) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };

    // Need at least 2 items to show series navigation
    if siblings.len() < 2 {
        return String::new();
    }

    let current_id = item.id.to_string();
    let current_pos = siblings.iter().position(|s| s.id == current_id);

    let Some(pos) = current_pos else {
        return String::new();
    };

    let total = siblings.len();
    let position = pos + 1; // 1-based

    // Build previous/next links
    let previous = if pos > 0 {
        let prev = &siblings[pos - 1];
        serde_json::json!({
            "title": prev.title,
            "url": format!("/item/{}", prev.id),
        })
    } else {
        serde_json::Value::Null
    };

    let next = if pos + 1 < total {
        let nxt = &siblings[pos + 1];
        serde_json::json!({
            "title": nxt.title,
            "url": format!("/item/{}", nxt.id),
        })
    } else {
        serde_json::Value::Null
    };

    let nav = serde_json::json!({
        "series_title": series_title,
        "current_position": position,
        "total": total,
        "previous": previous,
        "next": next,
    });

    nav.to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_blog_item(series_title: Option<&str>) -> Item {
        let mut fields = HashMap::new();
        if let Some(title) = series_title {
            fields.insert(
                "field_series_title".to_string(),
                serde_json::Value::String(title.to_string()),
            );
        }
        Item {
            id: Uuid::nil(),
            item_type: "blog".to_string(),
            title: "Test Post".to_string(),
            fields,
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        }
    }

    #[test]
    fn perm_returns_series_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "view series navigation");
    }

    #[test]
    fn view_skips_non_blog_items() {
        let mut item = make_blog_item(Some("My Series"));
        item.item_type = "conference".to_string();
        assert!(__inner_tap_item_view(item).is_empty());
    }

    #[test]
    fn view_skips_items_without_series() {
        let item = make_blog_item(None);
        assert!(__inner_tap_item_view(item).is_empty());
    }

    #[test]
    fn view_skips_empty_series_title() {
        let item = make_blog_item(Some(""));
        assert!(__inner_tap_item_view(item).is_empty());
    }

    #[test]
    fn view_returns_empty_when_query_returns_empty() {
        // Stub query_raw returns "[]", so fewer than 2 siblings
        let item = make_blog_item(Some("Rust Series"));
        let result = __inner_tap_item_view(item);
        assert!(result.is_empty());
    }
}
