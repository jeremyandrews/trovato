#![allow(clippy::unwrap_used, clippy::expect_used)]
//! SMTP configuration has one source, the environment (**B26**).
//!
//! `/admin/config/site` used to carry an SMTP fieldset that wrote six `smtp_*`
//! keys into `site_config`, the password in the clear. Nothing read those keys:
//! the one `EmailService` is built from `SMTP_*` in the environment. So the form
//! was write only, and every database where an administrator typed a password
//! held it in clear text, including every config export.
//!
//! These tests pin the replacement: no SMTP input on the page and nothing stored
//! from a POST that carries one, a read only summary of the configuration the
//! kernel actually uses (never the username or password values), a migration
//! that deletes the old keys, and an export with no `smtp_*` key in it.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use trovato_kernel::models::{RegistrationMode, SiteConfig};

/// The SMTP host and the other values the fixture app is configured with. None
/// of them reaches a real server: the page only reads them.
const SMTP_HOST: &str = "smtp.b26.example.test";
const SMTP_PORT: u16 = 2526;
const SMTP_FROM: &str = "mailer-from@b26.example.test";
const SMTP_USERNAME: &str = "b26-configured-username";
const SMTP_PASSWORD: &str = "b26-Configured-Password-Value";

/// What an administrator types into the old form. Never to be stored.
const POSTED_PASSWORD: &str = "b26-Posted-Password-Never-Stored";

/// The six keys the old form wrote.
const SMTP_KEYS: &[&str] = &[
    "smtp_host",
    "smtp_port",
    "smtp_username",
    "smtp_password",
    "smtp_encryption",
    "smtp_from",
];

/// Serializes the tests in this binary: they all read or write `smtp_*` rows
/// in one shared `site_config` table.
static SMTP_KEYS_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// An app whose environment says SMTP is configured, with a username and a
/// password set.
async fn app() -> TestApp {
    TestApp::with_config(|config| {
        config.smtp_host = Some(SMTP_HOST.to_string());
        config.smtp_port = SMTP_PORT;
        config.smtp_username = Some(SMTP_USERNAME.to_string());
        config.smtp_password = Some(SMTP_PASSWORD.to_string());
        config.smtp_encryption = "tls".to_string();
        config.smtp_from_email = SMTP_FROM.to_string();
    })
    .await
}

async fn text_body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// GET the site settings page as an administrator.
async fn site_page(app: &TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/admin/config/site")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    text_body(response).await
}

fn csrf_token(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html.find(needle).expect("token input") + needle.len();
    let end = start + html[start..].find('"').expect("token end");
    html[start..end].to_string()
}

async fn smtp_rows(app: &TestApp) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT key FROM site_config WHERE key LIKE 'smtp\\_%' ORDER BY key",
    )
    .fetch_all(&app.db)
    .await
    .unwrap()
}

async fn delete_smtp_rows(app: &TestApp) {
    sqlx::query("DELETE FROM site_config WHERE key LIKE 'smtp\\_%'")
        .execute(&app.db)
        .await
        .unwrap();
}

/// The settings a site form save rewrites in a form other than the one it
/// read: the registration mode is stored as `user_registration` even when it
/// came from the legacy `allow_user_registration` boolean or from no setting,
/// and the boolean is deleted.
const REGISTRATION_KEYS: &[&str] = &["user_registration", "allow_user_registration"];

/// POST the site settings form as an administrator would, carrying every one of
/// the old SMTP fields, and every other field at the value it already has, so
/// the shared site settings are left as they were.
///
/// The registration settings are put back exactly as they were found
/// afterwards: a stored mode overrides the boolean other test binaries set, so
/// leaving one behind closed `/user/register` for any of them sharing this
/// database.
async fn post_with_smtp_fields(app: &TestApp, cookies: &str) -> StatusCode {
    let mut registration = Vec::new();
    for key in REGISTRATION_KEYS {
        registration.push((*key, SiteConfig::get(&app.db, key).await.unwrap()));
    }
    let status = post_site_form(app, cookies).await;
    for (key, value) in registration {
        match value {
            Some(value) => SiteConfig::set(&app.db, key, value).await.unwrap(),
            None => {
                SiteConfig::delete(&app.db, key).await.unwrap();
            }
        }
    }
    status
}

