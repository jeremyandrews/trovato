#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `trovato import`'s `file` record (B184): a file a port has already copied
//! into the files directory, registered under the id, owner and date the
//! record gives, with no second upload of the bytes.
//!
//! These pin the importer's own checks: a record registers and the bytes it
//! names are servable, a rerun of an identical record changes nothing, an
//! item can reference the file exactly as it references an upload, and a path
//! that escapes the files directory or names nothing on disk fails that line
//! — inside the one transaction the rest of the import already uses — rather
//! than writing a partial result.
//!
//! Image derivatives of an imported file are covered in
//! `file_serving_access_test.rs`, which already builds the `trovato_image_styles`
//! fixture these need.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use serde_json::json;
use trovato_kernel::import::{ImportFailed, import};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const FILE_BODY: &[u8] = b"a file a migration already copied onto disk";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// Insert a real (non-admin) user and return its id — a valid `owner_id`
/// (`file_managed.owner_id` has a FK to `users`). Removed when the test ends.
async fn create_user(app: &TestApp) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(format!("b184owner{}", id.simple()))
    .bind(format!("b184-{}@example.test", id.simple()))
    .execute(&app.db)
    .await
    .expect("seed owner user");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

/// A fresh relative path under the files directory, in its own scratch
/// subdirectory so parallel tests, and reruns of the same test, never collide.
fn scratch_path(name: &str) -> String {
    format!("b184-test/{}/{name}", Uuid::now_v7().simple())
}

/// Write `data` at `rel`, inside the shared app's files directory, creating
/// its scratch subdirectory. Removed, subdirectory included, when the test ends.
async fn place_file(app: &TestApp, rel: &str, data: &[u8]) {
    let root = app
        .state
        .files()
        .local_dir()
        .expect("tests run against a local files directory");
    let full = root.join(rel);
    let dir = full.parent().expect("a parent directory").to_path_buf();
    tokio::fs::create_dir_all(&dir)
        .await
        .expect("create the scratch subdirectory");
    tokio::fs::write(&full, data)
        .await
        .expect("write the scratch file");
    common::defer_cleanup(move || async move {
        let _ = tokio::fs::remove_dir_all(&dir).await;
    });
}

/// Delete a `file_managed` row, and anything referencing it, when the test ends.
fn forget_file(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM file_reference WHERE file_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM file_managed WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

fn file_line(
    id: Uuid,
    owner: Uuid,
    path: &str,
    filename: &str,
    mime: &str,
    created: i64,
) -> serde_json::Value {
    json!({
        "type": "file", "id": id, "owner_id": owner, "path": path,
        "filename": filename, "mime_type": mime, "created": created,
    })
}

async fn run_lines(
    app: &TestApp,
    lines: &[serde_json::Value],
) -> anyhow::Result<trovato_kernel::import::ImportReport> {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    import(&app.state, text.as_bytes(), false).await
}

#[derive(Debug, sqlx::FromRow, PartialEq)]
struct StoredFileRow {
    owner_id: Uuid,
    filename: String,
    uri: String,
    filemime: String,
    filesize: i64,
    status: i16,
    created: i64,
}

async fn stored_file(app: &TestApp, id: Uuid) -> Option<StoredFileRow> {
    sqlx::query_as(
        "SELECT owner_id, filename, uri, filemime, filesize, status, created \
         FROM file_managed WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await
    .expect("look up the file")
}

/// A `file` record registers the bytes already on disk, and the plain file
/// route serves them at their path — the same route an uploaded file is
/// served from, with no re-upload and no copy.
#[test]
fn a_file_record_registers_and_serves() {
    run_test(async {
        let app = shared_app().await;
        // A file no item references yet is servable only to its owner and
        // admins (the any-referencing policy, D-29) — logged in as the owner,
        // not anonymous, so this test proves the route serves the bytes
        // without also re-proving that unrelated access policy.
        let name = common::username("b184owner");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.test"))
            .await;
        let owner = common::user_id_of(app, &name).await;
        let cookies = app.login(&name, "test-password-123").await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(owner)
                .execute(&pool)
                .await;
        });
        let id = Uuid::now_v7();
        let path = scratch_path("report.txt");
        place_file(app, &path, FILE_BODY).await;
        forget_file(app, id);

        let report = run_lines(
            app,
            &[file_line(
                id,
                owner,
                &path,
                "report.txt",
                "text/plain",
                1_554_112_800,
            )],
        )
        .await
        .expect("import");
        assert_eq!(report.of("file").created, 1, "{report:?}");
        assert_eq!(report.changes(), 1);

        let row = stored_file(app, id).await.expect("the file is stored");
        assert_eq!(row.owner_id, owner);
        assert_eq!(row.filename, "report.txt");
        assert_eq!(row.uri, format!("local://{path}"));
        assert_eq!(row.filemime, "text/plain");
        assert_eq!(
            row.filesize,
            FILE_BODY.len() as i64,
            "size is measured, not trusted"
        );
        assert_eq!(row.status, 1, "an imported file is Permanent");
        assert_eq!(row.created, 1_554_112_800);

        let resp = app
            .request_with_cookies(
                Request::get(format!("/files/{path}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the imported file is servable to its owner"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            body.as_ref(),
            FILE_BODY,
            "the real bytes on disk are streamed"
        );
    });
}

/// A rerun of an identical `file` record changes nothing: no new row, no
/// bumped `changed`, reported `unchanged`.
#[test]
fn a_rerun_of_an_identical_file_record_changes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let owner = create_user(app).await;
        let id = Uuid::now_v7();
        let path = scratch_path("minutes.txt");
        place_file(app, &path, FILE_BODY).await;
        forget_file(app, id);
        let line = file_line(id, owner, &path, "minutes.txt", "text/plain", 1_420_070_400);

        let first = run_lines(app, std::slice::from_ref(&line))
            .await
            .expect("first run");
        assert_eq!(first.of("file").created, 1);

        let second = run_lines(app, &[line]).await.expect("second run");
        assert_eq!(
            second.changes(),
            0,
            "a rerun of the same record changes nothing"
        );
        assert_eq!(second.of("file").unchanged, 1, "{second:?}");
    });
}

