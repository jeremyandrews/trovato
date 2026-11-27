#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin mails one of the site's own members by user id (**BL-94**,
//! G-MAIL-CANNOT-REACH-A-USER): `mail.send-to-user`, with opt out and
//! unsubscribe.
//!
//! The `mail` interface could only reach the site's own contact address, so no
//! plugin could send a member a notification or a digest. `send-to-user` takes
//! a user id, never an address: the kernel resolves it, the plugin never sees
//! it, and every message carries a working unsubscribe link and the
//! `List-Unsubscribe` headers a mail client's one-click button uses. Declaring
//! `mail` alone does not grant it; the plugin declares `mail_users = true`.
//!
//! Driven through the **real** `plugins/test_members` wasm over HTTP into a
//! loopback SMTP sink. Requires Postgres + Redis and the fixture `.wasm`.

mod common;

use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use common::smtp_sink::{Envelope, SmtpSink};
use trovato_sdk::host_errors;
use uuid::Uuid;

const PLUGIN: &str = "test_members";
const SITE_FROM: &str = "no-reply@bl94.example.test";
const BUCKET: &str = "plugin_mail_to_user_test";

struct Fixture {
    sink: SmtpSink,
    app: TestApp,
}

static CAPABLE: OnceLock<Fixture> = OnceLock::new();
static WITHOUT_CAPABILITY: OnceLock<Fixture> = OnceLock::new();

/// Serializes the tests that spend or reset the shared mail buckets.
static BUCKET_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn install_fixture() {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "0.100.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;
}

fn build(extra_dir: Option<std::path::PathBuf>) -> Fixture {
    let handle = common::shared_runtime_handle();
    std::thread::spawn(move || {
        handle.block_on(async move {
            install_fixture().await;
            let sink = SmtpSink::start().await;
            let port = sink.port;
            let app = TestApp::with_config(move |config| {
                if std::env::var_os("PLUGINS_DIR").is_none() {
                    config.plugins_dirs = vec![common::project_root().join("plugins")];
                }
                if let Some(dir) = extra_dir {
                    config.plugins_dirs.push(dir);
                }
                config.smtp_host = Some("127.0.0.1".to_string());
                config.smtp_port = port;
                config.smtp_encryption = "none".to_string();
                config.smtp_from_email = SITE_FROM.to_string();
            })
            .await;
            Fixture { sink, app }
        })
    })
    .join()
    .expect("app init thread panicked")
}

/// The fixture as shipped: `mail` and `mail_users = true`.
fn capable() -> &'static Fixture {
    CAPABLE.get_or_init(|| build(None))
}

/// The same wasm with `mail_users` removed from its manifest.
fn without_capability() -> &'static Fixture {
    WITHOUT_CAPABILITY.get_or_init(|| {
        let dir = common::plugin_copy::copy_with_manifest(PLUGIN, |manifest| {
            assert!(manifest.contains("mail_users = true"));
            manifest.replace("mail_users = true", "")
        });
        build(Some(dir))
    })
}

async fn text_body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn token_in(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html.find(needle).expect("token input") + needle.len();
    let end = start + html[start..].find('"').expect("token end");
    html[start..end].to_string()
}

/// Ask the plugin to mail `user_id`; returns the plugin's JSON answer and its
/// raw text.
async fn mail_user(
    app: &TestApp,
    user_id: &str,
    subject: &str,
    body: &str,
) -> (serde_json::Value, String) {
    mail_user_query(
        app,
        &format!(
            "user_id={}&subject={}&body={}",
            urlencoding::encode(user_id),
            urlencoding::encode(subject),
            urlencoding::encode(body)
        ),
    )
    .await
}

