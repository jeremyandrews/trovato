#![allow(clippy::unwrap_used, clippy::expect_used)]
//! An account may have no email address (B309).
//!
//! An administrator, `trovato user create` and `trovato import` may all leave
//! `mail` empty. Such an account signs in with its name and password; an
//! empty address never matches a lookup (login, password reset, duplicate
//! checks), and nothing that would mail the account tries to. Self
//! registration is unchanged and still requires an address.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use serde_json::json;
use trovato_kernel::import::import;
use trovato_kernel::models::{RegistrationMode, SiteConfig, User};
use uuid::Uuid;

const BROWSER: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const PASSWORD: &str = "test-password-123";

struct Answer {
    status: StatusCode,
    cookies: String,
    body: String,
}

async fn answer(response: axum::response::Response) -> Answer {
    let status = response.status();
    let cookies = common::extract_cookies(&response);
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Answer {
        status,
        cookies,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> Answer {
    let request = Request::get(path)
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", test_ip_for("b309-no-mail"))
        .body(Body::empty())
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

async fn post_form(app: &TestApp, path: &str, cookies: &str, body: String, ip: &str) -> Answer {
    let request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", ip)
        .body(Body::from(body))
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

/// The CSRF token hidden field on a page rendered with `form::hidden_fields`.
fn hidden(page: &str, name: &str) -> String {
    let marker = format!(r#"name="{name}" value=""#);
    let start = page
        .find(&marker)
        .unwrap_or_else(|| panic!("no {name}: {page}"))
        + marker.len();
    let end = page[start..].find('"').unwrap();
    page[start..start + end].to_string()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn delete_user_named_on_exit(app: &TestApp, name: &str) {
    let db = app.db.clone();
    let name = name.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&name)
            .execute(&db)
            .await;
    });
}

/// Set the registration mode for the rest of this test, restoring whatever it
/// was on exit (as `account_functions_test.rs` does).
async fn set_registration(app: &TestApp, mode: RegistrationMode) {
    let mut saved = Vec::new();
    for key in ["user_registration", "allow_user_registration"] {
        saved.push((key, SiteConfig::get(&app.db, key).await.ok().flatten()));
    }
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        for (key, value) in saved {
            match value {
                Some(value) => {
                    let _ = SiteConfig::set(&db, key, value).await;
                }
                None => {
                    let _ = SiteConfig::delete(&db, key).await;
                }
            }
        }
    });
    mode.save(&app.db).await.unwrap();
}

async fn login_json(app: &TestApp, identifier: &str, password: &str) -> StatusCode {
    let ip = test_ip_for(identifier);
    app.state.lockout().clear_all(identifier).await.ok();
    app.state.rate_limiter().reset("login", &ip).await.ok();
    app.request(
        Request::post("/user/login/json")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({"username": identifier, "password": password}).to_string(),
            ))
            .unwrap(),
    )
    .await
    .status()
}

