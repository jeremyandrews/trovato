#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A visitor with no account reaches the site owner (**`trovato_contact`**).
//!
//! This is the feature the three 0.101 plugin surfaces were added for, and it
//! uses all three at once, so this file is where they are proven to work
//! *together* rather than one at a time:
//!
//! - the form posts with no JavaScript, carrying a `_token` field the kernel
//!   minted and handed to the plugin in `ApiRequest::csrf_token`;
//! - the page arrives inside the site's theme, with the site header and
//!   navigation around it;
//! - the message is delivered through the `mail` host interface to the site's
//!   configured address, which the plugin never names.
//!
//! Everything below drives the **real** `plugins/trovato_contact` wasm through
//! the real router over HTTP, into a real SMTP conversation with the loopback
//! sink in `common::smtp_sink`.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/trovato_contact/`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use common::smtp_sink::{Envelope, SmtpSink};
use trovato_kernel::models::SiteConfig;

const PLUGIN: &str = "trovato_contact";

/// The address the site publishes as its own. The plugin never names it.
const SITE_CONTACT: &str = "owner@contact-test.local";

/// The transport identity, distinct from the contact address on purpose.
const SITE_FROM: &str = "no-reply@contact-test.local";

/// Serializes the tests that write `site_mail`, which is one site-wide key.
static SITE_MAIL_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// The rate-limit bucket this file's requests land in.
///
/// Without it every request falls into the shared `127.0.0.1` bucket, which is
/// 100 requests a minute for the whole suite, and a file that adds a dozen makes
/// somebody else's test fail. `TestApp::login` does the same for logins.
const BUCKET: &str = "contact_form_test";

/// Markup only the site's page template produces.
const THEME_CHROME: [&str; 2] = [r#"class="site-header""#, r#"aria-label="Main""#];

/// The sink and an app delivering to it.
struct Fixture {
    sink: SmtpSink,
    app: TestApp,
}

static FIXTURE: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();

/// Build the fixture once, off-thread.
///
/// `TestApp` construction compiles every plugin's wasm and its future is not
/// `Send`, which `common::run_test` requires, so it runs under `block_on` on the
/// shared runtime from a scratch thread — the same shape `plugin_api_test` uses.
fn fixture() -> &'static Fixture {
    FIXTURE.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || {
            handle.block_on(async {
                let sink = SmtpSink::start().await;
                let app = build_app(sink.port).await;
                Fixture { sink, app }
            })
        })
        .join()
        .expect("contact fixture app init thread panicked")
    })
}

async fn build_app(smtp_port: u16) -> TestApp {
    trovato_test_utils::env::load_dotenv();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "0.100.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;

    TestApp::with_config(move |config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
        config.smtp_host = Some("127.0.0.1".to_string());
        config.smtp_port = smtp_port;
        config.smtp_encryption = "none".to_string();
        config.smtp_from_email = SITE_FROM.to_string();
    })
    .await
}

async fn text_body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// GET the form: the session cookie, the page, and the token out of it.
async fn get_form(app: &TestApp) -> (String, String) {
    let response = app
        .request(
            Request::get("/contact")
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the contact form must be public"
    );
    let cookies = common::extract_cookies(&response);
    let html = text_body(response).await;
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .unwrap_or_else(|| panic!("no _token in the form: {html}"))
        + needle.len();
    let end = start + html[start..].find('"').expect("token end");
    (cookies, html[start..end].to_string())
}

/// POST the form with the given body, under the given session.
async fn post_form(app: &TestApp, cookies: &str, body: String) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::post("/contact")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, text_body(response).await)
}

/// A valid submission body carrying `token`, with a unique subject so the test
/// can find its own message in the sink.
fn submission(token: &str, subject: &str, attach: bool) -> String {
    format!(
        "_token={token}&name=Ada+Lovelace&email=ada%40example.test&subject={subject}\
         &message=Please+get+in+touch&attach={}",
        if attach { "1" } else { "0" }
    )
}