/// Call the fixture's mail route with `query` as built by the caller.
async fn mail_user_query(app: &TestApp, query: &str) -> (serde_json::Value, String) {
    let path = format!("/test-members/mail-user?{query}");
    let response = app
        .request(
            Request::get(path)
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = text_body(response).await;
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    deliver_queued(app).await;
    (json, text)
}

/// Send what this plugin queued (BG18: plugin mail is queued, and a worker
/// sends it). The plugin's category names its rows, and its recipients are
/// this binary's own members.
async fn deliver_queued(app: &TestApp) {
    if app.state.email().is_none() {
        return;
    }
    let recipients: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT recipient FROM mail_outbox WHERE category = $1 AND status = 'queued'",
    )
    .bind(format!("plugin:{PLUGIN}"))
    .fetch_all(&app.db)
    .await
    .unwrap();
    let recipients: Vec<&str> = recipients.iter().map(String::as_str).collect();
    app.deliver_mail_to(&recipients).await;
}

fn messages_for(sink: &SmtpSink, subject: &str) -> Vec<Envelope> {
    sink.messages()
        .into_iter()
        .filter(|m| m.data.contains(&format!("Subject: {subject}")))
        .collect()
}

/// A member with an address; returns `(id, address)`.
async fn member(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let mail = format!("{name}@member.bl94.example.test");
    app.create_test_user(&name, "bl94-Password-123", &mail)
        .await;
    (common::user_id_of(app, &name).await, mail)
}

/// A subject unique to one test, so the shared sink can be filtered by it.
fn subject(tag: &str) -> String {
    format!("{tag} {}", Uuid::now_v7().simple())
}

/// The message as one string with folded lines joined, so a header or a URL
/// split across lines reads whole.
fn unfolded(message: &Envelope) -> String {
    message
        .data
        .replace("=\r\n", "")
        .replace("=\n", "")
        .replace("\r\n ", " ")
        .replace("\n ", " ")
}

/// The unsubscribe path a delivered message carries in its body.
fn unsubscribe_path(message: &Envelope) -> String {
    let data = unfolded(message);
    let start = data
        .find("/user/mail/unsubscribe/")
        .expect("the message carries an unsubscribe link");
    data[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
        .collect()
}

async fn reset_buckets(app: &TestApp, user: Uuid) {
    let limiter = app.state.rate_limiter();
    limiter
        .reset("mail_users", &format!("plugin:{PLUGIN}"))
        .await
        .ok();
    limiter
        .reset("mail_recipient", &format!("plugin:{PLUGIN}:user:{user}"))
        .await
        .ok();
}

async fn opted_out_at(app: &TestApp, user: Uuid) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar(
        "SELECT opted_out_at FROM user_mail_preference WHERE user_id = $1 AND plugin = $2",
    )
    .bind(user)
    .bind(PLUGIN)
    .fetch_optional(&app.db)
    .await
    .unwrap()
    .flatten()
}

async fn page(app: &TestApp, request: Request<Body>) -> (StatusCode, String) {
    let response = app.request(request).await;
    let status = response.status();
    (status, text_body(response).await)
}

// ---------------------------------------------------------------------------
// The send
// ---------------------------------------------------------------------------

/// A normal send reaches the member's address from the kernel's `from`
/// address, with an unsubscribe footer and the two `List-Unsubscribe` headers,
/// and the plugin is told only that it was sent.
#[test]
fn a_member_is_mailed_with_an_unsubscribe_link_and_headers() {
    common::run_test(async {
        let fixture = capable();
        let app = &fixture.app;
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let (user, address) = member(app, "bl94-send").await;
        reset_buckets(app, user).await;
        let subject = subject("MemberNotice");

        let (json, text) = mail_user(app, &user.to_string(), &subject, "Hello member").await;

        assert_eq!(json["sent"], true, "{text}");
        assert!(
            !text.contains(&address),
            "the plugin never sees the address"
        );
        let messages = messages_for(&fixture.sink, &subject);
        assert_eq!(messages.len(), 1, "{messages:?}");
        let message = &messages[0];
        assert_eq!(message.recipients, vec![address.clone()]);
        let data = unfolded(message);
        assert!(
            data.lines()
                .any(|l| l.starts_with("From: ")
                    && l.trim_end().ends_with(&format!("<{SITE_FROM}>"))),
            "{data}"
        );
        assert!(data.contains("Hello member"), "{data}");
        let path = unsubscribe_path(message);
        let header_line = data
            .lines()
            .find(|line| line.starts_with("List-Unsubscribe: <"))
            .unwrap_or_else(|| panic!("a List-Unsubscribe header: {data}"));
        assert!(
            header_line.contains(&format!("{path}>")),
            "the header carries the same link as the footer: {header_line}"
        );
        assert!(
            data.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"),
            "{data}"
        );
    });
}

