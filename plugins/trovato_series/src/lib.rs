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

/// Escape a string for interpolation into HTML text or a double-quoted
/// attribute value.
///
/// The SDK ships no escaping helper (**G-SDK-NO-ESCAPE**, Argus M3), so every
/// rendering plugin writes this. Kept deliberately conservative: the five
/// characters that can break out of text or an attribute.
fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render series navigation when viewing a blog item with a series title.
///
/// Queries for sibling items that share the same `field_series_title`, ordered
/// by creation date, and returns an HTML fragment — the item route appends a
/// view tap's output to the page's children, so markup is what it must be
/// handed. (Before the G-VIEW-OUTPUT-JSON-ENCODED fix this tap returned a JSON
/// metadata blob, which reached the page as escaped JSON text and rendered as
/// nothing a reader could use.)
///
/// Returns an empty string when the item is not part of a series.
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
        // The content-type column on `item` is `type`, not `item_type` — the
        // latter is the *table* of type definitions. Querying `item_type` made
        // every sibling lookup fail, so this tap returned an empty fragment on
        // every blog post it was supposed to decorate.
        "SELECT id::text, title FROM item \
         WHERE type = 'blog' \
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

    let mut html = String::new();
    html.push_str("<nav class=\"series-nav\" aria-label=\"Series navigation\">");
    html.push_str(&format!(
        "<p class=\"series-nav__position\">Part {position} of {total} in the series \
         <strong class=\"series-nav__title\">{}</strong></p>",
        escape(&series_title)
    ));

    html.push_str("<ul class=\"series-nav__links\">");
    if pos > 0 {
        let prev = &siblings[pos - 1];
        html.push_str(&format!(
            "<li class=\"series-nav__prev\"><a rel=\"prev\" href=\"/item/{}\">\
             &larr; {}</a></li>",
            escape(&prev.id),
            escape(&prev.title)
        ));
    }
    if pos + 1 < total {
        let nxt = &siblings[pos + 1];
        html.push_str(&format!(
            "<li class=\"series-nav__next\"><a rel=\"next\" href=\"/item/{}\">\
             {} &rarr;</a></li>",
            escape(&nxt.id),
            escape(&nxt.title)
        ));
    }
    html.push_str("</ul></nav>");

    html
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
