#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B144: a site browsed by IP address starts, and its absolute URLs carry
//! that address.
//!
//! Before this fix, `AppState::new` built the WebAuthn relying party from
//! `SITE_URL` with `?`, and `webauthn-rs` refuses an IP address as a relying
//! party id (`rp_id is not an effective_domain of rp_origin`), so a bare IP
//! `SITE_URL` aborted startup entirely — nothing in this file could run.
//! Passkeys are now optional at runtime: an IP (or otherwise invalid relying
//! party) `SITE_URL` disables them with a startup warning, rather than
//! refusing to start.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::smtp_sink::SmtpSink;
use common::{TestApp, run_test, shared_app};
use tower::ServiceExt;
use trovato_kernel::gather::types::{
    GatherQuery, QueryDefinition, QueryDisplay, QuerySort, SortDirection,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::CreateItem;
use trovato_kernel::routes::feed::build_feed_router;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// The site's address for every test in this file — a bare IPv4 address, no
/// hostname, the case that used to abort startup.
const IP_SITE_URL: &str = "http://10.20.250.7";

const ITEM_TYPE: &str = "site_url_ip_test";

/// An app whose `SITE_URL` is a bare IP address, with mail routed to a
/// loopback sink so a reset mail's body can be inspected.
struct Fixture {
    sink: SmtpSink,
    app: TestApp,
}

static FIXTURE: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();

fn fixture() -> &'static Fixture {
    FIXTURE.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || {
            handle.block_on(async {
                let sink = SmtpSink::start().await;
                let port = sink.port;
                let app = TestApp::with_config(move |config| {
                    config.site_url = IP_SITE_URL.to_string();
                    config.runtime.site_url = IP_SITE_URL.to_string();
                    config.smtp_host = Some("127.0.0.1".to_string());
                    config.smtp_port = port;
                    config.smtp_encryption = "none".to_string();
                    config.smtp_from_email = "no-reply@b144.example.test".to_string();
                })
                .await;
                Fixture { sink, app }
            })
        })
        .join()
        .expect("fixture init thread panicked — the app must build under an IP SITE_URL")
    })
}