/// Without `mail_users` the call is refused and nothing is sent, although the
/// plugin declares `mail`.
#[test]
fn without_the_capability_the_send_is_denied() {
    common::run_test(async {
        let fixture = without_capability();
        let app = &fixture.app;
        // Held although this test queues nothing: its call still drains what
        // this plugin queued, and must not send another test's row to this
        // fixture's sink.
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let (user, _) = member(app, "bl94-denied").await;
        let subject = subject("Denied");

        let (json, text) = mail_user(app, &user.to_string(), &subject, "body").await;

        assert_eq!(json["code"], host_errors::ERR_MAIL_USERS_DENIED, "{text}");
        assert!(messages_for(&fixture.sink, &subject).is_empty());
    });
}

/// An unknown user, a blocked one and one with no address all get the one
/// recipient-unavailable code, and nothing is sent.
#[test]
fn an_unknown_blocked_or_addressless_user_is_unavailable() {
    common::run_test(async {
        let fixture = capable();
        let app = &fixture.app;
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let (blocked, _) = member(app, "bl94-blocked").await;
        sqlx::query("UPDATE users SET status = 0 WHERE id = $1")
            .bind(blocked)
            .execute(&app.db)
            .await
            .unwrap();
        let (addressless, _) = member(app, "bl94-noaddr").await;
        sqlx::query("UPDATE users SET mail = '' WHERE id = $1")
            .bind(addressless)
            .execute(&app.db)
            .await
            .unwrap();

        for (label, user) in [
            ("unknown", Uuid::now_v7().to_string()),
            ("blocked", blocked.to_string()),
            ("addressless", addressless.to_string()),
            ("malformed", "not-a-user-id".to_string()),
        ] {
            let subject = subject(&format!("Unavailable{label}"));
            let (json, text) = mail_user(app, &user, &subject, "body").await;
            assert_eq!(
                json["code"],
                host_errors::ERR_MAIL_RECIPIENT_UNAVAILABLE,
                "{label}: {text}"
            );
            assert!(
                messages_for(&fixture.sink, &subject).is_empty(),
                "{label}: nothing sent"
            );
        }
    });
}

/// Subject header injection and an oversized subject or body are refused the
/// way `send-to-site-contacts` refuses them.
#[test]
fn a_malformed_message_is_refused_as_send_to_site_contacts_refuses_it() {
    common::run_test(async {
        let fixture = capable();
        let app = &fixture.app;
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let (user, _) = member(app, "bl94-invalid").await;
        reset_buckets(app, user).await;

        for (label, subject, body) in [
            (
                "injection",
                "Hello\r\nBcc: victim@example.test".to_string(),
                "body".to_string(),
            ),
            ("long subject", "s".repeat(513), "body".to_string()),
            ("empty body", "Subject".to_string(), String::new()),
        ] {
            let (json, text) = mail_user(app, &user.to_string(), &subject, &body).await;
            assert_eq!(
                json["code"],
                host_errors::ERR_MAIL_INVALID_REQUEST,
                "{label}: {}",
                &text[..text.len().min(200)]
            );
        }
        // A body over the 256 KiB limit is too long for a URL, so the fixture
        // builds it from its length.
        let (json, text) = mail_user_query(
            app,
            &format!("user_id={user}&subject=Subject&body_len={}", 256 * 1024 + 1),
        )
        .await;
        assert_eq!(
            json["code"],
            host_errors::ERR_MAIL_INVALID_REQUEST,
            "long body: {}",
            &text[..text.len().min(200)]
        );
        assert!(
            fixture
                .sink
                .messages()
                .iter()
                .all(|m| !m.data.contains("victim@example.test")),
            "nothing injected reached the wire"
        );
    });
}

