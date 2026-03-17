//! CFP (Call for Papers) deadline badge plugin for Ritrovo conferences.
//!
//! Displays countdown badges on conference items when a CFP deadline is set:
//! - Green badge: more than 14 days remaining
//! - Yellow badge: 7--14 days remaining
//! - Red badge: less than 7 days remaining
//! - No badge: deadline has passed

use trovato_sdk::prelude::*;

/// Render a CFP badge when viewing a conference with an active CFP deadline.
///
/// Reads `field_cfp_end_date` from the item fields. If the date is in the
/// future, injects an HTML badge indicating how much time remains.
#[plugin_tap]
pub fn tap_item_view(item: Item) -> String {
    // Only process conference items
    if item.item_type != "conference" {
        return String::new();
    }

    // Extract CFP end date (Unix timestamp)
    let cfp_end = match item.fields.get("field_cfp_end_date") {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => s.parse::<i64>().unwrap_or(0),
        _ => return String::new(),
    };

    if cfp_end == 0 {
        return String::new();
    }

    // Current time from item's changed timestamp as rough proxy
    // (plugins don't have access to system clock directly)
    let now = item.changed;
    let remaining_secs = cfp_end - now;

    if remaining_secs <= 0 {
        return String::new();
    }

    let remaining_days = remaining_secs / 86400;

    let (color_class, label) = if remaining_days > 14 {
        (
            "cfp-badge--open",
            format!("CFP Open \u{2014} {remaining_days} days left"),
        )
    } else if remaining_days > 7 {
        (
            "cfp-badge--closing",
            format!("CFP Closing \u{2014} {remaining_days} days left"),
        )
    } else if remaining_days > 1 {
        (
            "cfp-badge--urgent",
            format!("CFP Urgent \u{2014} {remaining_days} days left"),
        )
    } else {
        ("cfp-badge--urgent", "CFP Closes Today!".to_string())
    };

    format!(r#"<span class="cfp-badge {color_class}">{label}</span>"#,)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_conference(cfp_end: i64, changed: i64) -> Item {
        let mut fields = HashMap::new();
        fields.insert(
            "field_cfp_end_date".to_string(),
            serde_json::Value::Number(serde_json::Number::from(cfp_end)),
        );
        Item {
            id: Uuid::nil(),
            item_type: "conference".to_string(),
            title: "Test Conf".to_string(),
            fields,
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed,
        }
    }

    #[test]
    fn no_badge_for_non_conference() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "blog".to_string(),
            title: "Test".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
        };
        assert!(__inner_tap_item_view(item).is_empty());
    }

    #[test]
    fn no_badge_when_no_cfp_date() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "conference".to_string(),
            title: "Test Conf".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
        };
        assert!(__inner_tap_item_view(item).is_empty());
    }

    #[test]
    fn green_badge_more_than_14_days() {
        let now = 1_000_000;
        let cfp_end = now + 20 * 86400; // 20 days later
        let item = make_conference(cfp_end, now);
        let result = __inner_tap_item_view(item);
        assert!(result.contains("cfp-badge--open"));
        assert!(result.contains("20 days left"));
    }

    #[test]
    fn yellow_badge_7_to_14_days() {
        let now = 1_000_000;
        let cfp_end = now + 10 * 86400; // 10 days later
        let item = make_conference(cfp_end, now);
        let result = __inner_tap_item_view(item);
        assert!(result.contains("cfp-badge--closing"));
    }

    #[test]
    fn red_badge_less_than_7_days() {
        let now = 1_000_000;
        let cfp_end = now + 3 * 86400; // 3 days later
        let item = make_conference(cfp_end, now);
        let result = __inner_tap_item_view(item);
        assert!(result.contains("cfp-badge--urgent"));
    }

    #[test]
    fn no_badge_past_deadline() {
        let now = 1_000_000;
        let cfp_end = now - 86400; // 1 day ago
        let item = make_conference(cfp_end, now);
        assert!(__inner_tap_item_view(item).is_empty());
    }
}