async fn admin(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let cookies = app
        .create_and_login_admin(&name, PASSWORD, &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    delete_user_named_on_exit(app, &name);
    (id, cookies)
}

/// Create an account with no mail through the admin "Add user" form.
/// Returns `(user_id, cookies)` of the admin who made it, and leaves the new
/// account named for cleanup.
async fn create_via_admin_form(app: &TestApp, admin_cookies: &str, name: &str) -> StatusCode {
    let form = get(app, "/admin/people/add", admin_cookies).await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    let token = hidden(&form.body, "_token");
    delete_user_named_on_exit(app, name);
    let body = format!(
        "_token={token}&_form_build_id=x&name={}&mail=&password={}&status=1",
        enc(name),
        enc(PASSWORD)
    );
    let a = post_form(
        app,
        "/admin/people/add",
        admin_cookies,
        body,
        &test_ip_for(&format!("b309-create-{name}")),
    )
    .await;
    assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
    a.status
}

/// **Red on `main`: an administrator can create an account with no address.**
///
/// It is written with `mail = ''`, and signs in with its name alone.
#[test]
fn admin_form_creates_an_account_with_no_mail_that_logs_in_by_name() {
    run_test(async {
        let app = shared_app().await;
        let (_, admin_cookies) = admin(app, "b309-admin-a").await;
        let name = common::username("b309-no-mail");

        create_via_admin_form(app, &admin_cookies, &name).await;

        let user = app
            .state
            .users()
            .find_by_name(&name)
            .await
            .unwrap()
            .expect("the account exists");
        assert_eq!(user.mail, "", "stored with no address");
        assert_eq!(user.mail_address(), None);

        assert_eq!(
            login_json(app, &name, PASSWORD).await,
            StatusCode::OK,
            "an account with no address still signs in by name"
        );
    });
}

/// **Red on `main`: two accounts with no address coexist.**
///
/// The "already in use" check, and the database's own unique index, both
/// exempt the empty address, so creating a second one is not refused as a
/// duplicate of the first.
#[test]
fn two_accounts_with_no_mail_coexist() {
    run_test(async {
        let app = shared_app().await;
        let (_, admin_cookies) = admin(app, "b309-admin-b").await;
        let first = common::username("b309-twin-a");
        let second = common::username("b309-twin-b");

        create_via_admin_form(app, &admin_cookies, &first).await;
        create_via_admin_form(app, &admin_cookies, &second).await;

        let a = app
            .state
            .users()
            .find_by_name(&first)
            .await
            .unwrap()
            .unwrap();
        let b = app
            .state
            .users()
            .find_by_name(&second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(a.mail, "");
        assert_eq!(b.mail, "");
        assert_ne!(a.id, b.id);

        assert_eq!(login_json(app, &first, PASSWORD).await, StatusCode::OK);
        assert_eq!(login_json(app, &second, PASSWORD).await, StatusCode::OK);
    });
}

/// **Red on `main`: `find_by_mail` never returns an account for an empty or
/// whitespace argument, even when one exists with that stored value.**
///
/// This is what keeps a login or a password reset request that names no
/// address from ever landing on one of these accounts: before the fix,
/// `find_by_mail(pool, "")` matched the first such row the index returned,
/// an arbitrary account with no address rather than "not found".
#[test]
fn find_by_mail_never_matches_an_empty_or_blank_argument() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b309-blank-lookup");
        app.create_test_user(&name, PASSWORD, "").await;
        delete_user_named_on_exit(app, &name);

        assert!(User::find_by_mail(&app.db, "").await.unwrap().is_none());
        assert!(User::find_by_mail(&app.db, "   ").await.unwrap().is_none());
    });
}

/// **Red on `main`: a password reset requested with no address sends no mail
/// and answers exactly as it would for an address nobody has.**
#[test]
fn password_reset_for_no_address_sends_nothing_and_answers_like_an_unknown_address() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b309-reset-no-mail");
        app.create_test_user(&name, PASSWORD, "").await;
        delete_user_named_on_exit(app, &name);
        let started_at = chrono::Utc::now();

        let ip_blank = test_ip_for("b309-reset-blank");
        let ip_unknown = test_ip_for("b309-reset-unknown");
        let limiter = app.state.rate_limiter();
        limiter.reset("recovery", &ip_blank).await.ok();
        limiter
            .reset("recovery", &format!("password-reset:{ip_blank}"))
            .await
            .ok();
        limiter.reset("reset_mail", "").await.ok();
        limiter.reset("recovery", &ip_unknown).await.ok();
        limiter
            .reset("recovery", &format!("password-reset:{ip_unknown}"))
            .await
            .ok();
        let unknown = format!("nobody-b309-{}@example.com", Uuid::now_v7().simple());
        limiter.reset("reset_mail", &unknown).await.ok();

        let blank_answer = app
            .request(
                Request::post("/user/password-reset")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip_blank)
                    .body(Body::from(json!({ "email": "" }).to_string()))
                    .unwrap(),
            )
            .await;
        let blank_status = blank_answer.status();
        let blank_body = to_bytes(blank_answer.into_body(), 1_000_000).await.unwrap();

        let unknown_answer = app
            .request(
                Request::post("/user/password-reset")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip_unknown)
                    .body(Body::from(json!({ "email": unknown }).to_string()))
                    .unwrap(),
            )
            .await;
        let unknown_status = unknown_answer.status();
        let unknown_body = to_bytes(unknown_answer.into_body(), 1_000_000)
            .await
            .unwrap();

        assert_eq!(blank_status, StatusCode::OK);
        assert_eq!(
            blank_status, unknown_status,
            "an empty address answers exactly as an unknown one does"
        );
        assert_eq!(
            blank_body, unknown_body,
            "byte identical, so neither reveals anything about the account"
        );

        // Give the spawned lookup time to run, then confirm it matched no
        // account: not the one just created, and not some other account that
        // also has no address (the anonymous sentinel row always does), which
        // is what an unguarded `LOWER(mail) = LOWER('')` would otherwise find.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM password_reset_tokens t \
             JOIN users u ON u.id = t.user_id \
             WHERE u.mail = '' AND t.created > $1",
        )
        .bind(started_at)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(
            leaked, 0,
            "no reset was queued for any account with no address"
        );
    });
}

