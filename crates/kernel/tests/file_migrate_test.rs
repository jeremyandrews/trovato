#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `trovato files migrate`: an end-to-end local-to-S3 move against the
//! in-process mock S3 server (`common::mock_s3`), proving size/checksum
//! verification, the uri rewrite, and that an interrupted run resumes
//! correctly — picking up only what is left, never reprocessing or
//! duplicating a file already finished.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::mock_s3::{self, MockS3};
use sqlx::Row;
use trovato_kernel::file::{FileStorage, LocalFileStorage, MigrationBackends, S3Settings, S3Storage, migrate_files};
use uuid::Uuid;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("trovato-migrate-test-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn s3_for(mock: &MockS3, key_prefix: &str) -> Arc<dyn FileStorage> {
    Arc::new(
        S3Storage::new(S3Settings {
            endpoint: mock.endpoint(),
            region: mock_s3::REGION.to_string(),
            bucket: mock_s3::BUCKET.to_string(),
            key_prefix: key_prefix.to_string(),
            path_style: true,
            access_key_id: mock_s3::ACCESS_KEY_ID.to_string(),
            secret_access_key: mock_s3::SECRET_ACCESS_KEY.to_string(),
            public_base_url: None,
            presign_ttl: Duration::from_secs(60),
        })
        .unwrap(),
    )
}

async fn insert_user(db: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, is_admin) VALUES ($1, $2, 'x', $3, false)",
    )
    .bind(id)
    .bind(format!("migrate_test_{}", id.simple()))
    .bind(format!("{}@example.test", id.simple()))
    .execute(db)
    .await
    .expect("insert test user");
    id
}

async fn insert_file(
    db: &sqlx::PgPool,
    owner: Uuid,
    id: Uuid,
    uri: &str,
    size: i64,
    visibility: i16,
) {
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, \
         visibility, created, changed) VALUES ($1, $2, 'x.bin', $3, 'application/octet-stream', \
         $4, 1, $5, 0, 0)",
    )
    .bind(id)
    .bind(owner)
    .bind(uri)
    .bind(size)
    .bind(visibility)
    .execute(db)
    .await
    .expect("insert test file row");
}

async fn uri_of(db: &sqlx::PgPool, id: Uuid) -> String {
    sqlx::query("SELECT uri FROM file_managed WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn moves_verifies_and_rewrites_local_files_to_s3() {
    let db = trovato_test_utils::ScratchDb::create("file_migrate_basic").await;
    trovato_kernel::db::run_migrations(db.pool())
        .await
        .expect("migrate scratch database");

    let mock = MockS3::start().await;
    let local_dir = TempDir::new();
    let local: Arc<dyn FileStorage> = Arc::new(LocalFileStorage::new(&local_dir.0, "/files"));
    let s3_public = s3_for(&mock, "public/");
    let s3_private = s3_for(&mock, "private/");

    let owner = insert_user(db.pool()).await;
    let id_a = Uuid::now_v7();
    let id_b = Uuid::now_v7();
    let uri_a = format!("local://2026/01/{id_a}_a.bin");
    let uri_b = format!("local://2026/01/{id_b}_b.bin");
    local.write(&uri_a, b"public payload").await.unwrap();
    local.write(&uri_b, b"private payload, a bit longer").await.unwrap();
    insert_file(db.pool(), owner, id_a, &uri_a, 14, 0).await;
    insert_file(db.pool(), owner, id_b, &uri_b, 29, 1).await;

    let backends = MigrationBackends {
        public_from: local.clone(),
        public_to: s3_public.clone(),
        private_from: local.clone(),
        private_to: s3_private.clone(),
    };

    let summary = migrate_files(db.pool(), "local", "s3", &backends, |_| {})
        .await
        .expect("migrate");
    assert_eq!(summary.moved, 2);
    assert_eq!(summary.already_done_elsewhere, 0);

    let new_uri_a = uri_of(db.pool(), id_a).await;
    let new_uri_b = uri_of(db.pool(), id_b).await;
    assert_eq!(new_uri_a, "s3://2026/01/".to_string() + &format!("{id_a}_a.bin"));
    assert_eq!(new_uri_b, "s3://2026/01/".to_string() + &format!("{id_b}_b.bin"));

    // The public file landed under the public prefix, the private one under
    // the private prefix — the two schemes do not collide in one bucket.
    assert_eq!(
        mock.object(&format!("public/2026/01/{id_a}_a.bin")).unwrap(),
        b"public payload"
    );
    assert_eq!(
        mock.object(&format!("private/2026/01/{id_b}_b.bin")).unwrap(),
        b"private payload, a bit longer"
    );

    // The source copies are gone.
    assert!(!local.exists(&uri_a).await.unwrap());
    assert!(!local.exists(&uri_b).await.unwrap());
}

#[tokio::test]
async fn resumes_after_a_simulated_interruption_without_reprocessing_finished_files() {
    let db = trovato_test_utils::ScratchDb::create("file_migrate_resume").await;
    trovato_kernel::db::run_migrations(db.pool())
        .await
        .expect("migrate scratch database");

    let mock = MockS3::start().await;
    let local_dir = TempDir::new();
    let local: Arc<dyn FileStorage> = Arc::new(LocalFileStorage::new(&local_dir.0, "/files"));
    let s3 = s3_for(&mock, "public/");

    let owner = insert_user(db.pool()).await;
    let id_done = Uuid::now_v7();
    let id_pending = Uuid::now_v7();

    // `id_done` is prepared as if a prior run already finished it: its row
    // already points at `s3://`, its bytes are on the S3 side, AND (the
    // worst case a crash between the DB commit and the source delete can
    // leave) its bytes are STILL also present on the local side. A rerun
    // must not touch it again — not reprocess it, not re-delete anything,
    // not count it as moved.
    let done_uri_local = format!("local://2026/02/{id_done}_done.bin");
    let done_uri_s3 = format!("s3://2026/02/{id_done}_done.bin");
    local.write(&done_uri_local, b"already migrated").await.unwrap();
    s3.write(&done_uri_s3, b"already migrated").await.unwrap();
    insert_file(db.pool(), owner, id_done, &done_uri_s3, 17, 0).await;

    // `id_pending` is a genuinely unfinished file: still on local only.
    let pending_uri = format!("local://2026/02/{id_pending}_pending.bin");
    local.write(&pending_uri, b"not yet moved").await.unwrap();
    insert_file(db.pool(), owner, id_pending, &pending_uri, 13, 0).await;

    let backends = MigrationBackends {
        public_from: local.clone(),
        public_to: s3.clone(),
        private_from: local.clone(),
        private_to: s3.clone(),
    };

    let summary = migrate_files(db.pool(), "local", "s3", &backends, |_| {})
        .await
        .expect("resumed migrate");
    assert_eq!(
        summary.moved, 1,
        "only the genuinely unfinished file is processed"
    );

    assert_eq!(uri_of(db.pool(), id_done).await, done_uri_s3, "untouched");
    assert_eq!(
        local.read(&done_uri_local).await.unwrap(),
        b"already migrated".to_vec(),
        "the orphaned local copy of an already-finished file is left alone, not deleted again"
    );

    let new_pending_uri = uri_of(db.pool(), id_pending).await;
    assert_eq!(new_pending_uri, format!("s3://2026/02/{id_pending}_pending.bin"));
    assert_eq!(
        s3.read(&new_pending_uri).await.unwrap(),
        b"not yet moved".to_vec()
    );
    assert!(
        !local.exists(&pending_uri).await.unwrap(),
        "the newly migrated file's source copy is cleaned up"
    );
}