/// The messages the sink holds whose subject names this test.
fn messages_for(sink: &SmtpSink, subject: &str) -> Vec<Envelope> {
    sink.messages()
        .into_iter()
        .filter(|m| m.data.contains(subject))
        .collect()
}

async fn set_site_contact(app: &TestApp) {
    SiteConfig::set_site_mail(&app.db, SITE_CONTACT)
        .await
        .expect("set the site contact address");
}

/// **All three surfaces, together.** An anonymous visitor loads the form, posts
/// it with no JavaScript and no header, and the message reaches the site's
/// configured address.
#[test]
fn a_visitor_with_no_account_sends_a_message_to_the_site_owner() {
    common::run_test(async {
        let fixture = fixture();
        let _guard = SITE_MAIL_LOCK.lock().await;
        set_site_contact(&fixture.app).await;

        let (cookies, token) = get_form(&fixture.app).await;
        let (status, html) = post_form(
            &fixture.app,
            &cookies,
            submission(&token, "AllThree", false),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(
            html.contains("Thank you, Ada Lovelace"),
            "the confirmation must name the sender: {html}"
        );
        // Themed, like the form was.
        for marker in THEME_CHROME {
            assert!(html.contains(marker), "missing {marker}: {html}");
        }

        let messages = messages_for(&fixture.sink, "AllThree");
        assert_eq!(messages.len(), 1, "{messages:?}");
        let message = &messages[0];

        // The recipient the plugin never named.
        assert_eq!(message.recipients, vec![SITE_CONTACT.to_string()]);
        assert!(
            message.data.contains("Subject: [contact] AllThree"),
            "{}",
            message.data
        );
        // The visitor's address is in the body, since the interface has no
        // Reply-To to put it in.
        assert!(
            message.data.contains("ada@example.test"),
            "{}",
            message.data
        );
        assert!(
            message.data.contains("Please get in touch"),
            "{}",
            message.data
        );
    });
}

/// The form arrives inside the site's theme, which is what makes it a usable
/// public page rather than an unstyled fragment.
#[test]
fn the_form_is_rendered_into_the_site_theme() {
    common::run_test(async {
        let fixture = fixture();

        let response = fixture
            .app
            .request(
                Request::get("/contact")
                    .header("x-forwarded-for", common::test_ip_for(BUCKET))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8"),
        );
        let html = text_body(response).await;

        assert!(html.contains("<!DOCTYPE html>"), "{html}");
        for marker in THEME_CHROME {
            assert!(html.contains(marker), "missing {marker}: {html}");
        }
        // The form itself, with no JavaScript in it.
        //
        // Asserted on the form element rather than on the page: a themed page
        // carries the site's own `trovato.js`, and the claim here is that *the
        // form* needs no scripting, not that the site ships none.
        assert!(
            html.contains(r#"<form method="post" action="/contact""#),
            "{html}"
        );
        assert!(html.contains(r#"name="message""#), "{html}");

        let form_start = html.find("<form method=\"post\"").expect("the form");
        let form_end = html[form_start..].find("</form>").expect("the form's end") + form_start;
        let form = &html[form_start..form_end];
        assert!(!form.contains("<script"), "no script in the form: {form}");
        assert!(
            !form.contains(" on"),
            "no inline event handlers in the form: {form}"
        );
    });
}

/// A post with no token is refused by the kernel before the plugin sees it, and
/// nothing is sent.
#[test]
fn a_submission_with_no_token_is_refused_and_sends_nothing() {
    common::run_test(async {
        let fixture = fixture();
        let _guard = SITE_MAIL_LOCK.lock().await;
        set_site_contact(&fixture.app).await;

        let (cookies, _) = get_form(&fixture.app).await;
        let body = "name=Ada&email=ada%40example.test&subject=NoToken&message=hello".to_string();
        let (status, _) = post_form(&fixture.app, &cookies, body).await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            messages_for(&fixture.sink, "NoToken").is_empty(),
            "a refused post must not send mail"
        );
    });
}

/// The same for a forged token: 403, and nothing sent.
#[test]
fn a_submission_with_a_forged_token_is_refused_and_sends_nothing() {
    common::run_test(async {
        let fixture = fixture();
        let _guard = SITE_MAIL_LOCK.lock().await;
        set_site_contact(&fixture.app).await;

        let (cookies, _) = get_form(&fixture.app).await;
        let (status, _) = post_form(
            &fixture.app,
            &cookies,
            submission("not-a-real-token", "Forged", false),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            messages_for(&fixture.sink, "Forged").is_empty(),
            "a forged token must not send mail"
        );
    });
}

/// Invalid input comes back as the form, with the errors and a fresh token, and
/// that fresh token works — which is the part that makes a rejected submission
/// recoverable rather than a dead end.
#[test]
fn an_invalid_submission_can_be_corrected_and_resubmitted() {
    common::run_test(async {
        let fixture = fixture();
        let _guard = SITE_MAIL_LOCK.lock().await;
        set_site_contact(&fixture.app).await;

        let (cookies, token) = get_form(&fixture.app).await;

        // No message: understood, and not acted on.
        let bad =
            format!("_token={token}&name=Ada&email=ada%40example.test&subject=Retry&message=");
        let (status, html) = post_form(&fixture.app, &cookies, bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{html}");
        assert!(html.contains("Please write a message."), "{html}");
        assert!(
            messages_for(&fixture.sink, "Retry").is_empty(),
            "an invalid submission must not send mail"
        );

        // The re-rendered form carries a fresh token, because the submitted one
        // was spent verifying the request that failed validation.
        let needle = r#"name="_token" value=""#;
        let start = html.find(needle).expect("fresh token in the re-render") + needle.len();
        let end = start + html[start..].find('"').unwrap();
        let fresh = &html[start..end];
        assert_ne!(fresh, token, "the re-render must not reuse a spent token");

        let (status, html) =
            post_form(&fixture.app, &cookies, submission(fresh, "Retry", false)).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert_eq!(messages_for(&fixture.sink, "Retry").len(), 1);
    });
}

/// The attachment path, end to end: the visitor asks for a copy and it arrives as
/// a MIME part.
#[test]
fn a_visitor_can_attach_a_copy_of_the_message() {
    common::run_test(async {
        let fixture = fixture();
        let _guard = SITE_MAIL_LOCK.lock().await;
        set_site_contact(&fixture.app).await;

        let (cookies, token) = get_form(&fixture.app).await;
        let (status, html) =
            post_form(&fixture.app, &cookies, submission(&token, "Attached", true)).await;
        assert_eq!(status, StatusCode::OK, "{html}");

        let messages = messages_for(&fixture.sink, "Attached");
        assert_eq!(messages.len(), 1, "{messages:?}");
        let data = &messages[0].data;
        assert!(data.contains("multipart/mixed"), "{data}");
        assert!(data.contains(r#"filename="message.txt""#), "{data}");
    });
}

/// What a stranger types is escaped on the way back out. The plugin escapes it,
/// because the kernel does not sanitize a plugin's HTML — so this pins the
/// plugin's own escaping through the whole stack rather than in a unit test.
#[test]
fn markup_a_visitor_types_comes_back_escaped() {
    common::run_test(async {
        let fixture = fixture();
        let _guard = SITE_MAIL_LOCK.lock().await;
        set_site_contact(&fixture.app).await;

        let (cookies, token) = get_form(&fixture.app).await;
        // Invalid on purpose (no message), so the values come back in the form.
        let body = format!(
            "_token={token}&name=%3Cscript%3Ealert(1)%3C%2Fscript%3E\
             &email=nope&subject=Escaped&message="
        );
        let (status, html) = post_form(&fixture.app, &cookies, body).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "unescaped markup reached the page: {html}"
        );
        assert!(html.contains("&lt;script&gt;"), "{html}");
    });
}

/// Leave the plugin disabled so it does not load in other test binaries.
#[test]
fn zz_leaves_the_plugin_disabled() {
    common::run_test(async {
        let fixture = fixture();
        sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
            .bind(PLUGIN)
            .execute(&fixture.app.db)
            .await
            .ok();
    });
}