/// A changed record updates the row in place, keeping the id and the storage
/// path, but moving a file's path is refused: the bytes were never copied
/// there, so import cannot make the row follow them.
#[test]
fn a_changed_file_record_updates_but_cannot_move_the_path() {
    run_test(async {
        let app = shared_app().await;
        let owner = create_user(app).await;
        let other_owner = create_user(app).await;
        let id = Uuid::now_v7();
        let path = scratch_path("agenda.txt");
        place_file(app, &path, FILE_BODY).await;
        forget_file(app, id);

        run_lines(
            app,
            &[file_line(
                id,
                owner,
                &path,
                "agenda.txt",
                "text/plain",
                1_420_070_400,
            )],
        )
        .await
        .expect("first run");

        let renamed = file_line(
            id,
            other_owner,
            &path,
            "agenda-final.txt",
            "text/plain",
            1_420_070_400,
        );
        let report = run_lines(app, &[renamed]).await.expect("update");
        assert_eq!(report.of("file").updated, 1, "{report:?}");
        let row = stored_file(app, id).await.expect("still stored");
        assert_eq!(row.owner_id, other_owner);
        assert_eq!(row.filename, "agenda-final.txt");

        let other_path = scratch_path("elsewhere.txt");
        place_file(app, &other_path, FILE_BODY).await;
        let moved = file_line(
            id,
            other_owner,
            &other_path,
            "agenda-final.txt",
            "text/plain",
            1_420_070_400,
        );
        let error = run_lines(app, &[moved]).await.expect_err("refused");
        let failed = error
            .downcast_ref::<ImportFailed>()
            .expect("an ImportFailed");
        assert!(failed.message.contains("cannot be changed"), "{failed}");
        let row = stored_file(app, id).await.expect("unchanged");
        assert_eq!(
            row.uri,
            format!("local://{path}"),
            "the path on disk did not move"
        );
    });
}

/// An item can reference an imported file exactly as it references an
/// uploaded one: the file reference seam looks a file up by id or storage
/// URI alone, and does not care how the `file_managed` row got there.
#[test]
fn an_item_can_reference_an_imported_file() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let file_id = Uuid::now_v7();
        let path = scratch_path("logo.txt");
        place_file(app, &path, FILE_BODY).await;
        forget_file(app, file_id);

        run_lines(
            app,
            &[file_line(
                file_id,
                owner,
                &path,
                "logo.txt",
                "text/plain",
                1_420_070_400,
            )],
        )
        .await
        .expect("import the file");

        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: "B184 file reference".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(json!({ "field_logo": file_id.to_string() })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("B184 test".to_string()),
                },
                &admin(),
            )
            .await
            .expect("create an item referencing the imported file");
        let item_id = item.id;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("UPDATE item SET current_revision_id = NULL WHERE id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item_revision WHERE item_id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
        });

        let referenced: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM file_reference WHERE file_id = $1 AND item_id = $2")
                .bind(file_id)
                .bind(item.id)
                .fetch_optional(&app.db)
                .await
                .unwrap();
        assert!(
            referenced.is_some(),
            "the item's File field reference was recorded for the imported file"
        );
    });
}

