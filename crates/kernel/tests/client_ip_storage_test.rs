#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `CLIENT_IP_STORAGE` (B121): what of a client address outlives the request.
//!
//! Each test drives a whole visit through the router (a run of failed sign
//! ins that ends in a lockout, a sign in, a page view that registers the
//! session, a comment) from one address of its own, and then searches **every**
//! text, JSON and address column of every table in the database for that
//! address. Under `none` nothing turns up; under `truncated` only the /24 does.
//! The searches are by value, not by a list of known columns, so a column added
//! later that stores the address is caught too.
//!
//! The session index (Redis) is checked as well: it lives as long as the
//! session does, which is long enough to count as stored.
//!
//! Rate limiting is the one consumer that keeps the whole address, under every
//! setting, in Redis only; the last test shows it still tells two clients apart
//! under `none`.
//!
//! Requires Postgres and Redis; runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test};
use sqlx::PgPool;
use trovato_kernel::client_addr::ClientIpStorage;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::system_log::{self, SystemLogLayer};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const ITEM_TYPE: &str = "client_ip_storage_test";
const TYPE_SEED_LOCK: i64 = 0x_B121_0000_0001;
const PASSWORD: &str = "correct-horse-battery-staple";

/// A random address in 198.18.0.0/15 (RFC 2544, never routed), so no other
/// test's rows contain its /24 by accident. The host part is never 0, so the
/// full address and its truncated form never coincide.
fn fresh_ip() -> (String, String) {
    let n = Uuid::now_v7().as_u128();
    let third = (n % 250) as u8;
    let fourth = ((n >> 8) % 250) as u8 + 2;
    (
        format!("198.18.{third}.{fourth}"),
        format!("198.18.{third}."),
    )
}

/// The site log writer, once per binary: the lockout warning is one of the
/// places an address used to reach the database.
fn install_system_log(db: &PgPool) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::{Layer, filter::LevelFilter};
        let (layer, receiver) = SystemLogLayer::new();
        let _ = tracing_subscriber::registry()
            .with(layer.with_filter(LevelFilter::INFO))
            .try_init();
        system_log::spawn_writer(db.clone(), receiver);
    });
}

/// Every `table.column` in the public schema whose text contains `needle`.
///
/// Every column that can hold text: `text`, `varchar`, `char`, `json`,
/// `jsonb`, `inet`, `cidr`. Returns the scanned count too, so the report can
/// say how many it looked at.
async fn columns_containing(db: &PgPool, needle: &str) -> (usize, Vec<String>) {
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.table_name::text, c.column_name::text \
         FROM information_schema.columns c \
         JOIN information_schema.tables t \
           ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
         WHERE c.table_schema = 'public' AND t.table_type = 'BASE TABLE' \
           AND c.data_type IN ('text', 'character varying', 'character', 'json', \
                               'jsonb', 'inet', 'cidr') \
         ORDER BY 1, 2",
    )
    .fetch_all(db)
    .await
    .unwrap();

    let pattern = format!("%{needle}%");
    let mut hits = Vec::new();
    for (table, column) in &columns {
        // Table and column names come from information_schema, quoted; the
        // searched value is a bind parameter.
        let sql =
            format!("SELECT EXISTS (SELECT 1 FROM \"{table}\" WHERE \"{column}\"::text LIKE $1)");
        let found: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(&pattern)
            .fetch_one(db)
            .await
            .unwrap_or(false);
        if found {
            hits.push(format!("{table}.{column}"));
        }
    }
    (columns.len(), hits)
}

async fn ensure_item_type(app: &TestApp) {
    app.ensure_plugin_enabled("trovato_comments").await;
    let mut tx = app.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Client IP Storage Test', 'Fixture type', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    app.state
        .content_types()
        .replace(
            ITEM_TYPE,
            "Client IP Storage Test",
            Some("Fixture type"),
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();
}

async fn create_item(app: &TestApp) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: format!("Client IP {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("client ip storage test".to_string()),
            },
            &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .unwrap()
        .id
}

