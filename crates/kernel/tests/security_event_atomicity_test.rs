#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A security event that records a stored change commits with that change
//! (B277): when the event cannot be written, the change does not persist and
//! the request does not report success.
//!
//! Before this, every such path committed its change and wrote its event
//! afterwards, on the pool, and a failed write was only logged. An account
//! could be created, a password changed, an account deleted or an API token
//! issued with no record of it in the security log.
//!
//! Each test makes the event insert fail with a trigger on
//! `security_audit_log` that raises for one `User-Agent` only, the one its own
//! request sends, so the tests running beside it on the shared database keep
//! writing their events. The trigger is dropped when the test ends, pass or
//! fail.
//!
//! Requires Postgres and Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::{RegistrationMode, SiteConfig};
use uuid::Uuid;

const PASSWORD: &str = "b277-old-password-1";
const NEW_PASSWORD: &str = "b277-new-password-2";

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

/// Make every security event a request with `user_agent` writes fail, until
/// the test ends. Returns nothing: the trigger is the effect.
async fn refuse_events_from(app: &TestApp, user_agent: &str) {
    // A user agent is free text a test chose: quoted, never interpolated raw.
    let quoted = user_agent.replace('\'', "''");
    refuse_events_where(app, &format!("NEW.user_agent = '{quoted}'")).await;
}

/// Make every security event matching `condition` (a trigger `WHEN` clause
/// over `NEW`, built by the test from values it chose) fail, until the test
/// ends.
async fn refuse_events_where(app: &TestApp, condition: &str) {
    let name = format!("b277_refuse_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'security event refused by the B277 test'; END $$"
    )))
    .execute(&app.db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER {name} BEFORE INSERT ON security_audit_log FOR EACH ROW \
         WHEN ({condition}) EXECUTE FUNCTION {name}()"
    )))
    .execute(&app.db)
    .await
    .unwrap();
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER IF EXISTS {name} ON security_audit_log"
        )))
        .execute(&db)
        .await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP FUNCTION IF EXISTS {name}()"
        )))
        .execute(&db)
        .await;
    });
}

/// A `User-Agent` no other request sends.
fn unique_agent(label: &str) -> String {
    format!("b277-{label}-{}", Uuid::now_v7().simple())
}

/// A fresh account, deleted when the test ends: `(id, name)`.
async fn account(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    app.create_test_user(&name, PASSWORD, &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM api_tokens WHERE user_id = $1")
            .bind(id)
            .execute(&db)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
    (id, name)
}

/// The page's `csrf-token` meta tag.
fn meta_csrf(html: &str) -> String {
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("a csrf-token meta tag") + marker.len();
    html[start..].split('"').next().unwrap().to_string()
}

async fn get_page(app: &TestApp, cookies: &str, path: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{path} did not load");
    body_of(response).await
}

/// Open registration for the test, and put the settings back after.
async fn open_registration(app: &TestApp) {
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
    RegistrationMode::Open
        .save(&app.db)
        .await
        .expect("open registration");
}

/// A registration whose `account.created` cannot be written creates no
/// account and does not answer as a registration that worked.
#[test]
fn a_registration_whose_event_fails_creates_no_account() {
    run_test(async {
        let app = shared_app().await;
        open_registration(app).await;
        let name = common::username("b277_reg");
        let ip = common::test_ip_for(&name);
        app.state.rate_limiter().reset("register", &ip).await.ok();
        let db = app.db.clone();
        let cleanup_name = name.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                .bind(&cleanup_name)
                .execute(&db)
                .await;
        });

        let agent = unique_agent("registration");
        refuse_events_from(app, &agent).await;
        let response = app
            .request(
                Request::post("/user/register/json")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::USER_AGENT, &agent)
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({
                            "username": name,
                            "mail": format!("{name}@example.com"),
                            "password": PASSWORD,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        let status = response.status();
        let body = body_of(response).await;

        assert!(
            !status.is_success(),
            "a registration whose event failed answered {status}: {body}"
        );
        let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(
            accounts, 0,
            "the account was created with no account.created event"
        );
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE recipient = $1")
                .bind(format!("{name}@example.com"))
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(queued, 0, "the verification mail was queued anyway");
    });
}

/// A password change whose `password.changed` cannot be written leaves the
/// old password in place and says the change failed.
#[test]
fn a_password_change_whose_event_fails_keeps_the_old_password() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_pw").await;
        let cookies = app.login(&name, PASSWORD).await;
        let html = get_page(app, &cookies, "/user/profile").await;
        let fields = common::form_fields(&html, "user_password_change");

        let agent = unique_agent("password");
        refuse_events_from(app, &agent).await;
        let response = app
            .request_with_cookies(
                Request::post("/user/password")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::USER_AGENT, &agent)
                    .body(Body::from(format!(
                        "{fields}&current_password={PASSWORD}&new_password={NEW_PASSWORD}\
                         &confirm_password={NEW_PASSWORD}"
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        let body = body_of(response).await;

        assert!(
            !body.contains("Password changed successfully."),
            "a password change whose event failed reported success"
        );
        assert!(
            body.contains("An error occurred while changing your password."),
            "a password change whose event failed did not say so"
        );
        let user = trovato_kernel::models::User::find_by_id(&app.db, id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            user.verify_password(PASSWORD),
            "the password changed with no password.changed event"
        );
        assert!(!user.verify_password(NEW_PASSWORD));
    });
}

/// An administrator's delete whose `account.deleted` cannot be written leaves
/// the account in place and answers with the delete's own failure.
#[test]
fn an_admin_delete_whose_event_fails_keeps_the_account() {
    run_test(async {
        let app = shared_app().await;
        let (target, _) = account(app, "b277_del").await;
        let admin_name = common::username("b277_del_admin");
        let admin = app
            .create_and_login_admin(&admin_name, PASSWORD, &format!("{admin_name}@example.com"))
            .await;
        let html = get_page(app, &admin, &format!("/admin/people/{target}/delete")).await;
        let fields = common::form_fields(&html, "user_delete_form");

        let agent = unique_agent("delete");
        refuse_events_from(app, &agent).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/people/{target}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::USER_AGENT, &agent)
                    .body(Body::from(fields))
                    .unwrap(),
                &admin,
            )
            .await;
        let status = response.status();
        let body = body_of(response).await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a delete whose event failed must answer as a failed delete: {body}"
        );
        assert!(body.contains("Failed to delete user."), "{body}");
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(target)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(
            exists,
            "the account was deleted with no account.deleted event"
        );
    });
}

