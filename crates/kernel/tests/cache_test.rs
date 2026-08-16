#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Cache layer tests.
//!
//! Tests for Phase 6A two-tier cache functionality.

use trovato_kernel::cache::CacheLayer;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use uuid::Uuid;

#[test]
fn test_stage_key_live() {
    // Live stage uses bare keys
    assert_eq!(CacheLayer::stage_key("item:123", None), "item:123");
    assert_eq!(
        CacheLayer::stage_key("item:123", Some(LIVE_STAGE_ID)),
        "item:123"
    );
}

#[test]
fn test_stage_key_non_live() {
    // Non-live stages get prefixed with UUID
    let preview = Uuid::now_v7();
    let staging = Uuid::now_v7();
    assert_eq!(
        CacheLayer::stage_key("item:123", Some(preview)),
        format!("st:{preview}:item:123")
    );
    assert_eq!(
        CacheLayer::stage_key("gather:view", Some(staging)),
        format!("st:{staging}:gather:view")
    );
}

#[test]
fn test_stage_key_preserves_colons() {
    // Keys with existing colons work correctly
    let preview = Uuid::now_v7();
    assert_eq!(
        CacheLayer::stage_key("item:type:page:123", Some(preview)),
        format!("st:{preview}:item:type:page:123")
    );
}

#[test]
fn test_stage_key_empty_key() {
    // Empty keys work (edge case)
    let preview = Uuid::now_v7();
    assert_eq!(CacheLayer::stage_key("", None), "");
    assert_eq!(
        CacheLayer::stage_key("", Some(preview)),
        format!("st:{preview}:")
    );
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