/// **Red on `main`: self registration with no address is still refused.**
#[test]
fn self_registration_without_an_address_is_still_refused() {
    run_test(async {
        let app = shared_app().await;
        set_registration(app, RegistrationMode::Open).await;
        let ip = test_ip_for("b309-register-no-mail");
        app.state.rate_limiter().reset("register", &ip).await.ok();

        let form = get(app, "/user/register", "").await;
        assert_eq!(form.status, StatusCode::OK, "{}", form.body);
        let fields = common::form_fields(&form.body, "user_register");
        let name = common::username("b309-applicant");
        let body = format!(
            "username={}&mail=&password={p}&confirm_password={p}&{fields}",
            enc(&name),
            p = enc("long-enough-pass-1")
        );
        let a = post_form(app, "/user/register", &form.cookies, body, &ip).await;
        assert!(a.body.contains("Email address is required."), "{}", a.body);
        assert!(
            app.state
                .users()
                .find_by_name(&name)
                .await
                .unwrap()
                .is_none(),
            "no account was created"
        );
    });
}

/// **Red on `main`: an administrator can clear an account's address, and
/// later give it one, through the edit form.**
#[test]
fn admin_edit_form_clears_then_sets_the_mail() {
    run_test(async {
        let app = shared_app().await;
        let (_, admin_cookies) = admin(app, "b309-admin-c").await;
        let name = common::username("b309-editable");
        let mail = format!("{name}@example.com");
        app.create_test_user(&name, PASSWORD, &mail).await;
        delete_user_named_on_exit(app, &name);
        let user_id = common::user_id_of(app, &name).await;

        // Clear it.
        let edit_path = format!("/admin/people/{user_id}/edit");
        let form = get(app, &edit_path, &admin_cookies).await;
        assert_eq!(form.status, StatusCode::OK, "{}", form.body);
        let token = hidden(&form.body, "_token");
        let body = format!(
            "_token={token}&_form_build_id=x&name={}&mail=&status=1",
            enc(&name)
        );
        let a = post_form(
            app,
            &edit_path,
            &admin_cookies,
            body,
            &test_ip_for("b309-edit-clear"),
        )
        .await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        app.state.users().invalidate(user_id);
        let cleared = app
            .state
            .users()
            .find_by_id(user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cleared.mail, "", "the address was cleared");

        // Set it again.
        let form = get(app, &edit_path, &admin_cookies).await;
        let token = hidden(&form.body, "_token");
        let new_mail = format!("{name}-new@example.com");
        let body = format!(
            "_token={token}&_form_build_id=x&name={}&mail={}&status=1",
            enc(&name),
            enc(&new_mail)
        );
        let a = post_form(
            app,
            &edit_path,
            &admin_cookies,
            body,
            &test_ip_for("b309-edit-set"),
        )
        .await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        app.state.users().invalidate(user_id);
        let restored = app
            .state
            .users()
            .find_by_id(user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.mail, new_mail, "the address was set again");
    });
}

/// **Red on `main`: a `trovato import` `user` record with no `mail` creates
/// an account that signs in by name.**
#[test]
fn import_user_record_with_no_mail_creates_an_account_that_logs_in_by_name() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b309-imported");
        delete_user_named_on_exit(app, &name);
        let id = Uuid::now_v7();
        let line = json!({
            "type": "user",
            "id": id,
            "name": name,
            "status": 1,
            "created": 1_700_000_000,
        });
        let text = format!("{line}\n");

        let report = import(&app.state, text.as_bytes(), false)
            .await
            .expect("the import succeeds");
        assert_eq!(report.of("user").created, 1, "{report:?}");

        let user = User::find_by_id(&app.db, id)
            .await
            .unwrap()
            .expect("the account exists");
        assert_eq!(user.name, name);
        assert_eq!(user.mail, "");

        // No password was given, so the account has none yet; set one the way
        // an operator finishes an imported account, and confirm the name
        // signs in (what matters here is that no address was required to
        // import or to sign in, not the password path itself).
        trovato_kernel::services::user::reset_password_offline(&app.db, id, PASSWORD)
            .await
            .unwrap();
        assert_eq!(login_json(app, &name, PASSWORD).await, StatusCode::OK);
    });
}

