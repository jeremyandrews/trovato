#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Config import maintains the file reference index and promotes the files an
//! imported item references.
//!
//! `config import` wrote items with its own `INSERT … ON CONFLICT` and nothing
//! else, so an imported item's File field held a uuid that no `file_reference`
//! row recorded and the file stayed `Temporary`: the six hour cleanup deleted
//! it the same afternoon (Ritrovo `G-CONFIG-IMPORT-NEVER-PROMOTES-A-FILE`).
//!
//! Runs against its own scratch database, as `config_import_test.rs` does,
//! because it imports a config set and runs the temporary file cleanup.

use std::path::PathBuf;
use std::sync::Arc;

use trovato_kernel::config_storage::DirectConfigStorage;
use trovato_kernel::config_storage::yaml::import_config;
use trovato_kernel::file::{FileService, FileStatus, LocalFileStorage};
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("trovato_b72_{label}_{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Insert a temporary `file_managed` row as old as the cleanup cutoff.
async fn old_temporary_file(pool: &sqlx::PgPool, owner: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) \
         VALUES ($1, $2, $3, $4, 'application/pdf', 4, 0, 1, 1)",
    )
    .bind(id)
    .bind(owner)
    .bind(name)
    .bind(format!("local://b72/{}_{name}", id.simple()))
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn imported_item_references_and_promotes_its_file() {
    let db = trovato_test_utils::ScratchDb::create("b72_fileref_import").await;
    let pool = db.pool();
    trovato_kernel::db::run_migrations(pool).await.unwrap();

    let owner = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, 'b72_owner', 'x', 'b72_owner@example.test', 1, false)",
    )
    .bind(owner)
    .execute(pool)
    .await
    .unwrap();

    let referenced = old_temporary_file(pool, owner, "programme.pdf").await;
    let referenced_too = old_temporary_file(pool, owner, "venue.pdf").await;
    let control = old_temporary_file(pool, owner, "stray.pdf").await;

    let dir = TempDir::new("set");
    std::fs::write(
        dir.0.join("item_type.b72_event.yml"),
        "type: b72_event\nlabel: Event\ndescription: null\nhas_title: true\n\
         title_label: Title\nplugin: core\nsettings:\n  fields:\n\
         \x20 - field_name: field_programme\n    field_type: File\n    label: Programme\n\
         \x20 - field_name: field_documents\n    field_type: File\n    label: Documents\n\
         \x20   cardinality: -1\n",
    )
    .unwrap();
    let item_id = Uuid::now_v7();
    std::fs::write(
        dir.0.join(format!("item.{item_id}.yml")),
        format!(
            "id: {item_id}\ntype: b72_event\ntitle: Imported Event\nstatus: 1\n\
             fields:\n  field_programme: {referenced}\n  field_documents:\n  - {referenced_too}\n"
        ),
    )
    .unwrap();

    let storage = DirectConfigStorage::new(pool.clone());
    import_config(&storage, pool, &dir.0, false)
        .await
        .expect("the set imports");

    let refs: Vec<Uuid> = sqlx::query_scalar(
        "SELECT file_id FROM file_reference WHERE item_id = $1 ORDER BY file_id",
    )
    .bind(item_id)
    .fetch_all(pool)
    .await
    .unwrap();
    let mut expected = vec![referenced, referenced_too];
    expected.sort();
    assert_eq!(
        refs, expected,
        "the imported item's File fields are indexed"
    );

    let files_dir = TempDir::new("files");
    let storage: Arc<dyn trovato_kernel::file::FileStorage> =
        Arc::new(LocalFileStorage::new(&files_dir.0, "/files"));
    let files = FileService::new(pool.clone(), storage.clone(), storage);
    for id in [referenced, referenced_too] {
        assert_eq!(
            files.get(id).await.unwrap().map(|f| f.status),
            Some(FileStatus::Permanent),
            "a file an imported item references is permanent"
        );
    }

    files.cleanup_temp_files(0).await.unwrap();
    assert!(
        files.get(control).await.unwrap().is_none(),
        "test control: an unreferenced old temporary file is cleaned up"
    );
    for id in [referenced, referenced_too] {
        assert!(
            files.get(id).await.unwrap().is_some(),
            "the referenced file survives the cleanup"
        );
    }
}