/// The per-recipient bucket refuses past its limit (10 an hour by default),
/// and so does the per-plugin one (1000 an hour).
#[test]
fn the_per_recipient_and_per_plugin_buckets_refuse_past_their_limits() {
    common::run_test(async {
        let fixture = capable();
        let app = &fixture.app;
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let limiter = app.state.rate_limiter();
        let (user, _) = member(app, "bl94-bucket").await;
        reset_buckets(app, user).await;

        let recipient_key = format!("plugin:{PLUGIN}:user:{user}");
        for _ in 0..10 {
            limiter
                .check("mail_recipient", &recipient_key)
                .await
                .expect("within the recipient bucket");
        }
        let subject_one = subject("RecipientBucket");
        let (json, text) = mail_user(app, &user.to_string(), &subject_one, "body").await;
        assert_eq!(json["code"], host_errors::ERR_MAIL_RATE_LIMITED, "{text}");
        assert!(messages_for(&fixture.sink, &subject_one).is_empty());
        reset_buckets(app, user).await;

        let plugin_key = format!("plugin:{PLUGIN}");
        for _ in 0..1000 {
            limiter
                .check("mail_users", &plugin_key)
                .await
                .expect("within the plugin bucket");
        }
        let subject_two = subject("PluginBucket");
        let (json, text) = mail_user(app, &user.to_string(), &subject_two, "body").await;
        reset_buckets(app, user).await;
        assert_eq!(json["code"], host_errors::ERR_MAIL_RATE_LIMITED, "{text}");
        assert!(messages_for(&fixture.sink, &subject_two).is_empty());
    });
}

// ---------------------------------------------------------------------------
// Unsubscribing
// ---------------------------------------------------------------------------