/// The POST [`post_with_smtp_fields`] makes.
async fn post_site_form(app: &TestApp, cookies: &str) -> StatusCode {
    let html = site_page(app, cookies).await;
    let token = csrf_token(&html);

    let site_name = SiteConfig::site_name(&app.db).await.unwrap();
    let site_slogan = SiteConfig::site_slogan(&app.db).await.unwrap_or_default();
    let mut site_mail = SiteConfig::site_mail(&app.db).await.unwrap_or_default();
    if site_mail.is_empty() {
        site_mail = "site@b26.example.test".to_string();
    }
    let front_page = SiteConfig::front_page(&app.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let items_per_page = SiteConfig::get(&app.db, "items_per_page")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "10".to_string());
    let registration_mode = RegistrationMode::load(&app.db).await.as_str().to_string();
    let notify = SiteConfig::get(&app.db, "notify_admin_on_register")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let update_check = trovato_kernel::update_status::setting_allows_check(&app.db).await;

    let mut fields: Vec<(&str, String)> = vec![
        ("_token", token),
        ("site_name", site_name),
        ("site_slogan", site_slogan),
        ("site_mail", site_mail),
        ("front_page", front_page),
        ("items_per_page", items_per_page),
        ("registration_mode", registration_mode),
        ("smtp_host", "smtp.posted.example.test".to_string()),
        ("smtp_port", "2525".to_string()),
        ("smtp_username", "posted-username".to_string()),
        ("smtp_password", POSTED_PASSWORD.to_string()),
        ("smtp_encryption", "starttls".to_string()),
        ("smtp_from", "posted-from@b26.example.test".to_string()),
    ];
    if notify {
        fields.push(("notify_admin_on_register", "1".to_string()));
    }
    if update_check {
        fields.push(("update_check", "1".to_string()));
    }
    let form = fields
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let response = app
        .request_with_cookies(
            Request::post("/admin/config/site")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
            cookies,
        )
        .await;
    response.status()
}

async fn admin_cookies(app: &TestApp) -> String {
    let name = common::username("b26admin");
    app.create_and_login_admin(
        &name,
        "b26-admin-Password-1",
        &format!("{name}@example.test"),
    )
    .await
}

