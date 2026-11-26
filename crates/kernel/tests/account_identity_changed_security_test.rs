#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `user.name_changed` and `user.mail_changed`: an account's username and
//! e-mail address are as much a part of its identity as its password, but
//! changing either wrote no security event at all before this — only the
//! content audit log's generic `update`/`user` row's `changed` list (and that
//! only when the optional `trovato_audit_log` plugin is enabled).
//!
//! Both events are emitted by `UserService::audit_updated`, the one place
//! every update path shares (self-service profile update, its e-mail
//! confirmation step, and the admin "Edit user" form), so one test per path
//! is really a test that each path reaches the same shared code.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::email_verification::{EmailVerificationToken, PURPOSE_EMAIL_CHANGE};
use uuid::Uuid;

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn extract_csrf_token(html: &str) -> Option<String> {
    let pattern = r#"name="_token" value=""#;
    let start = html.find(pattern)? + pattern.len();
    let end = html[start..].find('"')?;
    Some(html[start..start + end].to_string())
}

fn extract_form_build_id(html: &str) -> Option<String> {
    let pattern = r#"name="_form_build_id" value=""#;
    let start = html.find(pattern)? + pattern.len();
    let end = html[start..].find('"')?;
    Some(html[start..start + end].to_string())
}

async fn user_id_of(app: &TestApp, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// `(actor_id, details)` of every row of `kind` for `user_id`, oldest first.
async fn events(
    app: &TestApp,
    kind: &str,
    user_id: Uuid,
) -> Vec<(Option<Uuid>, serde_json::Value)> {
    sqlx::query_as(
        "SELECT actor_id, details FROM security_audit_log \
         WHERE kind = $1 AND user_id = $2 ORDER BY created, id",
    )
    .bind(kind)
    .bind(user_id)
    .fetch_all(&app.db)
    .await
    .unwrap()
}

async fn event_count(app: &TestApp, kind: &str, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM security_audit_log WHERE kind = $1 AND user_id = $2")
        .bind(kind)
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// A self-service rename, through `/user/profile`, writes one
/// `user.name_changed` event naming the old and new names, with no actor:
/// the account holder changed their own.
#[test]
fn self_service_rename_writes_one_user_name_changed_event() {
    run_test(async {
        let app = shared_app().await;
        let old_name = common::username("identchg-self");
        let password = "test-password-123";
        let mail = format!("{old_name}@example.com");
        let cookies = app.create_and_login_user(&old_name, password, &mail).await;
        let user_id = user_id_of(app, &old_name).await;

        let response = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        let html = body_text(response).await;
        let token = extract_csrf_token(&html).expect("CSRF token");
        let build_id = extract_form_build_id(&html).unwrap_or_default();

        let new_name = format!("{old_name}_renamed");
        let body = format!(
            "_token={token}&_form_build_id={build_id}&name={new_name}&mail={mail}\
             &timezone=&current_password={password}"
        );
        let response = app
            .request_with_cookies(
                Request::post("/user/profile")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Profile updated successfully."),
            "the rename must succeed: {html}"
        );

        let rows = events(app, "user.name_changed", user_id).await;
        assert_eq!(rows.len(), 1, "exactly one event: {rows:?}");
        let (actor_id, details) = &rows[0];
        assert_eq!(*actor_id, None, "the account holder changed their own name");
        assert_eq!(details["from"], old_name);
        assert_eq!(details["to"], new_name);
    });
}

/// A rename refused for a taken username writes no `user.name_changed` event.
#[test]
fn a_refused_rename_writes_no_user_name_changed_event() {
    run_test(async {
        let app = shared_app().await;
        let taken_name = common::username("identchg-taken");
        app.create_test_user(
            &taken_name,
            "test-password-123",
            &format!("{taken_name}@example.com"),
        )
        .await;

        let name = common::username("identchg-refused");
        let password = "test-password-123";
        let mail = format!("{name}@example.com");
        let cookies = app.create_and_login_user(&name, password, &mail).await;
        let user_id = user_id_of(app, &name).await;

        let response = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        let html = body_text(response).await;
        let token = extract_csrf_token(&html).expect("CSRF token");
        let build_id = extract_form_build_id(&html).unwrap_or_default();

        let body = format!(
            "_token={token}&_form_build_id={build_id}&name={taken_name}&mail={mail}\
             &timezone=&current_password={password}"
        );
        let response = app
            .request_with_cookies(
                Request::post("/user/profile")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            !html.contains("Profile updated successfully."),
            "a taken name must be refused"
        );

        assert_eq!(event_count(app, "user.name_changed", user_id).await, 0);
    });
}

/// The self-service e-mail change's confirmation step — `/user/verify-email/
/// {token}`, the only place the new address is actually written — writes one
/// `user.mail_changed` event with no actor and no address in its details.
#[test]
fn self_service_mail_change_confirmation_writes_one_user_mail_changed_event() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("identchg-mail");
        let old_mail = format!("{name}@example.com");
        let new_mail = format!("{name}-new@example.com");
        app.create_test_user(&name, "test-password-123", &old_mail)
            .await;
        let user_id = user_id_of(app, &name).await;

        // What `profile_update` does when it queues a pending change: record
        // it in `data`, so the confirmation step has an address to adopt.
        sqlx::query("UPDATE users SET data = $2::jsonb WHERE id = $1")
            .bind(user_id)
            .bind(serde_json::json!({ "pending_email": new_mail }))
            .execute(&app.db)
            .await
            .unwrap();
        let (_token_row, raw_token) =
            EmailVerificationToken::create(&app.db, user_id, PURPOSE_EMAIL_CHANGE)
                .await
                .expect("create a verification token");

        let response = app
            .request(
                Request::get(format!("/user/verify-email/{raw_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "the confirmation must be accepted: {}",
            response.status()
        );

        let stored_mail: String = sqlx::query_scalar("SELECT mail FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(stored_mail, new_mail, "the address must actually change");

        let rows = events(app, "user.mail_changed", user_id).await;
        assert_eq!(rows.len(), 1, "exactly one event: {rows:?}");
        let (actor_id, details) = &rows[0];
        assert_eq!(
            *actor_id, None,
            "the account holder confirmed their own change"
        );
        let dump = details.to_string();
        assert!(
            !dump.contains(&old_mail) && !dump.contains(&new_mail),
            "no address, old or new, belongs in the event: {dump}"
        );
    });
}

/// The admin "Edit user" form changes both the name and the e-mail address in
/// one write, writing one event of each kind, both naming the administrator
/// as the actor.
#[test]
fn admin_edit_writes_one_event_of_each_kind() {
    run_test(async {
        let app = shared_app().await;
        let old_name = common::username("identchg-admintarget");
        let old_mail = format!("{old_name}@example.com");
        app.create_test_user(&old_name, "test-password-123", &old_mail)
            .await;
        let user_id = user_id_of(app, &old_name).await;

        let admin_name = common::username("identchg-admin");
        let admin_cookies = app
            .create_and_login_admin(
                &admin_name,
                "correct-horse-battery-staple",
                &format!("{admin_name}@test.local"),
            )
            .await;
        let admin_id = user_id_of(app, &admin_name).await;

        let response = app
            .request_with_cookies(
                Request::get(format!("/admin/people/{user_id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &admin_cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        let token = extract_csrf_token(&html).expect("CSRF token");
        let build_id = extract_form_build_id(&html).unwrap_or_default();

        let new_name = format!("{old_name}_edited");
        let new_mail = format!("edited-{old_name}@example.com");
        let body = format!(
            "_token={token}&_form_build_id={build_id}&name={new_name}&mail={new_mail}&status=1"
        );
        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/people/{user_id}/edit"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &admin_cookies,
            )
            .await;
        assert!(
            response.status().is_redirection(),
            "the edit must succeed: {}",
            response.status()
        );

        let name_rows = events(app, "user.name_changed", user_id).await;
        assert_eq!(name_rows.len(), 1, "{name_rows:?}");
        assert_eq!(name_rows[0].0, Some(admin_id));
        assert_eq!(name_rows[0].1["from"], old_name);
        assert_eq!(name_rows[0].1["to"], new_name);

        let mail_rows = events(app, "user.mail_changed", user_id).await;
        assert_eq!(mail_rows.len(), 1, "{mail_rows:?}");
        assert_eq!(mail_rows[0].0, Some(admin_id));
    });
}
