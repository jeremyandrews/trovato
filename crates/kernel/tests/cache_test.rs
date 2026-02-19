#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Cache layer tests.
//!
//! Tests for Phase 6A two-tier cache functionality.

use trovato_kernel::cache::CacheLayer;

#[test]
fn test_stage_key_live() {
    // Live stage uses bare keys
    assert_eq!(CacheLayer::stage_key("item:123", None), "item:123");
    assert_eq!(CacheLayer::stage_key("item:123", Some("live")), "item:123");
}

#[test]
fn test_stage_key_non_live() {
    // Non-live stages get prefixed
    assert_eq!(
        CacheLayer::stage_key("item:123", Some("preview-abc")),
        "st:preview-abc:item:123"
    );
    assert_eq!(
        CacheLayer::stage_key("gather:view", Some("staging")),
        "st:staging:gather:view"
    );
}

#[test]
fn test_stage_key_preserves_colons() {
    // Keys with existing colons work correctly
    assert_eq!(
        CacheLayer::stage_key("item:type:page:123", Some("preview")),
        "st:preview:item:type:page:123"
    );
}

#[test]
fn test_stage_key_empty_key() {
    // Empty keys work (edge case)
    assert_eq!(CacheLayer::stage_key("", None), "");
    assert_eq!(CacheLayer::stage_key("", Some("preview")), "st:preview:");
}

#[tokio::test]
async fn test_cache_layer_creation() {
    // This test verifies the CacheLayer can be created
    // Actual caching tests require Redis
    let client = redis::Client::open("redis://127.0.0.1:6379").unwrap();
    let cache = CacheLayer::new(client);

    let stats = cache.stats().await;
    assert_eq!(stats.l1_entry_count, 0);
    assert_eq!(stats.l1_weighted_size, 0);
}
