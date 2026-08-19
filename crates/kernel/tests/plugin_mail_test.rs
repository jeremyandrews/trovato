#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin sends mail, and can only send it to the site's own address
//! (**the `mail` host interface**, added at `KERNEL_API_VERSION (0,101)`).
//!
//! The kernel has had SMTP, templates and a circuit breaker in
//! `services/email.rs` since before the freeze, and no seam onto it: a plugin
//! that needed to notify somebody posted to a webhook over `http`, which is not
//! email. This drives the seam through the **real** `plugins/test_plugin_api`
//! wasm, over HTTP, into a **real SMTP conversation**.
//!
//! # Why a socket rather than a capture hook
//!
//! Proving "a mail was sent" needs something to send it to. The alternative was a
//! capture mode on `EmailService`, which means test-only behaviour inside
//! production code on a path that sends mail to real people. So these tests run a
//! throwaway SMTP server on a loopback port and read the conversation: the
//! recipient the kernel chose, the headers it wrote, and the MIME parts lettre
//! built. Nothing about the kernel changes shape to be testable, and the
//! assertions are about bytes on a socket rather than about an internal flag.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_plugin_api/`.

mod common;

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use trovato_kernel::models::SiteConfig;

const PLUGIN: &str = "test_plugin_api";

/// The address the site publishes as its own. The plugin never names it.
const SITE_CONTACT: &str = "owner@example.test";

/// The address the site sends *from*, which is a transport identity and not a
/// contact address — asserted distinct so a bug that confuses the two shows up.
const SITE_FROM: &str = "no-reply@example.test";

/// Serializes the tests that write the site-wide `site_mail` key, the way
/// `update_banner_test.rs` serializes the ones that write `update_status`. One
/// key, one shared database, and tests that need it to say different things.
static SITE_MAIL_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

// ---------------------------------------------------------------------------
// A throwaway SMTP server
// ---------------------------------------------------------------------------

/// One message as the server received it.
#[derive(Default, Debug, Clone)]
struct Envelope {
    /// Addresses from `RCPT TO`, which is where the mail was actually going.
    recipients: Vec<String>,
    /// Everything between `DATA` and the terminating dot.
    data: String,
}

/// A loopback SMTP server that accepts everything and remembers it.
struct SmtpSink {
    port: u16,
    messages: Arc<Mutex<Vec<Envelope>>>,
}

impl SmtpSink {
    /// Bind an ephemeral port and start accepting.
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port for the SMTP sink");
        let port = listener
            .local_addr()
            .expect("read the sink's local address")
            .port();
        let messages: Arc<Mutex<Vec<Envelope>>> = Arc::new(Mutex::new(Vec::new()));

        let accepted = Arc::clone(&messages);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let messages = Arc::clone(&accepted);
                tokio::spawn(async move {
                    serve_one_session(stream, messages).await;
                });
            }
        });

        Self { port, messages }
    }

    /// Every message received so far.
    fn messages(&self) -> Vec<Envelope> {
        self.messages.lock().expect("sink mutex").clone()
    }
}

/// Speak enough SMTP for lettre to deliver one message.
///
/// The EHLO reply advertises no extensions on purpose: nothing to negotiate
/// means no STARTTLS upgrade attempt against a server that has no certificate.
async fn serve_one_session(stream: tokio::net::TcpStream, messages: Arc<Mutex<Vec<Envelope>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut write) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut envelope = Envelope::default();

    if write.write_all(b"220 sink.test ESMTP\r\n").await.is_err() {
        return;
    }

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let command = line.trim_end().to_string();
        let upper = command.to_ascii_uppercase();

        let reply: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            b"250 sink.test\r\n"
        } else if upper.starts_with("RCPT TO") {
            envelope.recipients.push(address_in(&command));
            b"250 OK\r\n"
        } else if upper.starts_with("DATA") {
            if write
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await
                .is_err()
            {
                return;
            }
            let mut data = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                if line.trim_end() == "." {
                    break;
                }
                data.push_str(&line);
            }
            envelope.data = data;
            // Recorded before the acceptance is written, so a caller whose send
            // has returned is guaranteed to see it.
            messages
                .lock()
                .expect("sink mutex")
                .push(std::mem::take(&mut envelope));
            b"250 Queued\r\n"
        } else if upper.starts_with("QUIT") {
            let _ = write.write_all(b"221 Bye\r\n").await;
            return;
        } else if upper.starts_with("RSET") {
            envelope = Envelope::default();
            b"250 OK\r\n"
        } else {
            // MAIL FROM, NOOP, and anything else this does not need to model.
            b"250 OK\r\n"
        };

        if write.write_all(reply).await.is_err() {
            return;
        }
    }
}

