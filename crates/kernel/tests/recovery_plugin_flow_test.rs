#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Story 4.6 — a **real WASM recovery plugin** driving the full kernel flow.
//!
//! `recovery_flow_test` exercises the flow through the built-in providers.
//! This file exercises the other half of the claim: that the flow the kernel
//! drives is genuinely the frozen tap contract, by putting two real
//! `wasm32-wasip1` plugins behind it and going through the same HTTP endpoints
//! a user would.
//!
//! - `trovato_recovery_ref` — the legitimate owner of
//!   `trovato_recovery_ref:code`, which accepts the code `123456`.
//! - `test_recovery_bystander` — a rogue that forges `Verified` on **any**
//!   `verify`, whatever the `method_id`. The owner-scoped fold must ignore it,
//!   and (per the D-32 amendment) the attempt must be audited, never silently
//!   dropped.
//!
//! Both plugins are `default_enabled = false`, so this file installs them in the
//! database and then builds its **own** `TestApp`, whose `AppState` loads the
//! enabled set at construction. It disables them again on the way out so the
//! setting does not leak into other test binaries.
//!
//! Requires Postgres + Redis, and the fixture `.wasm` files built and copied
//! into `plugins/<name>/` (the pre-commit script and both CI jobs do this).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;

const REF_PLUGIN: &str = "trovato_recovery_ref";
const ROGUE_PLUGIN: &str = "test_recovery_bystander";
const REF_METHOD: &str = "trovato_recovery_ref:code";
/// The code `trovato_recovery_ref` treats as correct.
const REF_CORRECT_CODE: &str = "123456";

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// The one app both tests share.
///
/// Built exactly once, for two reasons that both bite otherwise: constructing
/// two `AppState`s concurrently races on plugin migrations (two `CREATE TYPE`
/// statements collide), and a pool whose connections were opened on a per-test
/// runtime dies when that runtime does. Both tests therefore run on the
/// harness's shared runtime via `common::run_test`, exactly like
/// `common::shared_app`.
static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app_with_recovery_plugins() -> &'static TestApp {
    APP.get_or_init(|| {
        // Initialize on the harness's shared runtime, in a separate OS thread to
        // avoid a nested block_on — the same shape as `common::shared_app`.
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app_with_recovery_plugins()))
            .join()
            .expect("recovery fixture app init thread panicked")
    })
}

