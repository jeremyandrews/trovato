#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The content translation admin pages, and the templates the kernel renders.
//!
//! `routes/admin_translation.rs` registers `/admin/content/{id}/translate` and
//! `/admin/content/{id}/translate/{lang}`, and `trovato_content_translation`
//! puts both in the admin menu. Each route renders a template that was never
//! written (`admin/content-translate-list.html`, `admin/content-translate-edit.html`),
//! so every request answered 500 with a Tera "template not found" error.
//!
//! Two layers of test. The pages themselves must render what the routes load.
//! And, because nothing checks at build time that a template name in Rust
//! source names a real file, a scan requires every template the kernel passes
//! to `render_admin_template` or `tera().render` to exist under `templates/`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::path::Path;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, project_root, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

async fn get(app: &TestApp, cookies: &str, path: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", "10.73.0.1")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn create_item(app: &TestApp, title: &str) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("admin translation test".to_string()),
            },
            &UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .expect("create item")
        .id
}

/// Serializes the plugin migration below across tests, binaries and shards.
const TRANSLATION_MIGRATION_LOCK: i64 = 0x_7A11_0000_0001;

/// Apply `trovato_content_translation`'s own migrations, which create
/// `item_translation`.
///
/// The shared app discovers no plugins (its plugin directory is relative to the
/// test's working directory), so the table exists only if some earlier test in
/// the same database booted with the real plugins directory. Depending on that
/// is depending on shard order, so the fixture runs the migration itself. It is
/// idempotent: applied files are recorded in `plugin_migration` and skipped.
///
/// Driven on its own thread against the shared runtime, the way `shared_app`
/// builds the app: the migration runner's future does not meet `run_test`'s
/// `Send` bound.
fn ensure_translation_table(app: &TestApp) {
    let dir = common::project_root().join("plugins/trovato_content_translation");
    let info = trovato_kernel::plugin::PluginInfo::parse(
        &dir.join("trovato_content_translation.info.toml"),
    )
    .expect("parse the translation plugin manifest");
    let db = app.db.clone();
    let handle = common::shared_runtime_handle();

    std::thread::spawn(move || {
        handle.block_on(async move {
            // Held for the length of the transaction; the migration runs on its
            // own connection meanwhile, and any concurrent caller waits here.
            let mut guard = db.begin().await.expect("begin migration lock");
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(TRANSLATION_MIGRATION_LOCK)
                .execute(&mut *guard)
                .await
                .expect("take migration lock");
            let result = trovato_kernel::plugin::migration::run_plugin_migrations(
                &db,
                "trovato_content_translation",
                &info,
                &dir,
            )
            .await;
            guard.commit().await.expect("release migration lock");
            result.expect("run the translation plugin migrations");
        });
    })
    .join()
    .expect("translation migration thread panicked");
}

/// An admin session with the translation plugin enabled and an item that has an
/// Italian translation and no Hebrew one.
async fn fixture(app: &TestApp) -> (String, Uuid, String) {
    ensure_translation_table(app);
    app.ensure_plugin_enabled("trovato_content_translation")
        .await;

    let tag = Uuid::now_v7().simple().to_string();
    let original = format!("Original {tag}");
    let item = create_item(app, &original).await;
    sqlx::query(
        "INSERT INTO item_translation (item_id, language, title, fields) \
         VALUES ($1, 'it', $2, '{\"field_note\": \"tradotto\"}'::jsonb)",
    )
    .bind(item)
    .bind(format!("Tradotto {tag}"))
    .execute(&app.db)
    .await
    .expect("record translation");

    let name = format!("transadmin_{tag}");
    let cookies = app
        .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    (cookies, item, tag)
}

#[test]
fn the_translation_list_renders_each_language_and_its_state() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, tag) = fixture(app).await;

        let (status, html) = get(app, &cookies, &format!("/admin/content/{item}/translate")).await;
        assert_eq!(status, StatusCode::OK, "translation list: {html}");

        assert!(html.contains(&format!("Original {tag}")));
        assert!(html.contains(&format!("Tradotto {tag}")));
        assert!(
            html.contains(&format!("/admin/content/{item}/translate/it")),
            "the Italian translation links to its page"
        );
        assert!(
            html.contains("Not translated"),
            "a language without a translation is shown as such"
        );
        assert!(
            !html.contains("<form"),
            "the list is read-only; there is no write path for translations yet"
        );
    });
}

#[test]
fn the_translation_page_renders_an_existing_and_a_missing_translation() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, tag) = fixture(app).await;

        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/content/{item}/translate/it"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "existing translation: {html}");
        assert!(html.contains(&format!("Original {tag}")));
        assert!(html.contains(&format!("Tradotto {tag}")));
        assert!(html.contains("tradotto"), "the translated fields are shown");
        assert!(!html.contains("<form"), "the page is read-only");

        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/content/{item}/translate/he"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "missing translation: {html}");
        assert!(html.contains("has no <code>he</code> translation"));
    });
}

/// The first string literal after each occurrence of `call` in `source`.
fn template_names_after(source: &str, call: &str) -> Vec<String> {
    source
        .match_indices(call)
        .filter_map(|(at, _)| {
            let rest = &source[at + call.len()..];
            // Only the argument list of this call: stop at its closing paren so
            // a variable template name does not borrow a later literal.
            let args = &rest[..rest.find(')').unwrap_or(rest.len())];
            let open = args.find('"')?;
            let name = args[open + 1..].split('"').next()?;
            name.ends_with(".html").then(|| name.to_string())
        })
        .collect()
}

fn rust_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every template name the kernel hands to the renderer names a real file.
#[test]
fn every_template_the_kernel_renders_exists() {
    let root = project_root();
    let mut files = Vec::new();
    rust_sources(&root.join("crates/kernel/src"), &mut files);

    let mut checked = 0;
    let mut missing = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file).expect("read source");
        // Unit-test modules render synthetic templates they write themselves.
        let source = source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or_default()
            .to_string();
        let names = template_names_after(&source, "render_admin_template(")
            .into_iter()
            .chain(template_names_after(&source, "tera().render("));
        for name in names {
            checked += 1;
            if !root.join("templates").join(&name).is_file() {
                missing.push(format!("{} renders {name}", file.display()));
            }
        }
    }

    assert!(
        checked >= 50,
        "found only {checked} template renders, so the scan is not seeing the source"
    );
    assert!(
        missing.is_empty(),
        "templates rendered by the kernel that do not exist:\n{}",
        missing.join("\n")
    );
}
