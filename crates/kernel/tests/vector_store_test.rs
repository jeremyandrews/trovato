//! Integration tests for the VectorStore service.
//!
//! These tests check that PgVectorStore handles both the pgvector-available
//! and pgvector-unavailable cases gracefully. The pgvector extension may
//! not be installed in the CI or local test database.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{run_test, shared_app};
use trovato_kernel::services::vector_store::{PgVectorStore, VectorStore};
use uuid::Uuid;

#[test]
fn pgvector_availability_check_does_not_panic() {
    run_test(async {
        let app = shared_app().await;
        let store = PgVectorStore::new(app.db.clone()).await;

        // This should return true or false, never panic
        let available = store.is_available().await;
        println!("pgvector available: {available}");
    });
}

#[test]
fn store_embedding_graceful_when_unavailable() {
    run_test(async {
        let app = shared_app().await;
        let store = PgVectorStore::new(app.db.clone()).await;

        if store.is_available().await {
            println!("pgvector is available — testing real storage");

            // Create a test item first
            let item_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO item (id, type, title, author_id, status, created, changed, fields) \
                 VALUES ($1, 'test_vec', 'Vector Test', $2, 0, 0, 0, '{}')",
            )
            .bind(item_id)
            .bind(Uuid::nil())
            .execute(&app.db)
            .await
            .unwrap();

            let embedding = vec![0.1_f32, 0.2, 0.3, 0.4];
            let result = store
                .store_embedding(item_id, "body", "test-model", &embedding)
                .await;
            assert!(result.is_ok(), "store_embedding failed: {result:?}");

            // Search should find it
            let results = store
                .similarity_search(&[0.1, 0.2, 0.3, 0.4], "test-model", 10)
                .await
                .unwrap();
            assert!(
                results.iter().any(|r| r.item_id == item_id),
                "expected to find stored embedding"
            );

            // Delete
            let deleted = store.delete_embeddings(item_id).await.unwrap();
            assert!(deleted > 0);

            // Clean up test item
            sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(item_id)
                .execute(&app.db)
                .await
                .ok();
        } else {
            println!("pgvector not available — testing graceful degradation");

            let embedding = vec![0.1_f32, 0.2, 0.3];
            let result = store
                .store_embedding(Uuid::nil(), "body", "test-model", &embedding)
                .await;
            assert!(result.is_err(), "should fail when pgvector unavailable");

            let results = store
                .similarity_search(&[0.1, 0.2, 0.3], "test-model", 10)
                .await
                .unwrap();
            assert!(results.is_empty(), "should return empty without pgvector");

            let deleted = store.delete_embeddings(Uuid::nil()).await.unwrap();
            assert_eq!(deleted, 0);

            let stale = store.mark_stale("old-model").await.unwrap();
            assert_eq!(stale, 0);
        }
    });
}

#[test]
fn mark_stale_returns_zero_when_no_embeddings() {
    run_test(async {
        let app = shared_app().await;
        let store = PgVectorStore::new(app.db.clone()).await;

        // Even if pgvector is available, marking a nonexistent model returns 0
        let count = store.mark_stale("nonexistent-model-xyz").await.unwrap();
        assert_eq!(count, 0);
    });
}