/// GET on the link is a confirmation page and changes nothing; POST with no
/// session and no CSRF token, as a mail client's one-click sends it, records
/// the opt out, and again changes nothing more. A later send is refused with
/// its own code.
#[test]
fn the_unsubscribe_link_confirms_on_get_and_opts_out_on_post() {
    common::run_test(async {
        let fixture = capable();
        let app = &fixture.app;
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let (user, _) = member(app, "bl94-unsub").await;
        reset_buckets(app, user).await;
        let first = subject("BeforeOptOut");
        let (json, _) = mail_user(app, &user.to_string(), &first, "body").await;
        assert_eq!(json["sent"], true);
        let path = unsubscribe_path(&messages_for(&fixture.sink, &first)[0]);

        let (status, html) = page(
            app,
            Request::get(&path)
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(r#"method="post""#), "{html}");
        assert!(html.contains(&format!(r#"action="{path}""#)), "{html}");
        assert!(
            opted_out_at(app, user).await.is_none(),
            "GET changes nothing"
        );

        // A one-click POST: no cookie, no CSRF token.
        let one_click = || {
            Request::post(&path)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::from("List-Unsubscribe=One-Click"))
                .unwrap()
        };
        let (status, html) = page(app, one_click()).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(html.contains("You will no longer receive"), "{html}");
        let when = opted_out_at(app, user)
            .await
            .expect("the opt out is recorded");

        let (status, _) = page(app, one_click()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            opted_out_at(app, user).await,
            Some(when),
            "a second POST is idempotent"
        );

        let after = subject("AfterOptOut");
        let (json, text) = mail_user(app, &user.to_string(), &after, "body").await;
        assert_eq!(json["code"], host_errors::ERR_MAIL_OPTED_OUT, "{text}");
        assert!(messages_for(&fixture.sink, &after).is_empty());
    });
}

/// A forged or unknown token changes nothing and says the link is invalid.
#[test]
fn a_forged_token_changes_nothing() {
    common::run_test(async {
        let app = &capable().app;
        let before: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_mail_preference WHERE opted_out_at IS NOT NULL",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        let path = format!("/user/mail/unsubscribe/{}", "A".repeat(43));

        for request in [
            Request::get(&path)
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::empty())
                .unwrap(),
            Request::post(&path)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::from("List-Unsubscribe=One-Click"))
                .unwrap(),
        ] {
            let (status, html) = page(app, request).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(
                html.contains("This unsubscribe link is not valid"),
                "{html}"
            );
        }
        let after: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_mail_preference WHERE opted_out_at IS NOT NULL",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(before, after, "nothing changed");
    });
}

/// The profile page lists the plugins the member opted out of, each with a
/// Resubscribe button, and resubscribing clears the opt out. With none, the
/// section is absent.
#[test]
fn the_profile_page_lists_opt_outs_and_resubscribes() {
    common::run_test(async {
        let fixture = capable();
        let app = &fixture.app;
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let name = common::username("bl94-profile");
        let mail = format!("{name}@member.bl94.example.test");
        let cookies = app
            .create_and_login_user(&name, "bl94-Password-123", &mail)
            .await;
        let user = common::user_id_of(app, &name).await;

        let profile = || async {
            let response = app
                .request_with_cookies(
                    Request::get("/user/profile").body(Body::empty()).unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            text_body(response).await
        };

        let html = profile().await;
        assert!(
            !html.contains(r#"id="mail-preferences""#),
            "nothing shown with no opt outs"
        );

        reset_buckets(app, user).await;
        let first = subject("ProfileOptOut");
        mail_user(app, &user.to_string(), &first, "body").await;
        let path = unsubscribe_path(&messages_for(&fixture.sink, &first)[0]);
        page(
            app,
            Request::post(&path)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::from("List-Unsubscribe=One-Click"))
                .unwrap(),
        )
        .await;
        assert!(opted_out_at(app, user).await.is_some());

        let html = profile().await;
        assert!(html.contains(r#"id="mail-preferences""#), "{html}");
        assert!(
            html.contains(r#"action="/user/mail/resubscribe""#),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"name="plugin" value="{PLUGIN}""#)),
            "{html}"
        );

        // Resubscribe, with the CSRF token the profile page carries for it.
        let section = &html[html.find(r#"id="mail-preferences""#).unwrap()..];
        let token = token_in(section);
        let response = app
            .request_with_cookies(
                Request::post("/user/mail/resubscribe")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "_token={}&_form_build_id={}&plugin={PLUGIN}",
                        urlencoding::encode(&token),
                        common::form_build_id(section, "user_mail_resubscribe")
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(response.status().is_redirection(), "{}", response.status());
        assert!(opted_out_at(app, user).await.is_none(), "resubscribed");

        // Without a CSRF token, resubscribing is refused.
        sqlx::query("UPDATE user_mail_preference SET opted_out_at = now() WHERE user_id = $1")
            .bind(user)
            .execute(&app.db)
            .await
            .unwrap();
        let response = app
            .request_with_cookies(
                Request::post("/user/mail/resubscribe")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("_token=forged&plugin={PLUGIN}")))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(!response.status().is_redirection());
        assert!(opted_out_at(app, user).await.is_some(), "still opted out");
    });
}

/// Deleting a user deletes their preference rows.
#[test]
fn deleting_the_user_deletes_their_rows() {
    common::run_test(async {
        let fixture = capable();
        let app = &fixture.app;
        common::hold_until_cleanup(BUCKET_LOCK.lock().await);
        let (user, _) = member(app, "bl94-delete").await;
        reset_buckets(app, user).await;
        let (json, _) = mail_user(app, &user.to_string(), &subject("Delete"), "body").await;
        assert_eq!(json["sent"], true);
        let rows = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM user_mail_preference WHERE user_id = $1",
            )
            .bind(user)
            .fetch_one(&app.db)
            .await
            .unwrap()
        };
        assert_eq!(rows().await, 1, "the first send creates the row");

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user)
            .execute(&app.db)
            .await
            .unwrap();

        assert_eq!(rows().await, 0, "the rows go with the user");
    });
}

/// Leave the fixture plugin disabled so it does not load in other binaries.
#[test]
fn zz_leaves_the_fixture_plugin_disabled() {
    common::run_test(async {
        let app = &capable().app;
        sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
            .bind(PLUGIN)
            .execute(&app.db)
            .await
            .ok();
    });
}
