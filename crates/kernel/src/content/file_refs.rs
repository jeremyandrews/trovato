//! FR-8 Story 3.5 — extract file references from item field values.
//!
//! Items reference uploaded files as strings embedded in their field values —
//! either the canonical `local://{path}` storage URI (File fields) or the
//! `/files/{path}` public URL (block-editor images, rich-text `<img src>`,
//! page-builder content). Both forms carry the same `{path}` (the file's uri
//! minus the `local://` scheme). This module walks an item's `fields` JSON and
//! returns the set of referenced files as normalized `local://{path}` URIs, so
//! the reference index (`file_reference`) can resolve them against
//! `file_managed.uri`.
//!
//! Extraction is shape-agnostic (it recurses through arbitrary nested
//! objects/arrays) and HTML-aware (it scans *within* each string, so an `<img
//! src="/files/…">` inside a body field is found). It indexes only the two
//! forms the application actually produces; a reference stored under a
//! non-default `FILES_URL` (e.g. an absolute CDN URL) is not matched here — see
//! the module note in `item_service::sync_file_references`.

use std::collections::BTreeSet;

/// Characters permitted inside a file storage path. Scanning stops at the first
/// character outside this set — quotes, angle brackets, whitespace, JSON/HTML
/// punctuation — which cleanly bounds a URI embedded in markup or JSON.
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '~')
}

/// Extract every `local://{path}` and `/files/{path}` reference from `s`,
/// pushing each as a normalized `local://{path}` URI into `out`.
fn scan_string(s: &str, out: &mut BTreeSet<String>) {
    for (marker, keep_prefix) in [("local://", true), ("/files/", false)] {
        let mut rest = s;
        while let Some(pos) = rest.find(marker) {
            let after = &rest[pos + marker.len()..];
            let path: String = after.chars().take_while(|c| is_path_char(*c)).collect();
            // Advance past this marker occurrence regardless of match outcome.
            rest = &after[path.len()..];
            if path.is_empty() || path.starts_with('/') || path.contains("..") {
                continue;
            }
            if keep_prefix {
                out.insert(format!("local://{path}"));
            } else {
                // `/files/{path}` public URL → the canonical storage URI.
                out.insert(format!("local://{path}"));
            }
        }
    }
}

fn walk(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(s) => scan_string(s, out),
        serde_json::Value::Array(arr) => arr.iter().for_each(|v| walk(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| walk(v, out)),
        _ => {}
    }
}

/// Return the distinct set of files referenced by an item's `fields`, as
/// normalized `local://{path}` URIs (sorted, deduplicated).
pub(crate) fn extract_file_uris(fields: &serde_json::Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    walk(fields, &mut out);
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_local_uri_from_file_field() {
        let fields = serde_json::json!({
            "field_attachment": { "uri": "local://tenant/2026/02/abc123_report.pdf" }
        });
        assert_eq!(
            extract_file_uris(&fields),
            vec!["local://tenant/2026/02/abc123_report.pdf".to_string()]
        );
    }

    #[test]
    fn extracts_public_url_from_html_body() {
        let fields = serde_json::json!({
            "field_body": {
                "value": "<p>see <img src=\"/files/2026/02/def456_photo.jpg\" alt=\"x\"></p>"
            }
        });
        assert_eq!(
            extract_file_uris(&fields),
            vec!["local://2026/02/def456_photo.jpg".to_string()]
        );
    }

    #[test]
    fn recurses_arrays_and_blocks_and_dedupes() {
        let fields = serde_json::json!({
            "blocks": [
                { "type": "image", "data": { "file": { "url": "/files/2026/02/a_1.png" } } },
                { "type": "image", "data": { "file": { "url": "/files/2026/02/a_1.png" } } },
                { "type": "gallery", "images": [
                    "local://2026/02/b_2.png",
                    "/files/2026/03/c_3.png"
                ]}
            ]
        });
        assert_eq!(
            extract_file_uris(&fields),
            vec![
                "local://2026/02/a_1.png".to_string(),
                "local://2026/02/b_2.png".to_string(),
                "local://2026/03/c_3.png".to_string(),
            ]
        );
    }

    #[test]
    fn ignores_traversal_and_non_file_strings() {
        let fields = serde_json::json!({
            "a": "just some text with no files",
            "b": "https://example.com/page",
            "c": "local://../etc/passwd",
            "d": "/files/../secret",
        });
        assert!(extract_file_uris(&fields).is_empty());
    }

    #[test]
    fn empty_or_scalar_fields_yield_nothing() {
        assert!(extract_file_uris(&serde_json::json!({})).is_empty());
        assert!(extract_file_uris(&serde_json::Value::Null).is_empty());
        assert!(extract_file_uris(&serde_json::json!(42)).is_empty());
    }
}