async fn grant(app: &TestApp, user_id: Uuid, permissions: &[&str]) {
    let role_id = common::create_test_role(app, &format!("b121_{}", Uuid::now_v7().simple())).await;
    for permission in permissions {
        sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(role_id)
            .bind(permission)
            .execute(&app.db)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(user_id)
        .bind(role_id)
        .execute(&app.db)
        .await
        .unwrap();
    app.state.permissions().invalidate_all();
}

async fn login_json(
    app: &TestApp,
    ip: &str,
    name: &str,
    password: &str,
) -> axum::response::Response {
    app.request(
        Request::post("/user/login/json")
            .header("content-type", "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({ "username": name, "password": password }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

/// Each test builds an app of its own; a small pool each keeps five of them
/// inside the server's connection limit.
fn storage(c: &mut trovato_kernel::config::Config, value: ClientIpStorage) {
    c.runtime.client_ip_storage = value;
    c.database_max_connections = 5;
}

struct Visit {
    user_id: Uuid,
}

/// Five failed sign ins (the fifth locks the address out and logs it), a sign
/// in, a page view that registers the session, and a comment, all from `ip`.
async fn visit(app: &TestApp, ip: &str) -> Visit {
    install_system_log(&app.db);
    ensure_item_type(app).await;
    let item_id = create_item(app).await;

    let name = common::username("b121ip");
    app.create_test_user(&name, PASSWORD, &format!("{name}@test.local"))
        .await;
    let user_id = common::user_id_of(app, &name).await;
    grant(
        app,
        user_id,
        &["post comments", "access content", "skip comment approval"],
    )
    .await;

    // The failed sign ins.
    for _ in 0..5 {
        let r = login_json(app, ip, &name, "wrong-password").await;
        assert_ne!(r.status(), StatusCode::OK);
    }
    // The lockout is for this address; lift it and sign in properly.
    app.state.lockout().clear_all(&name).await.unwrap();
    let r = login_json(app, ip, &name, PASSWORD).await;
    assert_eq!(r.status(), StatusCode::OK, "sign in");
    let cookies = extract_cookies(&r);

    // The first page view after sign in registers the session (an audit row
    // and the session index entry), and carries the CSRF token.
    let page = app
        .request_with_cookies(
            Request::get("/")
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;
    let fresh = extract_cookies(&page);
    let cookies = if fresh.is_empty() { cookies } else { fresh };
    let html = String::from_utf8(
        to_bytes(page.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let token = html[start..].split('"').next().unwrap().to_string();

    let posted = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("x-forwarded-for", ip)
                .header("X-CSRF-Token", &token)
                .body(Body::from(
                    serde_json::json!({ "body": "A remark" }).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(posted.status(), StatusCode::OK, "the comment posts");

    Visit { user_id }
}

/// The `ip_address` of each security audit row about `user_id`, waiting for the
/// session registration row (written after the response).
async fn audit_ips(db: &PgPool, user_id: Uuid) -> Vec<(String, Option<String>)> {
    for _ in 0..50 {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT kind, ip_address FROM security_audit_log WHERE user_id = $1 ORDER BY created",
        )
        .bind(user_id)
        .fetch_all(db)
        .await
        .unwrap();
        if rows.iter().any(|(k, _)| k == "session.created") {
            return rows;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("no session.created audit row for {user_id}");
}

/// The lockout warning's `client_ip`, once the site log writer has caught up.
async fn lockout_log_ip(db: &PgPool, user_id: Uuid) -> String {
    let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .unwrap();
    for _ in 0..50 {
        let row: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT context FROM system_log \
             WHERE message LIKE 'sign-in locked for one address%' AND context::text LIKE $1 \
             ORDER BY id DESC LIMIT 1",
        )
        .bind(format!("%{name}%"))
        .fetch_optional(db)
        .await
        .unwrap();
        if let Some(context) = row {
            return context["client_ip"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the lockout warning never reached system_log");
}

#[test]
fn none_leaves_no_address_in_any_table_after_sign_ins_and_a_comment() {
    run_test(async {
        let app = TestApp::with_config(|c| storage(c, ClientIpStorage::None)).await;
        let (ip, network) = fresh_ip();
        let visit = visit(&app, &ip).await;

        // The places that used to hold it are there, and hold nothing.
        let rows = audit_ips(&app.db, visit.user_id).await;
        assert!(
            rows.iter().all(|(_, ip)| ip.is_none()),
            "every audit row's ip_address is NULL: {rows:?}"
        );
        assert_eq!(lockout_log_ip(&app.db, visit.user_id).await, "");
        let sessions = app
            .state
            .session_registry()
            .list(visit.user_id)
            .await
            .unwrap();
        assert!(!sessions.is_empty());
        assert!(sessions.iter().all(|s| s.ip.is_empty()), "{sessions:?}");

        // And nothing anywhere holds even the network.
        let (scanned, hits) = columns_containing(&app.db, &network).await;
        println!("CLIENT_IP_STORAGE=none: scanned {scanned} columns for {network}*: {hits:?}");
        println!("  security_audit_log rows: {rows:?}");
        assert!(hits.is_empty(), "{network}* found in {hits:?}");
    });
}

#[test]
fn truncated_stores_the_network_and_never_the_address() {
    run_test(async {
        let app = TestApp::with_config(|c| storage(c, ClientIpStorage::Truncated)).await;
        let (ip, network) = fresh_ip();
        let masked = format!("{network}0");
        let visit = visit(&app, &ip).await;

        let rows = audit_ips(&app.db, visit.user_id).await;
        assert!(
            rows.iter()
                .filter(|(k, _)| k == "session.created")
                .all(|(_, stored)| stored.as_deref() == Some(masked.as_str())),
            "the session row stores {masked}: {rows:?}"
        );
        assert_eq!(lockout_log_ip(&app.db, visit.user_id).await, masked);
        let sessions = app
            .state
            .session_registry()
            .list(visit.user_id)
            .await
            .unwrap();
        assert!(sessions.iter().all(|s| s.ip == masked), "{sessions:?}");

        let (scanned, hits) = columns_containing(&app.db, &ip).await;
        println!("CLIENT_IP_STORAGE=truncated: scanned {scanned} columns for {ip}: {hits:?}");
        println!("  security_audit_log rows: {rows:?}");
        assert!(hits.is_empty(), "the full {ip} found in {hits:?}");
        let (_, masked_hits) = columns_containing(&app.db, &masked).await;
        assert!(
            masked_hits.contains(&"security_audit_log.ip_address".to_string()),
            "{masked_hits:?}"
        );
    });
}

/// `full`, the default, is what it always was: the address, whole.
#[test]
fn full_stores_the_address() {
    run_test(async {
        let app = TestApp::with_config(|c| storage(c, ClientIpStorage::Full)).await;
        assert_eq!(app.state.runtime().client_ip_storage, ClientIpStorage::Full);
        let (ip, _) = fresh_ip();
        let visit = visit(&app, &ip).await;
        let rows = audit_ips(&app.db, visit.user_id).await;
        println!("CLIENT_IP_STORAGE=full: security_audit_log rows: {rows:?}");
        assert!(
            rows.iter()
                .filter(|(k, _)| k == "session.created")
                .all(|(_, stored)| stored.as_deref() == Some(ip.as_str())),
            "{rows:?}"
        );
        assert_eq!(lockout_log_ip(&app.db, visit.user_id).await, ip);
    });
}

/// Nothing stored does not mean nothing counted: the limiter keys on the whole
/// resolved address, in Redis, so one client spending its budget leaves the
/// next one's alone.
#[test]
fn rate_limiting_still_separates_two_clients_under_none() {
    run_test(async {
        let app = TestApp::with_rate_limits(|c| storage(c, ClientIpStorage::None)).await;
        let (api_limit, _) = app.state.rate_limiter().config().bucket("api").unwrap();
        let (a, _) = fresh_ip();
        let (b, _) = fresh_ip();
        assert_ne!(a, b);

        let get = |ip: String| {
            Request::get("/api/v1/items")
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap()
        };
        let mut refused_a = 0;
        for _ in 0..=api_limit {
            if app.request(get(a.clone())).await.status() == StatusCode::TOO_MANY_REQUESTS {
                refused_a += 1;
            }
        }
        let b_status = app.request(get(b.clone())).await.status();
        println!(
            "CLIENT_IP_STORAGE=none: {a} sent {} api requests, {refused_a} refused; {b} then got {b_status}",
            api_limit + 1
        );
        assert!(
            refused_a >= 1,
            "client A exceeded the api limit of {api_limit}"
        );
        assert_ne!(
            b_status,
            StatusCode::TOO_MANY_REQUESTS,
            "client B has its own budget"
        );
    });
}

/// The spoofing hole, through the whole stack: with no trusted proxy (the
/// default), a client that sends a different `X-Forwarded-For` on every request
/// is still one client, the socket peer, and runs out of budget like one.
#[test]
fn a_spoofed_forwarded_for_from_an_untrusted_peer_does_not_buy_a_new_budget() {
    run_test(async {
        let app = TestApp::with_rate_limits(|c| {
            storage(c, ClientIpStorage::Full);
            c.trusted_proxies = Default::default();
        })
        .await;
        let (api_limit, _) = app.state.rate_limiter().config().bucket("api").unwrap();
        // The test transport's peer is 127.0.0.1; start its budget clean.
        app.state
            .rate_limiter()
            .reset("api", "127.0.0.1")
            .await
            .ok();

        let mut refused = 0;
        for _ in 0..=api_limit {
            let (spoofed, _) = fresh_ip();
            let status = app
                .request(
                    Request::get("/api/v1/items")
                        .header("x-forwarded-for", spoofed)
                        .header("x-real-ip", "203.0.113.50")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .status();
            if status == StatusCode::TOO_MANY_REQUESTS {
                refused += 1;
            }
        }
        app.state
            .rate_limiter()
            .reset("api", "127.0.0.1")
            .await
            .ok();
        println!(
            "untrusted peer, {} requests each with a new X-Forwarded-For: {refused} refused",
            api_limit + 1
        );
        assert!(
            refused >= 1,
            "{} requests from one peer must exceed the api limit of {api_limit}, whatever header they carry",
            api_limit + 1
        );
    });
}

/// The hole this change closes. The account deletion step-up keyed its
/// password-guess limit on the leftmost `X-Forwarded-For` entry as sent, so a
/// signed-in session could guess the password without limit by sending a new
/// value each time. With no trusted proxy, every guess is the socket peer's.
#[test]
fn the_deletion_step_up_limit_cannot_be_dodged_with_forwarded_for() {
    run_test(async {
        let app = TestApp::with_config(|c| {
            storage(c, ClientIpStorage::Full);
            c.trusted_proxies = Default::default();
        })
        .await;
        let limiter = app.state.rate_limiter();
        let (limit, _) = limiter.config().bucket("password").unwrap();
        limiter.reset("login", "127.0.0.1").await.ok();
        limiter.reset("password", "127.0.0.1").await.ok();

        let name = common::username("b121del");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@test.local"))
            .await;
        let user_id = common::user_id_of(&app, &name).await;

        let mut cookies = cookies;
        let mut statuses = Vec::new();
        for _ in 0..=limit {
            // A form token is good for one post; fetch a fresh one each time.
            let page = app
                .request_with_cookies(
                    Request::get("/user/delete").body(Body::empty()).unwrap(),
                    &cookies,
                )
                .await;
            let fresh = extract_cookies(&page);
            if !fresh.is_empty() {
                cookies = fresh;
            }
            let html = String::from_utf8(
                to_bytes(page.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            let fields = common::form_fields(&html, "user_delete_confirm");

            let (spoofed, _) = fresh_ip();
            let response = app
                .request_with_cookies(
                    Request::post("/user/delete/reauth")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .header("x-forwarded-for", &spoofed)
                        .body(Body::from(format!("{fields}&password=wrong")))
                        .unwrap(),
                    &cookies,
                )
                .await;
            statuses.push(response.status().as_u16());
        }
        limiter.reset("password", "127.0.0.1").await.ok();
        println!(
            "deletion step-up, {} wrong guesses each with a new X-Forwarded-For: {statuses:?}",
            limit + 1
        );
        assert_eq!(
            statuses.last(),
            Some(&429),
            "guess {} is over the password limit of {limit}: {statuses:?}",
            limit + 1
        );

        // And the audit log holds the peer, not anything the client sent.
        let ips: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT DISTINCT ip_address FROM security_audit_log \
             WHERE user_id = $1 AND kind = 'auth.login_failed'",
        )
        .bind(user_id)
        .fetch_all(&app.db)
        .await
        .unwrap();
        println!("  auth.login_failed ip_address values: {ips:?}");
        assert_eq!(ips, vec![Some("127.0.0.1".to_string())]);
    });
}
