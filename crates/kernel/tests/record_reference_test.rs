#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11g / D-57 regression: reverse RecordReference resolution is GIN-indexed
//! JSONB containment and **superset-correct** — every referrer is returned, with
//! no silent `LIMIT 50`-per-type cap, for both single-value (scalar UUID string)
//! and multi-value (UUID inside a JSON array) RecordReference storage shapes.

mod common;

use common::{run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use uuid::Uuid;

/// Insert a bare item row directly (no taps / pathauto) with `fields` set.
async fn insert_item(db: &sqlx::PgPool, id: Uuid, title: &str, fields: serde_json::Value) {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, fields, stage_id, language) \
         VALUES ($1, 'conference', $2, $3, 1, $4, $4, $5, $6, 'en')",
    )
    .bind(id)
    .bind(title)
    .bind(Uuid::nil())
    .bind(now)
    .bind(&fields)
    .bind(LIVE_STAGE_ID)
    .execute(db)
    .await
    .expect("insert item");
}

#[test]
fn find_referencing_returns_all_referrers_past_the_former_fifty_cap() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let target = Uuid::now_v7();
        // A field key unique to this test so cleanup and assertions are isolated.
        let field = "field_ref_p11g_d57";
        let mut inserted: Vec<Uuid> = Vec::new();

        // 55 scalar-form referrers (single-value RecordReference) — exceeds the
        // former 50-cap on its own.
        for i in 0..55 {
            let id = Uuid::now_v7();
            inserted.push(id);
            insert_item(
                &app.db,
                id,
                &format!("d57-scalar-{i}"),
                serde_json::json!({ field: target.to_string() }),
            )
            .await;
        }
        // 5 array-form referrers (multi-value RecordReference), each also holding
        // an unrelated id to prove containment matches inside arrays.
        for i in 0..5 {
            let id = Uuid::now_v7();
            inserted.push(id);
            insert_item(
                &app.db,
                id,
                &format!("d57-array-{i}"),
                serde_json::json!({ field: [Uuid::now_v7().to_string(), target.to_string()] }),
            )
            .await;
        }
        // A control that references a *different* target — must be excluded.
        let control = Uuid::now_v7();
        inserted.push(control);
        insert_item(
            &app.db,
            control,
            "d57-control",
            serde_json::json!({ field: Uuid::now_v7().to_string() }),
        )
        .await;

        let found = app
            .state
            .items()
            .find_referencing(Some("conference"), field, target)
            .await
            .expect("find_referencing query");
        let found_ids: std::collections::HashSet<Uuid> = found.iter().map(|i| i.id).collect();

        // All 60 referrers returned — the 50-cap is gone (D-57 correctness fix).
        assert_eq!(
            found.len(),
            60,
            "every referrer past the former 50-cap must be returned"
        );
        // Every referrer (scalar and array form) is present; the control is not.
        for id in inserted.iter().take(60) {
            assert!(found_ids.contains(id), "missing referrer {id}");
        }
        assert!(
            !found_ids.contains(&control),
            "an item referencing a different target must be excluded"
        );

        // Cleanup — the shared app is reused by other tests.
        sqlx::query("DELETE FROM item WHERE id = ANY($1)")
            .bind(&inserted)
            .execute(&app.db)
            .await
            .expect("cleanup inserted items");
    });
}