/// The site settings page offers no SMTP input, and renders no password.
#[tokio::test]
async fn the_site_page_has_no_smtp_inputs_and_no_password() {
    let app = app().await;
    let _guard = SMTP_KEYS_LOCK.lock().await;
    let cookies = admin_cookies(&app).await;

    let html = site_page(&app, &cookies).await;

    for key in SMTP_KEYS {
        assert!(
            !html.contains(&format!(r#"name="{key}""#)),
            "the page must not offer an input named {key}"
        );
    }
    assert!(
        !html.contains(r#"type="password""#),
        "the site settings page has no password input"
    );
    assert!(
        !html.contains(SMTP_PASSWORD),
        "the configured SMTP password must never be rendered"
    );
    assert!(
        !html.contains(SMTP_USERNAME),
        "the configured SMTP username must never be rendered"
    );
}

/// The page shows the configuration the kernel actually uses: host, port,
/// encryption and from address, and only whether a username and a password are
/// set.
#[tokio::test]
async fn the_site_page_shows_the_effective_smtp_configuration() {
    let app = app().await;
    let _guard = SMTP_KEYS_LOCK.lock().await;
    let cookies = admin_cookies(&app).await;

    let html = site_page(&app, &cookies).await;

    for (id, value) in [
        ("smtp-host", SMTP_HOST.to_string()),
        ("smtp-port", SMTP_PORT.to_string()),
        ("smtp-encryption", "tls".to_string()),
        ("smtp-from", SMTP_FROM.to_string()),
        ("smtp-username", "set".to_string()),
        ("smtp-password", "set".to_string()),
    ] {
        assert!(
            html.contains(&format!(r#"id="{id}">{value}<"#)),
            "expected {id} to read {value:?} in the effective SMTP summary"
        );
    }
}

/// A POST that still carries the old SMTP fields stores none of them.
#[tokio::test]
async fn a_post_carrying_smtp_fields_stores_nothing() {
    let app = app().await;
    let _guard = SMTP_KEYS_LOCK.lock().await;
    delete_smtp_rows(&app).await;
    let cookies = admin_cookies(&app).await;

    let status = post_with_smtp_fields(&app, &cookies).await;
    let rows = smtp_rows(&app).await;
    let password_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM site_config WHERE value::text LIKE $1")
            .bind(format!("%{POSTED_PASSWORD}%"))
            .fetch_one(&app.db)
            .await
            .unwrap();
    delete_smtp_rows(&app).await;

    assert!(
        status.is_redirection(),
        "the settings still save: got {status}"
    );
    assert!(rows.is_empty(), "no smtp_* key may be stored: {rows:?}");
    assert_eq!(password_rows, 0, "the posted password must not be stored");
}

/// A config export after a save that carried SMTP fields contains no `smtp_*`
/// key, by file name or by content.
#[tokio::test]
async fn the_config_export_contains_no_smtp_key() {
    let app = app().await;
    let _guard = SMTP_KEYS_LOCK.lock().await;
    let cookies = admin_cookies(&app).await;
    post_with_smtp_fields(&app, &cookies).await;

    let dir = std::env::temp_dir().join(format!(
        "trovato_b26_export_{}",
        uuid::Uuid::now_v7().simple()
    ));
    let result = trovato_kernel::config_storage::yaml::export_config(
        app.config_storage().as_ref(),
        &app.db,
        &dir,
        false,
    )
    .await;
    delete_smtp_rows(&app).await;

    let mut offending = Vec::new();
    if result.is_ok() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            if name.starts_with("variable.smtp_") || contents.contains(POSTED_PASSWORD) {
                offending.push(name);
            }
        }
    }
    std::fs::remove_dir_all(&dir).ok();

    result.expect("config export");
    assert!(
        offending.is_empty(),
        "the export must hold no smtp_* key: {offending:?}"
    );
}

/// After the migration runs, `site_config` holds no `smtp_*` key.
///
/// The harness has already applied every migration, so this puts the six keys
/// back the way the old form wrote them and applies the migration's own SQL,
/// read from the file that ships, to them.
#[tokio::test]
async fn the_migration_deletes_every_smtp_key() {
    let app = app().await;
    let _guard = SMTP_KEYS_LOCK.lock().await;

    let dir = common::project_root().join("crates/kernel/migrations");
    let migration = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .find(|p| {
            let sql = std::fs::read_to_string(p).unwrap_or_default();
            sql.contains("DELETE FROM site_config") && sql.contains("smtp_password")
        })
        .expect("a migration deleting the smtp_* keys from site_config");
    let sql = std::fs::read_to_string(&migration).unwrap();

    for key in SMTP_KEYS {
        SiteConfig::set(&app.db, key, serde_json::json!("old-form-value"))
            .await
            .unwrap();
    }
    assert_eq!(smtp_rows(&app).await.len(), SMTP_KEYS.len());

    // The SQL is the migration file this repository ships, not input.
    let applied = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&app.db)
        .await;
    let rows = smtp_rows(&app).await;
    delete_smtp_rows(&app).await;

    applied.expect("apply the migration SQL");
    assert!(
        rows.is_empty(),
        "{} must delete every smtp_* key: {rows:?}",
        migration.display()
    );
}