/// A path that leads outside the files directory — by `..`, or by a symlink —
/// is refused by name, and nothing is written: not even a good record on an
/// earlier line in the same file.
#[test]
fn a_path_escaping_the_files_directory_is_rejected() {
    run_test(async {
        let app = shared_app().await;
        let owner = create_user(app).await;
        let good_owner = create_user(app).await;

        // `..` components.
        let escapee = Uuid::now_v7();
        let error = run_lines(
            app,
            &[file_line(
                escapee,
                owner,
                "../../etc/passwd",
                "passwd",
                "text/plain",
                1,
            )],
        )
        .await
        .expect_err("refused");
        let failed = error
            .downcast_ref::<ImportFailed>()
            .expect("an ImportFailed");
        assert!(failed.message.contains("relative path"), "{failed}");
        assert!(stored_file(app, escapee).await.is_none());

        // A symlink inside the files directory pointing outside it.
        let root = app
            .state
            .files()
            .local_dir()
            .expect("local files directory");
        let subdir = format!("b184-test/{}", Uuid::now_v7().simple());
        let link_dir = root.join(&subdir);
        tokio::fs::create_dir_all(&link_dir).await.unwrap();
        let outside =
            std::env::temp_dir().join(format!("b184-outside-{}.txt", Uuid::now_v7().simple()));
        tokio::fs::write(&outside, b"not in the files directory")
            .await
            .unwrap();
        std::os::unix::fs::symlink(&outside, link_dir.join("escape.txt")).unwrap();
        let rel = format!("{subdir}/escape.txt");
        common::defer_cleanup(move || async move {
            let _ = tokio::fs::remove_file(&outside).await;
        });
        common::defer_cleanup(move || async move {
            let _ = tokio::fs::remove_dir_all(&link_dir).await;
        });

        // A good record on an earlier line proves the failure rolls back the
        // whole transaction, not only the bad line.
        let good_id = Uuid::now_v7();
        let good_path = scratch_path("innocent.txt");
        place_file(app, &good_path, FILE_BODY).await;

        let symlinked = Uuid::now_v7();
        let error = run_lines(
            app,
            &[
                file_line(
                    good_id,
                    good_owner,
                    &good_path,
                    "innocent.txt",
                    "text/plain",
                    1,
                ),
                file_line(symlinked, owner, &rel, "escape.txt", "text/plain", 1),
            ],
        )
        .await
        .expect_err("refused");
        let failed = error
            .downcast_ref::<ImportFailed>()
            .expect("an ImportFailed");
        assert_eq!(failed.line, 2, "{failed}");
        assert!(
            failed.message.contains("outside the files directory"),
            "{failed}"
        );
        assert!(stored_file(app, symlinked).await.is_none());
        assert!(
            stored_file(app, good_id).await.is_none(),
            "the whole import rolled back, including the earlier good line"
        );
    });
}

/// A `file` record naming a path with nothing on disk fails that line with a
/// clear message, and rolls back the whole import — an earlier good record in
/// the same file is not kept either.
#[test]
fn a_missing_file_is_rejected_and_rolls_back_the_import() {
    run_test(async {
        let app = shared_app().await;
        let owner = create_user(app).await;
        let good_id = Uuid::now_v7();
        let good_path = scratch_path("present.txt");
        place_file(app, &good_path, FILE_BODY).await;
        forget_file(app, good_id);

        let missing_id = Uuid::now_v7();
        let missing_path = scratch_path("never-written.txt");

        let error = run_lines(
            app,
            &[
                file_line(good_id, owner, &good_path, "present.txt", "text/plain", 1),
                file_line(
                    missing_id,
                    owner,
                    &missing_path,
                    "ghost.txt",
                    "text/plain",
                    1,
                ),
            ],
        )
        .await
        .expect_err("refused");
        let failed = error
            .downcast_ref::<ImportFailed>()
            .expect("an ImportFailed");
        assert_eq!(failed.line, 2, "{failed}");
        assert!(failed.message.contains("not found"), "{failed}");
        assert!(stored_file(app, missing_id).await.is_none());
        assert!(
            stored_file(app, good_id).await.is_none(),
            "the whole import rolled back, including the earlier good line"
        );
    });
}
