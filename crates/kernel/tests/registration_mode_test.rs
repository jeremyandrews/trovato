#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The registration mode the admin form controls is the one the route honours.
//!
//! The register route gated on the boolean `allow_user_registration`, default
//! false (`routes/auth.rs:462`). The admin site-config form offered a three-mode
//! `user_registration` selector and saved it (`routes/admin_config.rs:339`), but
//! the only reader of that key was the same form re-rendering itself — so
//! choosing "open" changed nothing, and a config import of the boolean was the
//! only way to open registration.
//!
//! Precedence and fallbacks are unit-tested in `models::site_config`. What is
//! pinned here is the route: the setting decides whether `/user/register` serves.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::{RegistrationMode, SiteConfig};

/// Registration settings are site-wide, so this file serializes its own tests.
static REGISTRATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn clear_settings(app: &TestApp) {
    for key in ["user_registration", "allow_user_registration"] {
        SiteConfig::delete(&app.db, key)
            .await
            .expect("clear registration setting");
    }
}

async fn register_page_status(app: &TestApp) -> StatusCode {
    app.request(Request::get("/user/register").body(Body::empty()).unwrap())
        .await
        .status()
}

/// Choosing "open" opens registration. This is the defect: it did not.
#[test]
fn the_open_mode_opens_the_register_route() {
    run_test(async {
        let app = shared_app().await;
        let _guard = REGISTRATION.lock().await;
        clear_settings(app).await;

        RegistrationMode::Open
            .save(&app.db)
            .await
            .expect("save open mode");

        assert_eq!(
            register_page_status(app).await,
            StatusCode::OK,
            "the register form must be served when the mode is open"
        );

        clear_settings(app).await;
    });
}

/// And admin-only closes it.
#[test]
fn the_admin_only_mode_closes_the_register_route() {
    run_test(async {
        let app = shared_app().await;
        let _guard = REGISTRATION.lock().await;
        clear_settings(app).await;

        RegistrationMode::AdminOnly
            .save(&app.db)
            .await
            .expect("save admin_only mode");

        assert_eq!(
            register_page_status(app).await,
            StatusCode::NOT_FOUND,
            "the register form must not be served when only admins create accounts"
        );

        clear_settings(app).await;
    });
}

/// Saving the mode clears the boolean it supersedes, so one setting is left and
/// the two cannot disagree. This is the migration the issue asked for.
#[test]
fn saving_the_mode_removes_the_superseded_boolean() {
    run_test(async {
        let app = shared_app().await;
        let _guard = REGISTRATION.lock().await;
        clear_settings(app).await;

        // A site as it was: opened through the boolean, nothing else set.
        SiteConfig::set(&app.db, "allow_user_registration", serde_json::json!(true))
            .await
            .expect("set legacy boolean");

        // Before any save, the legacy value still decides — an existing site keeps
        // working across the upgrade.
        assert_eq!(
            register_page_status(app).await,
            StatusCode::OK,
            "the legacy boolean must keep an already-open site open"
        );

        RegistrationMode::AdminOnly
            .save(&app.db)
            .await
            .expect("save mode");

        assert_eq!(
            SiteConfig::get(&app.db, "allow_user_registration")
                .await
                .expect("read legacy key"),
            None,
            "the superseded boolean must be gone after a save"
        );
        assert_eq!(
            register_page_status(app).await,
            StatusCode::NOT_FOUND,
            "and the mode now decides"
        );

        clear_settings(app).await;
    });
}

/// A stored `closed` — the third mode the form used to offer — still closes the
/// public route, so no site's behaviour changes under the reduction to two modes.
#[test]
fn a_stored_closed_mode_still_closes_registration() {
    run_test(async {
        let app = shared_app().await;
        let _guard = REGISTRATION.lock().await;
        clear_settings(app).await;

        SiteConfig::set(&app.db, "user_registration", serde_json::json!("closed"))
            .await
            .expect("store closed");

        assert_eq!(register_page_status(app).await, StatusCode::NOT_FOUND);

        clear_settings(app).await;
    });
}

/// With nothing configured, registration is closed — the boolean's old default.
#[test]
fn registration_is_closed_by_default() {
    run_test(async {
        let app = shared_app().await;
        let _guard = REGISTRATION.lock().await;
        clear_settings(app).await;

        assert_eq!(register_page_status(app).await, StatusCode::NOT_FOUND);
    });
}

/// The POST is gated too, not just the form. A closed route that still accepts
/// submissions is not closed.
#[test]
fn the_registration_submission_is_gated_as_well() {
    run_test(async {
        let app = shared_app().await;
        let _guard = REGISTRATION.lock().await;
        clear_settings(app).await;

        RegistrationMode::AdminOnly
            .save(&app.db)
            .await
            .expect("save mode");

        let response = app
            .request(
                Request::post("/user/register")
                    .header("content-type", "application/x-www-form-urlencoded")
                    // A complete form body, `_token` included: the point is that
                    // the gate refuses it, not that the body is malformed. Axum
                    // extracts the form before the handler runs, so a missing
                    // field would 422 and prove nothing.
                    .body(Body::from(
                        "username=sneaky&mail=sneaky%40example.com&password=whatever123\
                         &confirm_password=whatever123&_token=irrelevant",
                    ))
                    .unwrap(),
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a closed route must refuse the submission, not only hide the form"
        );

        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE name = 'sneaky')")
                .fetch_one(&app.db)
                .await
                .expect("check user");
        assert!(
            !exists,
            "no account may be created while registration is closed"
        );

        clear_settings(app).await;
    });
}
