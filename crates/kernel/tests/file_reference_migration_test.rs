#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The `index_file_field_references` migration backfills references for File
//! field values stored before the kernel indexed them, promotes the files they
//! name, and is safe to run again.
//!
//! The scratch database is fully migrated first, so the state the migration
//! meets is seeded by hand: an item whose File fields hold a uuid and an array
//! of uuids, with no `file_reference` rows, and the files still `Temporary`.
//! The migration's SQL is then applied twice. The rows it leaves are printed,
//! so a CI log shows them.

use uuid::Uuid;

const MIGRATION: &str =
    include_str!("../migrations/20261120100001_index_file_field_references.sql");

async fn temporary_file(pool: &sqlx::PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) \
         VALUES ($1, $2, $3, $4, 'application/pdf', 4, 0, 1, 1)",
    )
    .bind(id)
    .bind(Uuid::nil())
    .bind(name)
    .bind(format!("local://migrate/{}_{name}", id.simple()))
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn the_migration_indexes_file_field_uuids_and_promotes_their_files() {
    let db = trovato_test_utils::ScratchDb::create("b72_fileref_migration").await;
    let pool = db.pool();
    trovato_kernel::db::run_migrations(pool).await.unwrap();

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ('b72_doc', 'Doc', '', true, 'Title', 'core', $1)",
    )
    .bind(serde_json::json!({ "fields": [
        { "field_name": "field_cover", "field_type": "File", "label": "Cover" },
        { "field_name": "field_pages", "field_type": "File", "label": "Pages", "cardinality": -1 },
        { "field_name": "field_note", "field_type": { "Text": { "max_length": null } }, "label": "Note" }
    ]}))
    .execute(pool)
    .await
    .unwrap();

    let cover = temporary_file(pool, "cover.pdf").await;
    let page = temporary_file(pool, "page.pdf").await;
    let untouched = temporary_file(pool, "unreferenced.pdf").await;
    let noted = temporary_file(pool, "noted.pdf").await;

    let item_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, fields, created, changed, stage_id, language, item_group_id) \
         VALUES ($1, 'b72_doc', 'Seeded', $2, 1, $3, 1, 1, \
                 (SELECT tag_id FROM stage_config WHERE is_default), 'en', $1)",
    )
    .bind(item_id)
    .bind(Uuid::nil())
    .bind(serde_json::json!({
        "field_cover": cover.to_string(),
        "field_pages": [page.to_string(), "not-a-uuid"],
        // A uuid in a field that is not a File field is not a reference.
        "field_note": noted.to_string(),
    }))
    .execute(pool)
    .await
    .unwrap();

    for run in 1..=2 {
        sqlx::raw_sql(MIGRATION)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("migration run {run}: {e}"));
    }

    let refs: Vec<(Uuid, Uuid, String, i16)> = sqlx::query_as(
        "SELECT r.file_id, r.item_id, f.filename, f.status \
         FROM file_reference r JOIN file_managed f ON f.id = r.file_id \
         WHERE r.item_id = $1 ORDER BY f.filename",
    )
    .bind(item_id)
    .fetch_all(pool)
    .await
    .unwrap();
    println!("file_reference after two runs (file_id | item_id | filename | status):");
    for (file_id, item, name, status) in &refs {
        println!("  {file_id} | {item} | {name} | {status}");
    }
    assert_eq!(
        refs.iter().map(|r| r.2.as_str()).collect::<Vec<_>>(),
        vec!["cover.pdf", "page.pdf"],
        "both File field values are referenced, once each, and nothing else"
    );
    assert!(
        refs.iter().all(|r| r.3 == 1),
        "every referenced file is Permanent"
    );

    let statuses: Vec<(String, i16)> = sqlx::query_as(
        "SELECT filename, status FROM file_managed WHERE id = ANY($1) ORDER BY filename",
    )
    .bind(vec![cover, page, untouched, noted])
    .fetch_all(pool)
    .await
    .unwrap();
    println!("file_managed status (0 Temporary, 1 Permanent):");
    for (name, status) in &statuses {
        println!("  {name} | {status}");
    }
    assert_eq!(
        statuses,
        vec![
            ("cover.pdf".to_string(), 1),
            ("noted.pdf".to_string(), 0),
            ("page.pdf".to_string(), 1),
            ("unreferenced.pdf".to_string(), 0),
        ]
    );
}
