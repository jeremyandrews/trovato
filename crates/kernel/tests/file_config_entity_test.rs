#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The `file` config entity: a config set can ship a file.
//!
//! The only way to create a `file_managed` row was `POST /file/upload` with a
//! session and a CSRF header, so a config set could reference a file by id but
//! never carry one (Ritrovo `G-NO-FILE-CONFIG-ENTITY`). A set now holds
//! `file.<uuid>.yml` (`filename`, `filemime`, optional `alt_text`, and `path`,
//! relative to the set's directory) beside the bytes themselves, imported
//! through the same validation upload uses.
//!
//! These run on the shared app, through its own config storage, so they also
//! pin that the running site's storage is wired to its file storage. Every id
//! is fresh and the set uses the shared `conference` type, so nothing here
//! grows a table the suite guards.
//!
//! Requires Postgres + Redis.

mod common;

use std::path::{Path, PathBuf};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::config_storage::yaml::{ConfigImportFailed, export_config, import_config};
use trovato_kernel::file::{FileStatus, MAX_FILE_SIZE};
use uuid::Uuid;

/// A minimal PDF: what `infer` recognizes as `application/pdf`.
const PDF: &[u8] = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n";

struct SetDir(PathBuf);

impl SetDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "trovato_b72_fileset_{label}_{}",
            Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(path.join("files")).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SetDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file_yaml(id: Uuid, filename: &str, filemime: &str, path: &str) -> String {
    format!(
        "id: {id}\nfilename: {filename}\nfilemime: {filemime}\nalt_text: Conference programme\npath: {path}\n"
    )
}

fn item_yaml(id: Uuid, file_id: Uuid) -> String {
    format!(
        "id: {id}\ntype: conference\ntitle: Imported Conference {}\nstatus: 1\n\
         fields:\n  field_start_date: '2026-09-01'\n  field_end_date: '2026-09-03'\n\
         \x20 field_logo: {file_id}\n",
        id.simple()
    )
}

async fn import(app: &TestApp, dir: &Path) -> anyhow::Result<()> {
    import_config(app.config_storage().as_ref(), &app.db, dir, false)
        .await
        .map(|_| ())
}

async fn anon_get(app: &TestApp, url: &str) -> axum::response::Response {
    app.request(Request::get(url).body(Body::empty()).unwrap())
        .await
}

/// A set holding `file.<uuid>.yml`, its bytes and an item referencing it
/// imports a permanent file with that id, and the published item's file is
/// served to an anonymous visitor.
#[test]
fn a_config_set_ships_a_file_an_item_serves() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let file_id = Uuid::now_v7();
        let item_id = Uuid::now_v7();
        let set = SetDir::new("ship");
        set.write(
            &format!("file.{file_id}.yml"),
            file_yaml(
                file_id,
                "programme.pdf",
                "application/pdf",
                "files/programme.pdf",
            )
            .as_bytes(),
        );
        set.write("files/programme.pdf", PDF);
        set.write(
            &format!("item.{item_id}.yml"),
            item_yaml(item_id, file_id).as_bytes(),
        );

        import(app, set.path()).await.expect("the set imports");

        let file = app
            .state
            .files()
            .get(file_id)
            .await
            .unwrap()
            .expect("file row");
        assert_eq!(file.status, FileStatus::Permanent);
        assert_eq!(file.filename, "programme.pdf");
        assert_eq!(file.filemime, "application/pdf");
        assert_eq!(file.alt_text.as_deref(), Some("Conference programme"));
        assert_eq!(
            app.state.files().storage().read(&file.uri).await.unwrap(),
            PDF
        );

        let url = app.state.files().storage().public_url(&file.uri);
        let resp = anon_get(app, &url).await;
        assert_eq!(resp.status(), StatusCode::OK, "anonymous GET {url}");
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/pdf"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), PDF);
    });
}