fn ip_for(tag: &str) -> String {
    common::test_ip_for(&format!("b144-{tag}"))
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// **The app builds with an IP `SITE_URL`.**
///
/// On `main` this panics inside `fixture()`: `AppState::new` propagates
/// `build_webauthn`'s "rp_id is not an effective_domain of rp_origin" error
/// with `?`, and the whole process refuses to start.
#[test]
fn the_app_builds_under_an_ip_site_url_with_passkeys_disabled() {
    run_test(async {
        let app = &fixture().app;
        assert!(
            app.state.webauthn().is_none(),
            "an IP SITE_URL cannot be a WebAuthn relying party, so passkeys must be disabled"
        );
    });
}

/// A hostname `SITE_URL` still configures WebAuthn exactly as before — the
/// shared app (a hostname or `localhost`) is unaffected by this change.
#[test]
fn a_hostname_site_url_still_configures_webauthn() {
    run_test(async {
        let app = shared_app().await;
        assert!(
            app.state.webauthn().is_some(),
            "a hostname SITE_URL must keep configuring WebAuthn as before"
        );
    });
}

/// Password login keeps working end to end under an IP `SITE_URL`.
#[test]
fn password_login_still_works_under_an_ip_site_url() {
    run_test(async {
        let app = &fixture().app;
        let name = common::username("b144-login");
        let cookies = app
            .create_and_login_user(&name, "b144-test-password-1", &format!("{name}@example.com"))
            .await;
        assert!(cookies.starts_with("id="), "password login still works");
    });
}

/// The login page explains that passkeys are unavailable, and does not ship
/// the passkey ceremony glue at all.
#[test]
fn the_login_page_explains_passkeys_are_unavailable() {
    run_test(async {
        let app = &fixture().app;
        let response = app
            .request(
                Request::get("/user/login")
                    .header("x-forwarded-for", ip_for("login-page"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Passkey sign-in isn't available on this site"),
            "the page must say plainly that passkeys are unavailable: {html}"
        );
        assert!(
            !html.contains("passkey-login.js"),
            "the ceremony glue must not ship when there is no relying party: {html}"
        );
        assert!(
            html.contains("action=\"/user/login\""),
            "password login must still be offered: {html}"
        );
    });
}

/// The passkey ceremony endpoints refuse cleanly rather than panicking on a
/// relying party that does not exist.
#[test]
fn passkey_routes_refuse_cleanly_when_disabled() {
    run_test(async {
        let app = &fixture().app;
        let response = app
            .request(
                Request::post("/user/webauthn/login/start")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", ip_for("login-start"))
                    .body(Body::from(
                        serde_json::json!({ "username": "nobody" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let html = body_text(response).await;
        assert!(
            html.contains("Passkeys are not available on this site"),
            "{html}"
        );
    });
}

/// A reset mail built under an IP `SITE_URL` carries that address.
#[test]
fn a_reset_mail_carries_the_ip_absolute_link() {
    run_test(async {
        let fixture = fixture();
        let app = &fixture.app;
        let name = common::username("b144-reset");
        let email = format!("{name}@b144.example.test");
        app.create_test_user(&name, "b144-test-password-2", &email)
            .await;
        let ip = ip_for(&format!("reset-{email}"));
        let limiter = app.state.rate_limiter();
        limiter.reset("recovery", &ip).await.ok();
        limiter
            .reset("recovery", &format!("password-reset:{ip}"))
            .await
            .ok();
        limiter.reset("reset_mail", &email.to_lowercase()).await.ok();

        let response = app
            .request(
                Request::post("/user/password-reset")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(serde_json::json!({ "email": email }).to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let mut found = Vec::new();
        for _ in 0..50 {
            found = fixture
                .sink
                .messages()
                .into_iter()
                .filter(|m| m.recipients.iter().any(|r| r == &email))
                .collect::<Vec<_>>();
            if !found.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(!found.is_empty(), "the reset mail must reach the sink");
        let unfolded = found[0].data.replace("=\r\n", "").replace("=\n", "");
        assert!(
            unfolded.contains(&format!("{IP_SITE_URL}/user/password-reset/")),
            "the reset link must be absolute with the IP SITE_URL: {unfolded}"
        );
    });
}

/// A feed built under an IP `SITE_URL` carries absolute links to that
/// address.
#[test]
fn a_feed_built_under_it_carries_the_ip_absolute_link() {
    run_test(async {
        let app = &fixture().app;

        let settings = serde_json::json!({ "fields": [] });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, 'B144 Feed Test', 'Fixture type for the B144 site-url test', true, 'Title', 'core', $2) \
             ON CONFLICT (type) DO NOTHING",
        )
        .bind(ITEM_TYPE)
        .bind(&settings)
        .execute(&app.db)
        .await
        .expect("seed item type");
        app.state
            .content_types()
            .replace(
                ITEM_TYPE,
                "B144 Feed Test",
                Some("Fixture type for the B144 site-url test"),
                settings,
            )
            .await
            .ok();

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/b144-{stamp}.xml");
        let query = GatherQuery {
            query_id: format!("b144_feed_test_{stamp}"),
            label: "B144 Feed Test Query".to_string(),
            description: None,
            definition: QueryDefinition {
                base_table: "item".to_string(),
                item_type: Some(ITEM_TYPE.to_string()),
                sorts: vec![QuerySort {
                    field: "created".to_string(),
                    direction: SortDirection::Desc,
                    nulls: None,
                }],
                ..Default::default()
            },
            display: QueryDisplay {
                items_per_page: 10,
                feed: Some(
                    serde_json::from_value(serde_json::json!({
                        "path": path,
                        "title": "B144 Feed Test",
                        "description": "Carries the IP SITE_URL",
                        "items": 20,
                    }))
                    .expect("feed config"),
                ),
                ..Default::default()
            },
            plugin: "core".to_string(),
            created: 0,
            changed: 0,
        };
        app.state
            .gather()
            .register_query(query.clone())
            .await
            .expect("register gather query");

        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let id = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: ITEM_TYPE.to_string(),
                    title: "A B144 post".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("b144 site-url test".to_string()),
                },
                &admin,
            )
            .await
            .expect("create item")
            .id;

        let redis_url = trovato_test_utils::env::redis_url();
        let session_layer = trovato_kernel::session::create_session_layer(
            &redis_url,
            tower_sessions::cookie::SameSite::Strict,
            trovato_kernel::session::CookieSecure::Always,
        )
        .await
        .expect("create session layer");
        let router = build_feed_router(std::slice::from_ref(&query))
            .with_state(app.state.clone())
            .layer(session_layer);

        let response = router
            .oneshot(Request::get(&path).body(Body::empty()).unwrap())
            .await
            .expect("serve feed");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(
            body.contains(&format!("<link>{IP_SITE_URL}/item/{id}</link>")),
            "the feed entry link must be absolute and use the IP SITE_URL, was:\n{body}"
        );

        app.state.items().delete(id, &admin).await.ok();
        sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(&query.query_id)
            .execute(&app.db)
            .await
            .ok();
    });
}
