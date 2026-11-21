#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Cache layer tests.
//!
//! Tests for the cache layer and for cache generations.

mod common;

use common::{run_test, shared_app, user_holding};
use trovato_kernel::cache::CacheLayer;
use trovato_kernel::cache::generation::{CacheKind, bump};
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
    let client = redis::Client::open(trovato_test_utils::env::redis_url()).unwrap();
    let cache = CacheLayer::new(client, Default::default());

    // The statistics come from Redis now that there is no in-process tier.
    assert!(cache.stats().await.redis_keys.is_some());
}

/// A bump clears the cache on the server that made it, too: one server polls
/// the generations like any other and drops its user cache on its own bump.
/// Here the row changes under the cache with no service call, as a write from
/// another server would, and only the bump makes it visible.
#[test]
fn a_bump_clears_this_servers_cache() {
    run_test(async {
        let app = shared_app().await;
        let (id, _) = user_holding(app, "genbump", &[]).await;
        let users = app.state.users();
        let before = users.find_by_id(id).await.unwrap().unwrap().name;

        let renamed = format!("{before}_x");
        sqlx::query("UPDATE users SET name = $1 WHERE id = $2")
            .bind(&renamed)
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(
            users.find_by_id(id).await.unwrap().unwrap().name,
            before,
            "the cache still holds the old row"
        );

        bump(CacheKind::Users);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if users.find_by_id(id).await.unwrap().unwrap().name == renamed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the bump did not clear this server's user cache within 2 s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
}