/// An API token whose `api_token.created` cannot be written is not issued:
/// no token row, and no token in the answer.
#[test]
fn an_api_token_whose_event_fails_is_not_issued() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_tok").await;
        let cookies = app.login(&name, PASSWORD).await;
        let html = get_page(app, &cookies, "/user/tokens").await;
        let csrf = meta_csrf(&html);

        let agent = unique_agent("token");
        refuse_events_from(app, &agent).await;
        let response = app
            .request_with_cookies(
                Request::post("/api/tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::USER_AGENT, &agent)
                    .header("x-csrf-token", csrf)
                    .body(Body::from(
                        serde_json::json!({ "name": "b277", "expires_in_days": 30 }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = body_of(response).await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a token whose event failed must answer as a failed create: {body}"
        );
        assert!(
            !body.contains("\"token\""),
            "the raw token was handed out: {body}"
        );
        let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens WHERE user_id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(
            tokens, 0,
            "the token was issued with no api_token.created event"
        );
    });
}

// ── One form, one transaction ──────────────────────────────────────────────
//
// The administrator's account form makes several changes from one submission:
// the account's fields, its roles, its password and, for a block, the deletion
// of its API tokens. They commit together with every event recording them,
// so an event that cannot be written leaves the account exactly as it was.

/// The account `id` as stored: `(mail, status)`.
async fn stored(app: &TestApp, id: Uuid) -> (String, i16) {
    sqlx::query_as("SELECT mail, status FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// A role nobody else uses, removed when the test ends.
async fn own_role(app: &TestApp, prefix: &str) -> Uuid {
    let role = trovato_kernel::models::Role::create(
        &app.db,
        &format!("{prefix}_{}", Uuid::now_v7().simple()),
    )
    .await
    .unwrap();
    common::track_test_role(&app.db, role.id);
    role.id
}

/// Whether account `id` holds role `role`.
async fn holds(app: &TestApp, id: Uuid, role: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_roles WHERE user_id = $1 AND role_id = $2)",
    )
    .bind(id)
    .bind(role)
    .fetch_one(&app.db)
    .await
    .unwrap()
}

/// A signed-in superuser of the test's own.
async fn admin(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(&name, PASSWORD, &format!("{name}@example.com"))
        .await
}

/// Post the administrator's edit form for `id` with `fields` (the form's own
/// token added).
async fn post_edit(app: &TestApp, admin: &str, id: Uuid, fields: &str) -> (StatusCode, String) {
    let html = get_page(app, admin, &format!("/admin/people/{id}/edit")).await;
    let form = common::form_fields(&html, "user_form");
    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/people/{id}/edit"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("{form}&{fields}")))
                .unwrap(),
            admin,
        )
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

/// Blocking an account deletes its API tokens. When an `api_token.revoked`
/// cannot be written, the block does not happen either: the account stays
/// active, keeps its token and its sessions, and the save says it failed.
///
/// The tokens used to be deleted after the block had committed, and a failed
/// delete was only logged: the account was blocked, the screen said so, and
/// the tokens were still there to work again once it was unblocked.
#[test]
fn an_admin_block_whose_token_event_fails_blocks_nothing() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_block").await;
        let signed_in = app.login(&name, PASSWORD).await;
        // One request, so the tracking middleware indexes the session.
        let _ = get_page(app, &signed_in, "/user/profile").await;
        let sessions = app.state.session_registry().list(id).await.unwrap().len();
        assert_eq!(sessions, 1, "the account's session is indexed");
        trovato_kernel::services::api_token::create(&app.state, id, "b277", None)
            .await
            .unwrap();
        let admin = admin(app, "b277_block_admin").await;

        refuse_events_where(
            app,
            &format!("NEW.kind = 'api_token.revoked' AND NEW.user_id = '{id}'"),
        )
        .await;
        // No `status` field: the box unticked, the account blocked.
        let (status, body) = post_edit(
            app,
            &admin,
            id,
            &format!(
                "name={}&mail={}",
                urlencoding::encode(&name),
                urlencoding::encode(&format!("{name}@example.com"))
            ),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a block whose token event failed answered as a save: {body}"
        );
        assert_eq!(stored(app, id).await.1, 1, "the account was blocked anyway");
        let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens WHERE user_id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(tokens, 1, "the token went with no api_token.revoked event");
        assert_eq!(
            app.state.session_registry().list(id).await.unwrap().len(),
            sessions,
            "the sessions of an account the save did not block were ended"
        );
    });
}