/// The address inside `RCPT TO:<someone@example.test>`.
fn address_in(command: &str) -> String {
    match (command.find('<'), command.rfind('>')) {
        (Some(open), Some(close)) if close > open + 1 => command[open + 1..close].to_string(),
        // No angle brackets is legal enough for a sink; keep whatever followed the colon.
        _ => command
            .split_once(':')
            .map(|(_, rest)| rest.trim().to_string())
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Install and enable the fixture plugin.
async fn install_fixture() {
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
}

/// An app whose mail goes to `port`, or nowhere at all when `port` is `None`.
async fn app_with_smtp(port: Option<u16>) -> TestApp {
    install_fixture().await;
    TestApp::with_config(move |config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
        config.smtp_host = port.map(|_| "127.0.0.1".to_string());
        if let Some(port) = port {
            config.smtp_port = port;
        }
        config.smtp_encryption = "none".to_string();
        config.smtp_from_email = SITE_FROM.to_string();
    })
    .await
}

/// A sink and an app pointed at it, built once for the whole binary.
struct Fixture {
    sink: SmtpSink,
    app: TestApp,
}

/// The app whose mail reaches a sink, and the sink.
static WITH_SMTP: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();

/// The app with no SMTP host configured at all.
static WITHOUT_SMTP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

/// Build a fixture on another thread, the way `plugin_api_test` does.
///
/// `TestApp` construction resolves the enabled plugin set and compiles every
/// plugin's wasm, and its future is not `Send`, which `common::run_test` requires.
/// Running it under `block_on` on the shared runtime from a scratch thread keeps
/// the construction out of the test's own future — and the sink's accept loop
/// still lands on the shared runtime, so it is serving while the test runs.
fn with_smtp() -> &'static Fixture {
    WITH_SMTP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || {
            handle.block_on(async {
                let sink = SmtpSink::start().await;
                let app = app_with_smtp(Some(sink.port)).await;
                Fixture { sink, app }
            })
        })
        .join()
        .expect("mail fixture app init thread panicked")
    })
}

/// The same, for a site that has configured no SMTP host.
fn without_smtp() -> &'static TestApp {
    WITHOUT_SMTP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(app_with_smtp(None)))
            .join()
            .expect("no-smtp fixture app init thread panicked")
    })
}

/// Leave the fixture disabled so it does not load in other test binaries.
async fn disable_plugin(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&app.db)
        .await
        .ok();
}

async fn text_body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A session and a CSRF token from the plugin's own form, the way a visitor gets
/// one.
async fn session_and_token(app: &TestApp) -> (String, String) {
    let response = app
        .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookies = common::extract_cookies(&response);
    let html = text_body(response).await;
    let needle = r#"name="_token" value=""#;
    let start = html.find(needle).expect("token input") + needle.len();
    let end = start + html[start..].find('"').expect("token end");
    (cookies, html[start..end].to_string())
}

