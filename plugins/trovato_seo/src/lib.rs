//! SEO plugin for Trovato.
//!
//! Provides meta tags, Open Graph markup, JSON-LD structured data,
//! and sitemap.xml generation for search engine optimization.

use trovato_sdk::prelude::*;

/// SEO-related permissions.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new(
            "administer seo",
            "Configure global SEO settings and defaults",
        ),
        PermissionDefinition::new(
            "edit seo fields",
            "Edit per-item SEO fields (meta title, description, robots)",
        ),
    ]
}

/// Admin menu for SEO configuration.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/seo", "SEO Settings")
            .callback("seo_admin")
            .permission("administer seo")
            .parent("/admin/config"),
    ]
}

/// Inject SEO structured data into item view.
///
/// Emits a JSON-LD `<script>` tag with Article structured data and
/// hidden meta-like markup for search engine optimization. JSON-LD
/// script tags are valid in `<body>` per the HTML5 specification.
#[plugin_tap]
pub fn tap_item_view(item: Item) -> String {
    let title = &item.title;
    let item_type = &item.item_type;
    let created = item.created;

    // Build description from field_description or field_body
    let description = item
        .get_text("field_description")
        .or_else(|| item.get_text("field_body"))
        .unwrap_or_default();

    // Truncate description to 160 characters for meta tag
    let meta_desc = truncate_description(&description, 160);

    let mut html = String::new();

    // Emit hidden SEO metadata as data attributes (available for theme JS)
    // and JSON-LD structured data (consumed directly by search engines).
    html.push_str("<div class=\"seo-metadata\" hidden");
    if !meta_desc.is_empty() {
        html.push_str(&format!(
            " data-description=\"{}\"",
            escape_attr(&meta_desc)
        ));
    }
    html.push_str(&format!(
        " data-og-type=\"{}\"",
        if item_type == "blog" {
            "article"
        } else {
            "website"
        }
    ));
    html.push_str("></div>");

    // JSON-LD structured data (valid in <body> per HTML5 spec)
    let date_published = format_timestamp(created);
    html.push_str("<script type=\"application/ld+json\">{");
    html.push_str("\"@context\":\"https://schema.org\",");
    html.push_str("\"@type\":\"Article\",");
    html.push_str(&format!("\"headline\":\"{}\",", escape_json_string(title)));
    if !meta_desc.is_empty() {
        html.push_str(&format!(
            "\"description\":\"{}\",",
            escape_json_string(&meta_desc)
        ));
    }
    html.push_str(&format!("\"datePublished\":\"{date_published}\""));
    html.push_str("}</script>");

    html
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
///
/// Handles UTF-8 char boundaries correctly.
fn truncate_description(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    // Leave room for "..."
    let target = max_len.saturating_sub(3);
    let mut end = target;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Escape a string for use in an HTML attribute value.
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape a string for use in a JSON string value.
fn escape_json_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Format a Unix timestamp as an ISO 8601 date string (UTC).
fn format_timestamp(ts: i64) -> String {
    let secs_per_day: i64 = 86400;
    let days = ts / secs_per_day;
    let remaining = ts % secs_per_day;

    let mut year: i64 = 1970;
    let mut remaining_days = days;

    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days_in_year: i64 = if leap { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month: i64 = 1;
    for &d in &month_days {
        if remaining_days < d {
            break;
        }
        remaining_days -= d;
        month += 1;
    }
    let day = remaining_days + 1;
    let hour = remaining / 3600;
    let minute = (remaining % 3600) / 60;
    let second = remaining % 60;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_two_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 2);
        assert_eq!(perms[0].name, "administer seo");
        assert_eq!(perms[1].name, "edit seo fields");
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/seo");
    }

    #[test]
    fn escape_attr_handles_special_chars() {
        assert_eq!(escape_attr("a&b"), "a&amp;b");
        assert_eq!(escape_attr("a\"b"), "a&quot;b");
        assert_eq!(escape_attr("a<b>c"), "a&lt;b&gt;c");
    }

    #[test]
    fn escape_json_string_handles_special_chars() {
        assert_eq!(escape_json_string("a\"b"), "a\\\"b");
        assert_eq!(escape_json_string("a\\b"), "a\\\\b");
        assert_eq!(escape_json_string("a\nb"), "a\\nb");
    }

    #[test]
    fn format_timestamp_produces_iso8601() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        let result = format_timestamp(1_704_067_200);
        assert!(result.starts_with("2024-01-01T"));
        assert!(result.ends_with('Z'));
    }

    #[test]
    fn truncate_description_under_limit() {
        let short = "Hello world";
        assert_eq!(truncate_description(short, 160), "Hello world");
    }

    #[test]
    fn truncate_description_over_limit() {
        let long = "a".repeat(200);
        let truncated = truncate_description(&long, 160);
        assert_eq!(truncated.len(), 160);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn tap_item_view_produces_json_ld() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "blog".into(),
            title: "Test Post".into(),
            fields: std::collections::HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: Uuid::nil(),
            created: 1_704_067_200,
            changed: 1_704_067_200,
            language: None,
        };
        let output = __inner_tap_item_view(item);
        assert!(output.contains("application/ld+json"));
        assert!(output.contains("\"headline\":\"Test Post\""));
        assert!(output.contains("schema.org"));
    }

    #[test]
    fn tap_item_view_includes_description_from_field() {
        let mut fields = std::collections::HashMap::new();
        fields.insert(
            "field_body".to_string(),
            serde_json::json!({"value": "My content body", "format": "plain_text"}),
        );
        let item = Item {
            id: Uuid::nil(),
            item_type: "page".into(),
            title: "Page".into(),
            fields,
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: Uuid::nil(),
            created: 1_704_067_200,
            changed: 1_704_067_200,
            language: None,
        };
        let output = __inner_tap_item_view(item);
        assert!(output.contains("My content body"));
    }
}