/// An edit changing the address, the blocked state and the password at once,
/// whose `password.changed` cannot be written, changes none of the three.
///
/// The fields used to commit first and the password after, so a failed
/// password event left the new address and the block in place (and skipped
/// the block's session and token cleanup).
#[test]
fn an_admin_edit_whose_password_event_fails_changes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_editpw").await;
        let before = stored(app, id).await;
        let admin = admin(app, "b277_editpw_admin").await;

        refuse_events_where(
            app,
            &format!("NEW.kind = 'password.changed' AND NEW.user_id = '{id}'"),
        )
        .await;
        let (status, body) = post_edit(
            app,
            &admin,
            id,
            &format!(
                "name={}&mail={}&password={NEW_PASSWORD}",
                urlencoding::encode(&name),
                urlencoding::encode(&format!("{name}+new@example.com"))
            ),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "an edit whose password event failed answered as a save: {body}"
        );
        assert_eq!(
            stored(app, id).await,
            before,
            "the address or the block stood though the edit failed"
        );
        let user = trovato_kernel::models::User::find_by_id(&app.db, id)
            .await
            .unwrap()
            .unwrap();
        assert!(user.verify_password(PASSWORD), "the password changed");
    });
}

/// An edit changing the address and granting a role, whose
/// `user.roles_changed` cannot be written, changes neither.
///
/// A failed membership change used to be logged and the save carry on, so
/// the address changed, the role did not, and the screen said it had saved.
#[test]
fn an_admin_edit_whose_role_event_fails_changes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_editrole").await;
        let role = own_role(app, "b277_editrole").await;
        let before = stored(app, id).await;
        let admin = admin(app, "b277_editrole_admin").await;

        refuse_events_where(
            app,
            &format!("NEW.kind = 'user.roles_changed' AND NEW.user_id = '{id}'"),
        )
        .await;
        let (status, body) = post_edit(
            app,
            &admin,
            id,
            &format!(
                "name={}&mail={}&status=1&role_{role}=1",
                urlencoding::encode(&name),
                urlencoding::encode(&format!("{name}+new@example.com"))
            ),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "an edit whose role event failed answered as a save: {body}"
        );
        assert_eq!(stored(app, id).await, before, "the address changed anyway");
        assert!(!holds(app, id, role).await, "the role was granted anyway");
    });
}