/// Re-importing the same set changes nothing; changed bytes replace the stored
/// ones under the same id and URI.
#[test]
fn reimport_is_idempotent_and_replaces_only_changed_bytes() {
    run_test(async {
        let app = shared_app().await;
        let file_id = Uuid::now_v7();
        let set = SetDir::new("again");
        set.write(
            &format!("file.{file_id}.yml"),
            file_yaml(file_id, "notes.txt", "text/plain", "files/notes.txt").as_bytes(),
        );
        set.write("files/notes.txt", b"first version");

        import(app, set.path()).await.expect("first import");
        let first = app.state.files().get(file_id).await.unwrap().unwrap();

        // Mark the row so an untouched re-import is visible.
        sqlx::query("UPDATE file_managed SET changed = 1 WHERE id = $1")
            .bind(file_id)
            .execute(&app.db)
            .await
            .unwrap();
        import(app, set.path()).await.expect("identical re-import");
        let same = app.state.files().get(file_id).await.unwrap().unwrap();
        assert_eq!(same.uri, first.uri);
        assert_eq!(same.changed, 1, "identical bytes are not rewritten");
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM file_managed WHERE id = $1")
            .bind(file_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(rows, 1);

        set.write("files/notes.txt", b"second, longer version");
        import(app, set.path()).await.expect("changed re-import");
        let changed = app.state.files().get(file_id).await.unwrap().unwrap();
        assert_eq!(changed.uri, first.uri, "the URI is stable across imports");
        assert_eq!(changed.filesize, b"second, longer version".len() as i64);
        assert_eq!(
            app.state
                .files()
                .storage()
                .read(&changed.uri)
                .await
                .unwrap(),
            b"second, longer version"
        );
    });
}

/// Export writes the file entity and its bytes, and the export imports again.
#[test]
fn export_writes_the_entity_and_its_bytes() {
    run_test(async {
        let app = shared_app().await;
        let file_id = Uuid::now_v7();
        let name = format!("handout-{}.pdf", file_id.simple());
        let set = SetDir::new("src");
        set.write(
            &format!("file.{file_id}.yml"),
            file_yaml(file_id, &name, "application/pdf", &format!("files/{name}")).as_bytes(),
        );
        set.write(&format!("files/{name}"), PDF);
        import(app, set.path()).await.expect("import");

        let out = SetDir::new("export");
        export_config(app.config_storage().as_ref(), &app.db, out.path(), false)
            .await
            .expect("export");

        let yaml = std::fs::read_to_string(out.path().join(format!("file.{file_id}.yml")))
            .expect("file entity exported");
        let path_line = yaml
            .lines()
            .find_map(|l| l.strip_prefix("path: "))
            .expect("exported entity names its bytes")
            .trim()
            .to_string();
        assert!(
            path_line.starts_with("files/"),
            "bytes live under files/: {path_line}"
        );
        assert_eq!(std::fs::read(out.path().join(&path_line)).unwrap(), PDF);
        assert!(yaml.contains("filemime: application/pdf"));
        assert!(!yaml.contains("JVBER"), "bytes are never base64 in YAML");

        // The exported pair imports again (into the same row: idempotent).
        let single = SetDir::new("reimport");
        single.write(&format!("file.{file_id}.yml"), yaml.as_bytes());
        single.write(
            &path_line,
            &std::fs::read(out.path().join(&path_line)).unwrap(),
        );
        import(app, single.path())
            .await
            .expect("the exported pair re-imports");
    });
}

/// A set whose file entity is unsafe or invalid fails validation, names the
/// file, and writes nothing, not even the valid item beside it.
async fn assert_refused(app: &TestApp, label: &str, file_id: Uuid, set: &SetDir) {
    let item_id = Uuid::now_v7();
    set.write(
        &format!("item.{item_id}.yml"),
        item_yaml(item_id, Uuid::now_v7()).as_bytes(),
    );

    let err = import(app, set.path())
        .await
        .expect_err(&format!("{label}: the import must fail"));
    let failed = err
        .downcast_ref::<ConfigImportFailed>()
        .unwrap_or_else(|| panic!("{label}: a validation failure, got {err:#}"));
    assert_eq!(failed.imported_total(), 0, "{label}: nothing is written");
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.filename == format!("file.{file_id}.yml")),
        "{label}: the failure names the file entity: {failed}"
    );
    assert!(
        app.state.files().get(file_id).await.unwrap().is_none(),
        "{label}: no file row"
    );
    let item: i64 = sqlx::query_scalar("SELECT count(*) FROM item WHERE id = $1")
        .bind(item_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(item, 0, "{label}: the valid item beside it is not imported");
}

#[test]
fn a_bytes_path_escaping_the_set_is_refused() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        // `..` out of the set.
        let outside = SetDir::new("outside");
        outside.write("secret.pdf", PDF);
        let set = SetDir::new("dotdot");
        let id = Uuid::now_v7();
        let rel = format!(
            "../{}/secret.pdf",
            outside.path().file_name().unwrap().to_str().unwrap()
        );
        set.write(
            &format!("file.{id}.yml"),
            file_yaml(id, "secret.pdf", "application/pdf", &rel).as_bytes(),
        );
        assert_refused(app, "dot-dot path", id, &set).await;

        // An absolute path.
        let set = SetDir::new("absolute");
        let id = Uuid::now_v7();
        let abs = outside.path().join("secret.pdf");
        set.write(
            &format!("file.{id}.yml"),
            file_yaml(id, "secret.pdf", "application/pdf", abs.to_str().unwrap()).as_bytes(),
        );
        assert_refused(app, "absolute path", id, &set).await;

        // A symlink inside the set pointing outside it.
        #[cfg(unix)]
        {
            let set = SetDir::new("symlink");
            let id = Uuid::now_v7();
            std::os::unix::fs::symlink(
                outside.path().join("secret.pdf"),
                set.path().join("files/linked.pdf"),
            )
            .unwrap();
            set.write(
                &format!("file.{id}.yml"),
                file_yaml(id, "linked.pdf", "application/pdf", "files/linked.pdf").as_bytes(),
            );
            assert_refused(app, "symlink out of the set", id, &set).await;
        }
    });
}

#[test]
fn an_oversized_disallowed_or_mislabelled_file_is_refused() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        // Bytes over MAX_FILE_SIZE.
        let set = SetDir::new("big");
        let id = Uuid::now_v7();
        let mut big = PDF.to_vec();
        big.resize(MAX_FILE_SIZE + 1, b' ');
        set.write("files/big.pdf", &big);
        set.write(
            &format!("file.{id}.yml"),
            file_yaml(id, "big.pdf", "application/pdf", "files/big.pdf").as_bytes(),
        );
        assert_refused(app, "over the size limit", id, &set).await;

        // A type the upload allowlist does not hold.
        let set = SetDir::new("svg");
        let id = Uuid::now_v7();
        set.write(
            "files/logo.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        );
        set.write(
            &format!("file.{id}.yml"),
            file_yaml(id, "logo.svg", "image/svg+xml", "files/logo.svg").as_bytes(),
        );
        assert_refused(app, "a disallowed type", id, &set).await;

        // Magic bytes that disagree with the declared type.
        let set = SetDir::new("magic");
        let id = Uuid::now_v7();
        set.write("files/photo.png", PDF);
        set.write(
            &format!("file.{id}.yml"),
            file_yaml(id, "photo.png", "image/png", "files/photo.png").as_bytes(),
        );
        assert_refused(app, "content that is not the declared type", id, &set).await;
    });
}
