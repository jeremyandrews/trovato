//! How many images in an item's rich text have no text alternative (B251).
//!
//! `item.fields` stores a rich text (`TextLong`) value as `{"value": "<html
//! or text>", "format": "<text format>"}`, the shape [`super::submission`]
//! writes and `routes::item::render_item_fields` reads back to render the
//! item's page. Every other field shape (a plain scalar, a Blocks tree) has
//! no `value` string and contributes nothing here.
//!
//! The scan renders each rich text value through the same [`FilterPipeline`]
//! the item's own page renders it with, then counts with
//! [`trovato_search_index::count_images_missing_alt`], the real HTML parser
//! the static search index already tokenizes pages with repurposed for a
//! different question. Counting after the format's filter runs, not before,
//! matters: `full_html` and `filtered_html` both keep `<img>` through
//! ammonia's sanitizer (it allows the tag and its `alt`, just not a script),
//! so what the viewer actually sees is what gets counted, and `plain_text`
//! HTML-escapes everything, so typed-out angle brackets never count as a tag.

use serde_json::Value;

use super::filter::FilterPipeline;

/// The images with no text alternative across every rich text value in
/// `fields`: no `alt` attribute at all, or one that is only whitespace.
/// `alt=""` is a deliberate decorative marker and is not counted.
pub fn count_missing_alt(fields: &Value) -> i32 {
    let Some(map) = fields.as_object() else {
        return 0;
    };
    let mut total = 0i32;
    for value in map.values() {
        let Some(text) = value.get("value").and_then(Value::as_str) else {
            continue;
        };
        let format = value
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("plain_text");
        let rendered = FilterPipeline::for_format(format).process(text);
        total += trovato_search_index::count_images_missing_alt(&rendered) as i32;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::count_missing_alt;
    use serde_json::json;

    #[test]
    fn an_img_with_no_alt_in_a_rich_text_field_is_counted() {
        let fields = json!({
            "body": {"value": "<p><img src=\"x\"></p>", "format": "filtered_html"},
        });
        assert_eq!(count_missing_alt(&fields), 1);
    }

    #[test]
    fn an_explicit_empty_alt_is_not_counted() {
        let fields = json!({
            "body": {"value": "<img src=\"x\" alt=\"\">", "format": "full_html"},
        });
        assert_eq!(count_missing_alt(&fields), 0);
    }

    #[test]
    fn a_plain_scalar_field_contributes_nothing() {
        let fields = json!({"views": 42, "tags": ["a", "b"]});
        assert_eq!(count_missing_alt(&fields), 0);
    }

    #[test]
    fn plain_text_format_escapes_typed_angle_brackets_so_nothing_is_counted() {
        let fields = json!({
            "body": {"value": "<img src=x>", "format": "plain_text"},
        });
        assert_eq!(count_missing_alt(&fields), 0);
    }

    #[test]
    fn counts_sum_across_fields() {
        let fields = json!({
            "body": {"value": "<img src=\"a\">", "format": "filtered_html"},
            "summary": {"value": "<img src=\"b\"><img src=\"c\" alt=\"ok\">", "format": "filtered_html"},
        });
        assert_eq!(count_missing_alt(&fields), 2);
    }
}