/// An account added with a role, whose `user.roles_changed` cannot be
/// written, is not created at all.
#[test]
fn an_admin_add_whose_role_event_fails_creates_no_account() {
    run_test(async {
        let app = shared_app().await;
        let role = own_role(app, "b277_addrole").await;
        let admin = admin(app, "b277_addrole_admin").await;
        let name = common::username("b277_addrole");
        let db = app.db.clone();
        let cleanup_name = name.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                .bind(&cleanup_name)
                .execute(&db)
                .await;
        });

        refuse_events_where(
            app,
            &format!("NEW.kind = 'user.roles_changed' AND NEW.details->>'role_id' = '{role}'"),
        )
        .await;
        let html = get_page(app, &admin, "/admin/people/add").await;
        let form = common::form_fields(&html, "user_form");
        let response = app
            .request_with_cookies(
                Request::post("/admin/people/add")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "{form}&name={name}&mail={}&password={PASSWORD}\
                         &status=1&role_{role}=1",
                        urlencoding::encode(&format!("{name}@example.com"))
                    )))
                    .unwrap(),
                &admin,
            )
            .await;
        let status = response.status();
        let body = body_of(response).await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "an add whose role event failed answered as a save: {body}"
        );
        let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(accounts, 0, "the account was created without its role");
    });
}

// ── Deletion's session count ───────────────────────────────────────────────

/// `account.deleted` counts the sessions the deletion ended, not the
/// sessions it found listed: an index entry that names no session it can end
/// is not counted.
///
/// The count used to be taken from a listing before the delete, and the
/// sessions ended after it with any failure only logged, so the event could
/// claim sessions were revoked that never were.
#[test]
fn account_deleted_counts_only_the_sessions_it_ended() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_sesscount").await;
        let signed_in = app.login(&name, PASSWORD).await;
        // One request, so the tracking middleware indexes the session.
        let _ = get_page(app, &signed_in, "/user/profile").await;
        let real = app.state.session_registry().list(id).await.unwrap();
        assert!(!real.is_empty(), "signing in indexes a session");

        // Listed, but filed under a device id it does not carry, so the
        // revoke, which looks an entry up by its device id, ends nothing.
        let mut stray = real[0].clone();
        stray.device_id = Uuid::now_v7();
        let mut conn = app
            .state
            .redis()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let key = format!("user_sessions:{id}");
        let _: () = redis::AsyncCommands::hset(
            &mut conn,
            &key,
            Uuid::now_v7().to_string(),
            serde_json::to_string(&stray).unwrap(),
        )
        .await
        .unwrap();
        common::defer_cleanup(move || async move {
            let _: Result<(), _> = redis::AsyncCommands::del(&mut conn, &key).await;
        });
        assert_eq!(
            app.state.session_registry().list(id).await.unwrap().len(),
            real.len() + 1
        );

        let admin = admin(app, "b277_sesscount_admin").await;
        let html = get_page(app, &admin, &format!("/admin/people/{id}/delete")).await;
        let fields = common::form_fields(&html, "user_delete_form");
        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/people/{id}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(fields))
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let details: serde_json::Value = sqlx::query_scalar(
            "SELECT details FROM security_audit_log \
             WHERE kind = 'account.deleted' AND subject_hash = $1",
        )
        .bind(trovato_kernel::audit::hash_subject(&id.to_string()))
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(
            details["sessions_revoked"],
            real.len(),
            "the event counted a session the deletion did not end"
        );
    });
}

// ── Concurrent writers ─────────────────────────────────────────────────────
//
// Each event must describe what its own write changed, even when another
// transaction changes the same row first and commits while this one waits.

/// Wait until a statement starting with `prefix` is blocked on a lock: the
/// second writer is then queued behind the first.
async fn wait_for_blocked(app: &TestApp, prefix: &str) {
    for _ in 0..200 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE wait_event_type = 'Lock' AND starts_with(query, $1)",
        )
        .bind(prefix)
        .fetch_one(&app.db)
        .await
        .unwrap();
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("no statement starting with {prefix:?} ever waited on a lock");
}