/// POST the plugin's mail form.
async fn post_mail(
    app: &TestApp,
    subject: &str,
    body: &str,
    attach: bool,
) -> (StatusCode, serde_json::Value) {
    let (cookies, token) = session_and_token(app).await;
    let form = format!(
        "_token={token}&subject={subject}&body={body}&attach={}",
        if attach { "1" } else { "0" }
    );
    let response = app
        .request_with_cookies(
            Request::post("/tpa/mail")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
            &cookies,
        )
        .await;
    let status = response.status();
    let text = text_body(response).await;
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// The messages the sink received whose headers carry `subject`.
///
/// Filtered rather than counted: the tests share one sink, so "the message this
/// test sent" is the one naming its own subject.
fn messages_for(sink: &SmtpSink, subject: &str) -> Vec<Envelope> {
    sink.messages()
        .into_iter()
        .filter(|m| m.data.contains(&format!("Subject: {subject}")))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// **The seam, closed.** A public plugin route sends mail, and it arrives at the
/// site's configured address over a real SMTP conversation.
#[test]
fn a_plugin_sends_mail_to_the_site_contact_address() {
    common::run_test(async {
        let fixture = with_smtp();
        let _guard = SITE_MAIL_LOCK.lock().await;
        SiteConfig::set_site_mail(&fixture.app.db, SITE_CONTACT)
            .await
            .expect("set the site contact address");

        let (status, json) = post_mail(
            &fixture.app,
            "ContactFormMessage",
            "HelloFromAVisitor",
            false,
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["sent"], true, "the plugin reported: {json}");

        let messages = messages_for(&fixture.sink, "ContactFormMessage");
        assert_eq!(messages.len(), 1, "expected one message: {messages:?}");
        let message = &messages[0];

        // The recipient the plugin never named.
        assert_eq!(
            message.recipients,
            vec![SITE_CONTACT.to_string()],
            "mail must go to the site's configured address and nowhere else"
        );
        // And the from address is the transport identity, not the contact address.
        assert!(
            message.data.contains(&format!("From: {SITE_FROM}")),
            "{}",
            message.data
        );
        assert!(
            message.data.contains("HelloFromAVisitor"),
            "{}",
            message.data
        );
    });
}

/// An attachment arrives as a `multipart/mixed` part carrying the filename the
/// plugin chose, which is the half of the interface the WIT record describes.
#[test]
fn an_attachment_arrives_as_a_mime_part() {
    common::run_test(async {
        let fixture = with_smtp();
        let _guard = SITE_MAIL_LOCK.lock().await;
        SiteConfig::set_site_mail(&fixture.app.db, SITE_CONTACT)
            .await
            .expect("set the site contact address");

        let (status, json) =
            post_mail(&fixture.app, "WithAttachment", "AttachedBodyText", true).await;

        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["sent"], true, "{json}");

        let messages = messages_for(&fixture.sink, "WithAttachment");
        assert_eq!(messages.len(), 1, "{messages:?}");
        let data = &messages[0].data;

        assert!(
            data.contains("multipart/mixed"),
            "an attachment makes the message multipart: {data}"
        );
        assert!(
            data.contains(r#"filename="message.txt""#),
            "the attachment keeps the plugin's filename: {data}"
        );
        // The text body is still there alongside it, rather than replaced by it.
        assert!(
            data.contains("text/plain"),
            "the body part survives: {data}"
        );
    });
}

/// With no `site_mail` configured there is nowhere for the message to go, and the
/// kernel says so rather than falling back to the `from` address.
#[test]
fn with_no_site_contact_address_the_send_is_refused() {
    common::run_test(async {
        let fixture = with_smtp();
        let _guard = SITE_MAIL_LOCK.lock().await;
        SiteConfig::set_site_mail(&fixture.app.db, "")
            .await
            .expect("clear the site contact address");

        let (status, json) = post_mail(&fixture.app, "NoRecipient", "Body", false).await;

        // Restore before asserting, so a failure here does not leave the key
        // empty for whichever test runs next.
        SiteConfig::set_site_mail(&fixture.app.db, SITE_CONTACT)
            .await
            .ok();

        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["sent"], false, "{json}");
        assert_eq!(
            json["code"],
            trovato_sdk::host_errors::ERR_MAIL_NO_RECIPIENT,
            "expected ERR_MAIL_NO_RECIPIENT: {json}"
        );
        assert!(
            messages_for(&fixture.sink, "NoRecipient").is_empty(),
            "nothing may be sent when there is no recipient"
        );
    });
}

/// A site with no SMTP host configured cannot send anything, and the plugin is
/// told that rather than being left to guess from a generic failure.
#[test]
fn with_no_smtp_configured_the_plugin_is_told_so() {
    common::run_test(async {
        let app = without_smtp();

        let (status, json) = post_mail(app, "NoSmtp", "Body", false).await;

        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["sent"], false, "{json}");
        assert_eq!(
            json["code"],
            trovato_sdk::host_errors::ERR_MAIL_NOT_CONFIGURED,
            "expected ERR_MAIL_NOT_CONFIGURED: {json}"
        );
    });
}

/// Leave the fixture plugin disabled so it does not load in other test binaries.
///
/// A test rather than a teardown hook, because the harness has none; it runs last
/// by name, and the tests it follows do not depend on the plugin staying enabled.
#[test]
fn zz_leaves_the_fixture_plugin_disabled() {
    common::run_test(async {
        let fixture = with_smtp();
        disable_plugin(&fixture.app).await;
    });
}