/// **Red on `main`: two imported accounts with no address do not collide.**
#[test]
fn import_two_user_records_with_no_mail_coexist() {
    run_test(async {
        let app = shared_app().await;
        let a_name = common::username("b309-import-twin-a");
        let b_name = common::username("b309-import-twin-b");
        delete_user_named_on_exit(app, &a_name);
        delete_user_named_on_exit(app, &b_name);
        let a_id = Uuid::now_v7();
        let b_id = Uuid::now_v7();
        let lines = [
            json!({"type": "user", "id": a_id, "name": a_name, "mail": "", "status": 1, "created": 1_700_000_000}),
            json!({"type": "user", "id": b_id, "name": b_name, "mail": "", "status": 1, "created": 1_700_000_000}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();

        let report = import(&app.state, text.as_bytes(), false)
            .await
            .expect("the import succeeds");
        assert_eq!(report.of("user").created, 2, "{report:?}");

        let a = User::find_by_id(&app.db, a_id).await.unwrap().unwrap();
        let b = User::find_by_id(&app.db, b_id).await.unwrap().unwrap();
        assert_eq!(a.mail, "");
        assert_eq!(b.mail, "");
    });
}

// =============================================================================
// `trovato user create` (through the real binary; a scratch database of its
// own, as `zz_operator_cli_test.rs` runs its own CLI tests)
// =============================================================================

mod cli {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;

    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;
    use trovato_test_utils::ScratchDb;
    use uuid::Uuid;

    fn bin() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_trovato"))
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("trovato-{label}-{}", Uuid::now_v7().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(database_url: &str, work: &TempDir) -> Command {
        let mut cmd = Command::new(bin());
        cmd.env_clear()
            .current_dir(&work.0)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("DATABASE_URL", database_url)
            .env("REDIS_URL", trovato_test_utils::env::redis_url())
            .env("DATABASE_MAX_CONNECTIONS", "5")
            .env("RUST_LOG", "warn")
            .env("TEMPLATES_DIR", repo_root().join("templates"))
            .env("STATIC_DIR", repo_root().join("static"))
            .env("UPLOADS_DIR", work.0.join("uploads"))
            .env("THEMES_DIR", work.0.join("themes"))
            .env("PLUGINS_DIR", work.0.join("plugins"))
            .env("COOKIE_SECURE", "never")
            .kill_on_drop(true);
        cmd
    }

    struct Ran {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    }

    impl Ran {
        fn all(&self) -> String {
            format!(
                "exit {:?}\n--- stdout\n{}--- stderr\n{}",
                self.code, self.stdout, self.stderr
            )
        }
    }

    async fn run(database_url: &str, args: &[&str], stdin: &str) -> Ran {
        let work = TempDir::new("b309-cli");
        let mut child = command(database_url, &work)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn trovato");
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin.as_bytes()).await.unwrap();
        drop(input);
        let out = child.wait_with_output().await.unwrap();
        Ran {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }
    }

    async fn migrated(label: &str) -> ScratchDb {
        let db = ScratchDb::create(label).await;
        trovato_kernel::db::run_migrations(db.pool())
            .await
            .expect("migrate the scratch database");
        db
    }

    #[derive(sqlx::FromRow)]
    struct Account {
        mail: Option<String>,
    }

    async fn account(pool: &sqlx::PgPool, name: &str) -> Option<Account> {
        sqlx::query_as("SELECT mail FROM users WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    async fn password_verifies(pool: &sqlx::PgPool, name: &str, password: &str) -> bool {
        trovato_kernel::models::User::find_by_name(pool, name)
            .await
            .unwrap()
            .expect("the account exists")
            .verify_password(password)
    }

    /// **Red on `main`: `trovato user create` with no `--mail` creates an
    /// account (instead of exiting 1 for a missing address), stored with no
    /// address, that signs in with its name and password.**
    #[tokio::test]
    async fn user_create_without_mail_logs_in_by_name() {
        let db = migrated("b309_cli_no_mail").await;

        let ran = run(
            &db.url(),
            &["user", "create", "b309_cli_carol"],
            &format!("{}\n", super::PASSWORD),
        )
        .await;
        assert_eq!(ran.code, Some(0), "{}", ran.all());

        let carol = account(db.pool(), "b309_cli_carol")
            .await
            .expect("the account exists");
        assert_eq!(
            carol.mail.as_deref(),
            Some(""),
            "created with no address, not refused"
        );
        assert!(password_verifies(db.pool(), "b309_cli_carol", super::PASSWORD).await);
    }
}