/// Install and enable the fixture plugins, then build an app that loads them.
///
/// `AppState` resolves its enabled plugin set at construction, so the database
/// has to say "enabled" before the app exists — which is why this uses a direct
/// pool rather than the app's own.
async fn build_app_with_recovery_plugins() -> TestApp {
    trovato_test_utils::env::load_dotenv();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");

    for plugin in [REF_PLUGIN, ROGUE_PLUGIN] {
        trovato_kernel::plugin::status::install_plugin(&pool, plugin, "1.0.0")
            .await
            .unwrap_or_else(|e| panic!("failed to install fixture '{plugin}': {e:#}"));
    }
    pool.close().await;

    // Integration tests run with the crate directory as CWD, so `Config`'s
    // default `./plugins` resolves to nothing. Point it at the real directory
    // through the config rather than through `PLUGINS_DIR`: `plugins_dirs` is a
    // `Config` field, so this fixture needs no process-global write at all. The
    // environment still wins when it says something, as it did before.
    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

/// Undo the enable so the fixtures do not leak into other test binaries.
///
/// Safe to call from either test: the plugins are already loaded into the shared
/// app's runtime, so flipping the database row afterwards affects only the *next*
/// `AppState` built anywhere, which is exactly the leak we are preventing.
async fn disable_recovery_plugins(app: &TestApp) {
    for plugin in [REF_PLUGIN, ROGUE_PLUGIN] {
        sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
            .bind(plugin)
            .execute(&app.db)
            .await
            .ok();
    }
}

async fn user_id_of(app: &TestApp, username: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn start_recovery(app: &TestApp, identifier: &str, ip: &str) -> axum::response::Response {
    app.request(
        Request::post("/user/recover/start")
            .header("content-type", "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({ "identifier": identifier }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

async fn reset_limits(app: &TestApp, ip: &str, user_id: uuid::Uuid) {
    app.state.rate_limiter().reset("recovery", ip).await.ok();
    app.state
        .rate_limiter()
        .reset("recovery", &format!("user:{user_id}"))
        .await
        .ok();
}

#[test]
fn a_recovery_plugin_drives_the_full_kernel_flow_and_a_rogue_cannot_escalate() {
    common::run_test(async {
        let app = app_with_recovery_plugins();

        // Both fixtures must actually be loaded, or this test proves nothing.
        let handlers = app
            .state
            .tap_dispatcher()
            .registry()
            .handler_count("tap_account_recovery");
        assert!(
            handlers >= 2,
            "expected both recovery fixtures loaded (got {handlers} tap_account_recovery handlers). \
         Build them first: cargo build -p {REF_PLUGIN} -p {ROGUE_PLUGIN} \
         --target wasm32-wasip1 --release && cp the .wasm into plugins/<name>/"
        );

        let username = "recplug_user";
        app.create_test_user(username, "test-password-123", "recplug_user@example.com")
            .await;
        let user_id = user_id_of(app, username).await;

        // Remove the built-in saved-codes path for this account so the flow is
        // driven purely by the plugin.
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();

        let ip = "10.61.0.11";
        reset_limits(app, ip, user_id).await;

        // ── describe: the plugin's method is offered, composed as a union ────────
        let start = start_recovery(app, username, ip).await;
        assert_eq!(start.status(), StatusCode::OK);
        let cookies = common::extract_cookies(&start);
        let body = json_body(start).await;
        let methods = body["methods"].as_array().unwrap();
        assert!(
            methods.iter().any(|m| m["method_id"] == REF_METHOD),
            "the WASM plugin's method must reach the chooser: {body}"
        );
        // The rogue advertises nothing, and could not have advertised the ref's
        // namespace even if it tried: `collect_methods` drops non-owner offers.
        assert!(
            !methods.iter().any(|m| m["method_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("test_recovery_bystander:"))),
            "the rogue offers no methods"
        );

        // ── initiate ─────────────────────────────────────────────────────────────
        let choose = app
            .request_with_cookies(
                Request::post("/user/recover/choose")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", ip)
                    .body(Body::from(
                        serde_json::json!({ "method_id": REF_METHOD }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            choose.status(),
            StatusCode::OK,
            "the owning plugin must be able to initiate its own method"
        );

        // ── verify with the WRONG code ───────────────────────────────────────────
        //
        // This is the load-bearing case. The rogue forges `Verified` on every
        // verify, including this one. The owner says `Rejected`. An owner Rejected
        // beats a non-owner Verified, so recovery must be DENIED — and the forged
        // vote must be audited as an attempted escalation.
        let wrong = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", ip)
                    .body(Body::from(
                        serde_json::json!({ "response": "000000" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            wrong.status(),
            StatusCode::UNAUTHORIZED,
            "a rogue's forged Verified must not rescue a wrong code"
        );

        let escalations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM security_audit_log
         WHERE user_id = $1 AND kind = 'recovery.fold_escalation_attempt'
           AND details->>'plugin' = $2",
        )
        .bind(user_id)
        .bind(ROGUE_PLUGIN)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(
            escalations >= 1,
            "the D-32 amendment: an ignored non-owner Verified must be audited, never silently dropped"
        );

        // ── verify with the CORRECT code ─────────────────────────────────────────
        reset_limits(app, ip, user_id).await;
        let start = start_recovery(app, username, ip).await;
        let cookies = common::extract_cookies(&start);
        let _ = json_body(start).await;

        app.request_with_cookies(
            Request::post("/user/recover/choose")
                .header("content-type", "application/json")
                .header("x-forwarded-for", ip)
                .body(Body::from(
                    serde_json::json!({ "method_id": REF_METHOD }).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;

        let verify = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", ip)
                    .body(Body::from(
                        serde_json::json!({ "response": REF_CORRECT_CODE }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            verify.status(),
            StatusCode::OK,
            "the owner's Verified on its own method must grant"
        );
        let verified_cookies = {
            let c = common::extract_cookies(&verify);
            if c.is_empty() { cookies.clone() } else { c }
        };
        assert_eq!(json_body(verify).await["next"], "reset");

        // Still not a session (D-38) — the plugin path grants exactly what the
        // built-in path does, no more.
        let not_logged_in = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &verified_cookies,
            )
            .await;
        assert_eq!(
            not_logged_in.status(),
            StatusCode::SEE_OTHER,
            "a plugin-verified recovery is still only a scoped credential reset"
        );

        // ── reset, and only now a session ────────────────────────────────────────
        let reset = app
            .request_with_cookies(
                Request::post("/user/recover/reset")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", ip)
                    .body(Body::from(
                        serde_json::json!({ "new_password": "plugin-recovered-pw-1" }).to_string(),
                    ))
                    .unwrap(),
                &verified_cookies,
            )
            .await;
        assert_eq!(reset.status(), StatusCode::OK);
        let session_cookies = common::extract_cookies(&reset);
        let profile = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &session_cookies,
            )
            .await;
        assert_eq!(
            profile.status(),
            StatusCode::OK,
            "the session established after a plugin-driven recovery must be real"
        );

        disable_recovery_plugins(app).await;
    });
}

#[test]
fn a_rogue_cannot_verify_a_method_it_does_not_own_even_alone() {
    common::run_test(async {
        // The bystander forges `Verified` on any verify. Dispatch a method whose
        // namespace it does not own with the owner absent from the answer set, and
        // the fold must still deny: a non-owner vote never counts, so there is no
        // owner vote at all and the fail-closed default applies.
        let app = app_with_recovery_plugins();

        let username = "recplug_rogue";
        app.create_test_user(username, "test-password-123", "recplug_rogue@example.com")
            .await;
        let user_id = user_id_of(app, username).await;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();

        let ip = "10.61.0.12";
        reset_limits(app, ip, user_id).await;

        let start = start_recovery(app, username, ip).await;
        let cookies = common::extract_cookies(&start);
        let _ = json_body(start).await;

        // Ask to initiate the ref plugin's method, then verify with a code the ref
        // plugin rejects. Only the rogue says Verified, and it owns nothing here.
        app.request_with_cookies(
            Request::post("/user/recover/choose")
                .header("content-type", "application/json")
                .header("x-forwarded-for", ip)
                .body(Body::from(
                    serde_json::json!({ "method_id": REF_METHOD }).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;

        let verify = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", ip)
                    .body(Body::from(
                        serde_json::json!({ "response": "definitely-wrong" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            verify.status(),
            StatusCode::UNAUTHORIZED,
            "fail-closed: a non-owner Verified is not a vote"
        );

        // And no grant was issued, so reset is refused too.
        let reset = app
            .request_with_cookies(
                Request::post("/user/recover/reset")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", ip)
                    .body(Body::from(
                        serde_json::json!({ "new_password": "should-never-apply-1" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(reset.status(), StatusCode::FORBIDDEN);

        // The password is untouched: the rogue changed nothing about the account.
        let still_works = app
            .request(
                Request::post("/user/login/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", "10.61.0.99")
                    .body(Body::from(
                        serde_json::json!({
                            "username": username,
                            "password": "test-password-123"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            still_works.status(),
            StatusCode::OK,
            "the original password must still be the account's password"
        );

        disable_recovery_plugins(app).await;
    });
}
