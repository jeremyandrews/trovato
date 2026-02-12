//! Test fixtures: synthetic item payloads for benchmarking.
//!
//! Generates a 4KB JSON item with 15 fields, nested arrays, and record
//! references to match the Phase 0 benchmark specification.

use serde_json::{Value, json};
use uuid::Uuid;

/// Generate a synthetic 4KB item JSON payload with 15 fields.
///
/// Matches the benchmark spec: text fields, integers, floats, nested
/// arrays, and record references to exercise both serialization modes.
pub fn synthetic_item() -> Value {
    let item_id = Uuid::now_v7();
    let author_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    json!({
        "id": item_id.to_string(),
        "revision_id": revision_id.to_string(),
        "type": "blog",
        "title": "Benchmarking WASM Plugin Architecture in Trovato CMS",
        "author_id": author_id.to_string(),
        "status": 1,
        "created": 1707600000i64,
        "changed": 1707686400i64,
        "promote": 1,
        "sticky": 0,
        "fields": {
            "field_body": {
                "value": "<p>This is a moderately long blog post body that simulates real-world content. It contains multiple paragraphs of text that would be typical for a blog entry on a content management system. The purpose is to create a payload that approximates 4KB when serialized to JSON, which is the target size for our Phase 0 benchmarks.</p><p>The second paragraph adds more content to reach our target payload size. In production, blog posts would typically contain formatted HTML with various elements including links, lists, images, and other rich content. This simulation helps us understand the real-world performance characteristics of our WASM boundary crossing.</p><p>A third paragraph to ensure we have enough content to be representative of actual usage patterns in a production CMS deployment.</p>",
                "format": "filtered_html"
            },
            "field_summary": {
                "value": "A benchmark test post for Phase 0 WASM architecture validation.",
                "format": "plain_text"
            },
            "field_tags": [
                { "target_id": Uuid::now_v7().to_string(), "target_type": "category_term" },
                { "target_id": Uuid::now_v7().to_string(), "target_type": "category_term" },
                { "target_id": Uuid::now_v7().to_string(), "target_type": "category_term" }
            ],
            "field_category": {
                "target_id": Uuid::now_v7().to_string(),
                "target_type": "category_term"
            },
            "field_image": {
                "file_id": Uuid::now_v7().to_string(),
                "alt": "Benchmark test image",
                "title": "Phase 0 Test"
            },
            "field_rating": { "value": 4 },
            "field_views": { "value": 1247 },
            "field_price": { "value": 29.99 },
            "field_published_date": { "value": "2025-02-11" },
            "field_email": { "value": "test@trovato.rs" },
            "field_related": [
                { "target_id": Uuid::now_v7().to_string(), "target_type": "item" },
                { "target_id": Uuid::now_v7().to_string(), "target_type": "item" }
            ],
            "field_metadata": {
                "seo_title": "WASM Benchmarks",
                "seo_description": "Phase 0 architecture validation for Trovato CMS",
                "canonical_url": "https://trovato.rs/blog/wasm-benchmarks",
                "og_image": "https://trovato.rs/images/benchmark.png"
            },
            "field_flags": {
                "featured": true,
                "sponsored": false,
                "allow_comments": true
            },
            "field_word_count": { "value": 342 }
        }
    })
}

/// Return the approximate size of the synthetic item in bytes.
pub fn synthetic_item_size() -> usize {
    serde_json::to_string(&synthetic_item())
        .map(|s| s.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_approximately_4kb() {
        let size = synthetic_item_size();
        assert!(size > 2000, "Payload too small: {size} bytes");
        assert!(size < 8000, "Payload too large: {size} bytes");
        println!("Synthetic item payload size: {size} bytes");
    }
}