/// An update that waited on another reports, as the earlier value, what that
/// one committed: putting an address back is a change from the address the
/// other request had set, and `user.mail_changed` records it.
///
/// The earlier values came from an unlocked read of the statement's snapshot,
/// from before the other request committed, so the update compared the
/// address with itself and recorded nothing.
#[test]
fn an_update_that_waited_records_the_change_from_what_committed() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_race_mail").await;
        let original = format!("{name}@example.com");
        let between = format!("{name}+between@example.com");

        let mut first = app.db.begin().await.unwrap();
        sqlx::query("UPDATE users SET mail = $1 WHERE id = $2")
            .bind(&between)
            .bind(id)
            .execute(&mut *first)
            .await
            .unwrap();
        let db = app.db.clone();
        let restore = original.clone();
        let second = tokio::spawn(async move {
            let mut conn = db.acquire().await.unwrap();
            trovato_kernel::models::User::update_returning_before(
                &mut conn,
                id,
                trovato_kernel::models::UpdateUser {
                    mail: Some(restore),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap()
        });
        wait_for_blocked(app, "UPDATE users SET mail").await;
        first.commit().await.unwrap();
        let (after, before) = second.await.unwrap();

        assert_eq!(after.mail, original);
        assert_eq!(
            before.mail, between,
            "the earlier address is the one the other request committed"
        );
    });
}

/// A password set while another request was clearing it is recorded as
/// `password.set` (the account had none once that one committed), not as
/// `password.changed`.
#[test]
fn a_password_write_that_waited_reads_whether_one_was_there_as_committed() {
    run_test(async {
        let app = shared_app().await;
        let (id, _) = account(app, "b277_race_pw").await;

        let mut first = app.db.begin().await.unwrap();
        trovato_kernel::models::User::clear_password(&mut *first, id)
            .await
            .unwrap();
        let state = app.state.clone();
        let second = tokio::spawn(async move {
            state
                .users()
                .update_password(
                    id,
                    NEW_PASSWORD,
                    &trovato_kernel::tap::UserContext::authenticated(id, vec![]),
                    "self_service",
                )
                .await
                .unwrap()
        });
        wait_for_blocked(app, "UPDATE users SET pass = $1").await;
        first.commit().await.unwrap();
        assert!(second.await.unwrap());

        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM security_audit_log WHERE user_id = $1 \
             AND kind IN ('password.set', 'password.changed')",
        )
        .bind(id)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(kinds, ["password.set"]);
    });
}

/// A role assignment that puts back a membership another request removed
/// meanwhile records `user.roles_changed`: it made a change, whatever an
/// earlier read said the account held.
#[test]
fn an_assignment_that_restores_a_concurrently_removed_role_is_recorded() {
    run_test(async {
        let app = shared_app().await;
        let (id, _) = account(app, "b277_race_role").await;
        let role = own_role(app, "b277_race_role").await;
        trovato_kernel::models::Role::assign_to_user(&app.db, id, role)
            .await
            .unwrap();

        let mut first = app.db.begin().await.unwrap();
        trovato_kernel::models::Role::remove_from_user(&mut *first, id, role)
            .await
            .unwrap();
        let state = app.state.clone();
        let second =
            tokio::spawn(async move { state.roles().assign_to_user(id, role).await.unwrap() });
        wait_for_blocked(app, "INSERT INTO user_roles").await;
        first.commit().await.unwrap();
        second.await.unwrap();

        assert!(holds(app, id, role).await);
        let added: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM security_audit_log WHERE kind = 'user.roles_changed' \
             AND user_id = $1 AND details->>'role_id' = $2 AND details->>'change' = 'added'",
        )
        .bind(id)
        .bind(role.to_string())
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(added, 1, "the membership was put back with no event");
    });
}

/// A grant another transaction made while this one waited is not this
/// one's to record: adding a permission that the other committed first
/// changes nothing here, and writes no `role.permissions_changed`.
#[test]
fn a_grant_another_transaction_made_is_not_recorded_as_this_ones() {
    run_test(async {
        let app = shared_app().await;
        let role = own_role(app, "b277_race_grant").await;

        let mut first = app.db.begin().await.unwrap();
        trovato_kernel::models::Role::add_permission(&mut *first, role, "access content")
            .await
            .unwrap();
        let state = app.state.clone();
        let second = tokio::spawn(async move {
            state
                .roles()
                .add_permission(role, "access content")
                .await
                .unwrap();
        });
        wait_for_blocked(app, "INSERT INTO role_permissions").await;
        first.commit().await.unwrap();
        second.await.unwrap();

        let recorded: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM security_audit_log \
             WHERE kind = 'role.permissions_changed' AND details->>'role_id' = $1",
        )
        .bind(role.to_string())
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(
            recorded, 0,
            "another transaction's grant was recorded as this one's"
        );
    });
}
