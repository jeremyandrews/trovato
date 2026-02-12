//! Test fixtures: synthetic item payloads for benchmarking.
//!
//! Generates JSON item payloads of configurable sizes with realistic
//! field structures for Phase 0 benchmark validation.

use serde_json::{Value, json};
use uuid::Uuid;

/// Payload size presets for benchmarking.
#[derive(Debug, Clone, Copy)]
pub enum PayloadSize {
    /// ~2.4KB - default Phase 0 benchmark
    Small,
    /// ~10KB - medium content
    Medium,
    /// ~50KB - large content with many fields
    Large,
    /// ~100KB - very large content (stress test)
    XLarge,
}

impl PayloadSize {
    /// Target size in bytes for this payload preset.
    pub fn target_bytes(&self) -> usize {
        match self {
            PayloadSize::Small => 2_400,
            PayloadSize::Medium => 10_000,
            PayloadSize::Large => 50_000,
            PayloadSize::XLarge => 100_000,
        }
    }

    /// Human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            PayloadSize::Small => "small (~2.4KB)",
            PayloadSize::Medium => "medium (~10KB)",
            PayloadSize::Large => "large (~50KB)",
            PayloadSize::XLarge => "xlarge (~100KB)",
        }
    }
}

impl std::str::FromStr for PayloadSize {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "small" | "s" | "2k" => Ok(PayloadSize::Small),
            "medium" | "m" | "10k" => Ok(PayloadSize::Medium),
            "large" | "l" | "50k" => Ok(PayloadSize::Large),
            "xlarge" | "xl" | "100k" => Ok(PayloadSize::XLarge),
            _ => Err(format!("Unknown payload size: {}. Use: small, medium, large, xlarge", s)),
        }
    }
}

/// Generate a synthetic item JSON payload of the default size (~2.4KB).
pub fn synthetic_item() -> Value {
    synthetic_item_sized(PayloadSize::Small)
}

/// Generate a synthetic item JSON payload of the specified size.
pub fn synthetic_item_sized(size: PayloadSize) -> Value {
    let item_id = Uuid::now_v7();
    let author_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    // Base paragraph for body content (~500 chars)
    let base_paragraph = "<p>This is a moderately long blog post body that simulates real-world content. It contains multiple paragraphs of text that would be typical for a blog entry on a content management system. The purpose is to create a payload that reaches our target size when serialized to JSON. In production, blog posts would typically contain formatted HTML with various elements including links, lists, images, and other rich content.</p>";

    // Generate body content based on target size
    let body_paragraphs = match size {
        PayloadSize::Small => 2,
        PayloadSize::Medium => 15,
        PayloadSize::Large => 80,
        PayloadSize::XLarge => 170,
    };
    let body = (0..body_paragraphs)
        .map(|_| base_paragraph)
        .collect::<Vec<_>>()
        .join("");

    // Generate additional array items for larger payloads
    let tag_count = match size {
        PayloadSize::Small => 3,
        PayloadSize::Medium => 10,
        PayloadSize::Large => 50,
        PayloadSize::XLarge => 100,
    };
    let tags: Vec<Value> = (0..tag_count)
        .map(|_| json!({ "target_id": Uuid::now_v7().to_string(), "target_type": "category_term" }))
        .collect();

    let related_count = match size {
        PayloadSize::Small => 2,
        PayloadSize::Medium => 5,
        PayloadSize::Large => 25,
        PayloadSize::XLarge => 50,
    };
    let related: Vec<Value> = (0..related_count)
        .map(|_| json!({ "target_id": Uuid::now_v7().to_string(), "target_type": "item" }))
        .collect();

    // Generate additional custom fields for larger payloads
    let mut fields = json!({
        "field_body": {
            "value": body,
            "format": "filtered_html"
        },
        "field_summary": {
            "value": "A benchmark test post for Phase 0 WASM architecture validation.",
            "format": "plain_text"
        },
        "field_tags": tags,
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
        "field_related": related,
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
    });

    // Add extra fields for larger payloads
    let extra_field_count = match size {
        PayloadSize::Small => 0,
        PayloadSize::Medium => 5,
        PayloadSize::Large => 20,
        PayloadSize::XLarge => 40,
    };
    for i in 0..extra_field_count {
        fields.as_object_mut().unwrap().insert(
            format!("field_extra_{}", i),
            json!({
                "value": format!("Extra field value {} with some additional text to increase payload size", i),
                "metadata": {
                    "created": 1707600000i64,
                    "updated": 1707686400i64,
                    "author": Uuid::now_v7().to_string()
                }
            })
        );
    }

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
        "fields": fields
    })
}

/// Generate a synthetic item JSON string of the specified size.
pub fn synthetic_item_json(size: PayloadSize) -> String {
    serde_json::to_string(&synthetic_item_sized(size)).unwrap()
}

/// Get the actual size of a payload preset.
pub fn synthetic_item_size_for(size: PayloadSize) -> usize {
    synthetic_item_json(size).len()
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
    fn payload_small_is_approximately_2kb() {
        let size = synthetic_item_size_for(PayloadSize::Small);
        println!("Small payload size: {size} bytes");
        assert!(size > 1_500, "Payload too small: {size} bytes");
        assert!(size < 5_000, "Payload too large: {size} bytes");
    }

    #[test]
    fn payload_medium_is_approximately_10kb() {
        let size = synthetic_item_size_for(PayloadSize::Medium);
        println!("Medium payload size: {size} bytes");
        assert!(size > 8_000, "Payload too small: {size} bytes");
        assert!(size < 15_000, "Payload too large: {size} bytes");
    }

    #[test]
    fn payload_large_is_approximately_50kb() {
        let size = synthetic_item_size_for(PayloadSize::Large);
        println!("Large payload size: {size} bytes");
        assert!(size > 40_000, "Payload too small: {size} bytes");
        assert!(size < 70_000, "Payload too large: {size} bytes");
    }

    #[test]
    fn payload_xlarge_is_approximately_100kb() {
        let size = synthetic_item_size_for(PayloadSize::XLarge);
        println!("XLarge payload size: {size} bytes");
        assert!(size > 80_000, "Payload too small: {size} bytes");
        assert!(size < 130_000, "Payload too large: {size} bytes");
    }
}
