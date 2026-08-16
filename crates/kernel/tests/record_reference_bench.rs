#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11g volume benchmarks (run explicitly: `cargo test --test record_reference_bench
//! -- --ignored --nocapture`). Ignored by default so CI's timing is unaffected;
//! they exist to document the D-57 reverse-reference and the Epic-3
//! access-filtered-gather numbers the P11g report cites.

mod common;

use std::collections::HashMap;
use std::time::Instant;

use common::shared_app;
use trovato_kernel::gather::{QueryContext, QueryDefinition, QueryDisplay};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TOTAL_ROWS: i64 = 5000;
const REFERRERS: i64 = 500; // every 10th row references the target
const TITLE_PREFIX: &str = "p11gbench";

/// Bulk-insert `TOTAL_ROWS` conference items (no taps / pathauto), of which every
/// 10th carries a RecordReference to `target` in `field_ref`.
async fn seed_volume(db: &sqlx::PgPool, target: Uuid) {
    sqlx::query(
        "INSERT INTO item \
         (id, type, title, author_id, status, created, changed, fields, stage_id, language) \
         SELECT gen_random_uuid(), 'conference', $1 || '-' || g, $2, 1, 0, 0, \
                CASE WHEN g % 10 = 0 THEN jsonb_build_object('field_ref', $3::text) \
                     ELSE '{}'::jsonb END, \
                $4, 'en' \
         FROM generate_series(1, $5) g",
    )
    .bind(TITLE_PREFIX)
    .bind(Uuid::nil())
    .bind(target.to_string())
    .bind(LIVE_STAGE_ID)
    .bind(TOTAL_ROWS)
    .execute(db)
    .await
    .expect("bulk seed");
}

async fn cleanup(db: &sqlx::PgPool) {
    sqlx::query("DELETE FROM item WHERE title LIKE $1")
        .bind(format!("{TITLE_PREFIX}-%"))
        .execute(db)
        .await
        .expect("cleanup");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "volume benchmark; run with --ignored --nocapture"]
async fn bench_d57_reverse_reference_at_volume() {
    let app = shared_app().await;
    app.ensure_conference_type().await;
    let target = Uuid::now_v7();
    cleanup(&app.db).await;
    seed_volume(&app.db, target).await;

    // Warm + measured runs of the GIN-indexed containment reverse lookup.
    let mut best = f64::MAX;
    let mut found = 0usize;
    for _ in 0..5 {
        let t = Instant::now();
        let refs = app
            .state
            .items()
            .find_referencing(Some("conference"), "field_ref", target)
            .await
            .expect("find_referencing");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        best = best.min(ms);
        found = refs.len();
    }
    println!(
        "[D-57] find_referencing over {TOTAL_ROWS} rows ({REFERRERS} referrers): \
         {found} returned, best {best:.2} ms (GIN idx_item_fields containment)"
    );
    assert_eq!(
        found as i64, REFERRERS,
        "superset-correct: every referrer returned, no 50-cap"
    );

    cleanup(&app.db).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "volume benchmark; run with --ignored --nocapture"]
async fn bench_access_filtered_gather_at_volume() {
    let app = shared_app().await;
    app.ensure_conference_type().await;
    let target = Uuid::now_v7();
    cleanup(&app.db).await;
    seed_volume(&app.db, target).await;

    let def = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some("conference".to_string()),
        ..Default::default()
    };
    let display = QueryDisplay {
        items_per_page: 25,
        ..Default::default()
    };

    // Measure both viewer tiers over the same 5000-row corpus:
    //  - anon exercises the SQL status predicate + the over-fetch/backfill loop
    //    + the per-type field seam (the Epic-3 access-filtered path);
    //  - admin bypasses per-item access (the SQL-fetch + projection baseline).
    for (label, viewer) in [
        ("anon-nogrant", UserContext::anonymous()),
        (
            "auth-viewer",
            UserContext::authenticated(
                Uuid::now_v7(),
                vec![
                    "access content".to_string(),
                    "view any conference".to_string(),
                ],
            ),
        ),
        (
            "admin",
            UserContext::authenticated(Uuid::now_v7(), vec!["administer site".to_string()]),
        ),
    ] {
        let ctx = QueryContext {
            current_user_id: None,
            viewer: Some(viewer),
            url_args: HashMap::new(),
            language: None,
        };
        let mut best = f64::MAX;
        let mut page_len = 0usize;
        let mut total = 0u64;
        for _ in 0..5 {
            let t = Instant::now();
            let result = app
                .state
                .gather()
                .execute_definition(&def, &display, 1, HashMap::new(), LIVE_STAGE_ID, &ctx)
                .await
                .expect("gather executes");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            best = best.min(ms);
            page_len = result.items.len();
            total = result.total;
        }
        println!(
            "[Epic-3] {label} conference gather over {TOTAL_ROWS} rows: \
             page {page_len}/25, count {total}, best {best:.2} ms"
        );
    }

    cleanup(&app.db).await;
}
