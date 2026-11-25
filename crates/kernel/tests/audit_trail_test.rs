#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every write service records an audit row, and every authentication and
//! account change records a security event, whichever path made it.
//!
//! The content audit log (`trovato_audit_log`'s `audit_log`) used to record
//! nothing: `AuditService::log` had no callers. Password login emitted no
//! login events where passkey login did, a reset by email or from the command
//! line emitted no `password.changed`, and role, permission and admin account
//! changes had no event kinds at all. Each test here writes through a service
//! (or the route a person uses) and reads back the row it should have left.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding, username};
use trovato_kernel::models::{CreateItem, CreateUser, UpdateUser};
use trovato_kernel::services::audit::AuditActor;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// The content type the item test owns.
const TYPE: &str = "b88b_audit_page";

/// The shared app with `trovato_audit_log`'s table in place and the plugin on.
async fn app() -> &'static TestApp {
    let app = shared_app().await;
    common::ensure_plugin_tables(app, "trovato_audit_log");
    app.ensure_plugin_enabled("trovato_audit_log").await;
    app
}

fn admin(id: Uuid) -> UserContext {
    UserContext::administrator(id, vec!["administer site".to_string()])
}

/// `(action, user_id, ip_address, details)` of every audit row for one entity,
/// oldest first.
async fn audit_rows(
    app: &TestApp,
    entity_type: &str,
    entity_id: &str,
) -> Vec<(String, Option<Uuid>, String, serde_json::Value)> {
    sqlx::query_as(
        "SELECT action, user_id, ip_address, details FROM audit_log \
         WHERE entity_type = $1 AND entity_id = $2 ORDER BY created, id",
    )
    .bind(entity_type)
    .bind(entity_id)
    .fetch_all(&app.db)
    .await
    .expect("read audit_log")
}

fn actions(rows: &[(String, Option<Uuid>, String, serde_json::Value)]) -> Vec<&str> {
    rows.iter().map(|r| r.0.as_str()).collect()
}

/// `(outcome, actor_id, details)` of the security events of `kind` about
/// `user_id`, newest first.
async fn security_events(
    app: &TestApp,
    kind: &str,
    user_id: Uuid,
) -> Vec<(String, Option<Uuid>, serde_json::Value)> {
    sqlx::query_as(
        "SELECT outcome, actor_id, details FROM security_audit_log \
         WHERE kind = $1 AND user_id = $2 ORDER BY created DESC, id DESC",
    )
    .bind(kind)
    .bind(user_id)
    .fetch_all(&app.db)
    .await
    .expect("read security_audit_log")
}

/// Security events of `kind` whose `details.role_id` is `role_id`.
async fn role_events(app: &TestApp, kind: &str, role_id: Uuid) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT details FROM security_audit_log \
         WHERE kind = $1 AND details->>'role_id' = $2 ORDER BY created, id",
    )
    .bind(kind)
    .bind(role_id.to_string())
    .fetch_all(&app.db)
    .await
    .expect("read security_audit_log")
}

async fn new_user(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    (common::user_id_of(app, &name).await, name)
}

fn unique(prefix: &str) -> String {
    format!("{prefix}_{}", &Uuid::now_v7().simple().to_string()[20..])
}

// ---------------------------------------------------------------------------
// Content audit rows, one per write service
// ---------------------------------------------------------------------------

#[test]
fn item_writes_are_audited() {
    run_test(async {
        let app = app().await;
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $1, 'B88b audit fixture', true, 'Title', 'core', '{}'::jsonb) \
             ON CONFLICT (type) DO NOTHING",
        )
        .bind(TYPE)
        .execute(&app.db)
        .await
        .unwrap();
        let (author, _) = new_user(app, "b88b-item").await;
        let user = admin(author);
        let items = app.state.items();

        let item = items
            .create(
                CreateItem {
                    item_type: TYPE.to_string(),
                    title: "Audited item".to_string(),
                    author_id: author,
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: None,
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &user,
            )
            .await
            .expect("create item");
        let update =
            serde_json::from_value(serde_json::json!({ "title": "Audited item, edited" })).unwrap();
        items
            .update(item.id, update, &user)
            .await
            .expect("update item");

        let first_revision: Uuid = sqlx::query_scalar(
            "SELECT id FROM item_revision WHERE item_id = $1 ORDER BY created, id LIMIT 1",
        )
        .bind(item.id)
        .fetch_one(&app.db)
        .await
        .expect("the item's first revision");
        items
            .delete_revision(item.id, first_revision, &user)
            .await
            .expect("delete revision");

        items.delete(item.id, &user).await.expect("delete item");

        let rows = audit_rows(app, "item", &item.id.to_string()).await;
        assert_eq!(
            actions(&rows),
            ["create", "update", "delete_revision", "delete"],
            "{rows:?}"
        );
        assert!(rows.iter().all(|r| r.1 == Some(author)), "{rows:?}");
        assert_eq!(rows[1].3["title"], "Audited item, edited");
        assert_eq!(rows[2].3["revision_id"], first_revision.to_string());
    });
}

#[test]
fn comment_writes_are_audited() {
    run_test(async {
        let app = app().await;
        common::ensure_plugin_tables(app, "trovato_comments");
        app.ensure_plugin_enabled("trovato_comments").await;
        let (author, _) = new_user(app, "b88b-comment").await;
        let item_id: Uuid = sqlx::query_scalar(
            "INSERT INTO item (id, type, title, status, author_id, created, changed, stage_id, fields, language) \
             VALUES ($1, 'page', 'Commented', 1, $2, 1, 1, $3, '{}'::jsonb, 'en') RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(author)
        .bind(trovato_kernel::models::stage::LIVE_STAGE_ID)
        .fetch_one(&app.db)
        .await
        .expect("seed item");
        let user = admin(author);
        let comments = app.state.comments();

        let create = serde_json::from_value(serde_json::json!({
            "item_id": item_id, "author_id": author, "body": "First", "status": 1
        }))
        .unwrap();
        let comment = comments
            .create(create, &user)
            .await
            .expect("create comment");
        let update =
            serde_json::from_value(serde_json::json!({ "body": "First, edited" })).unwrap();
        comments.update(comment.id, update, &user).await.unwrap();
        comments.delete(comment.id, &user).await.unwrap();

        let rows = audit_rows(app, "comment", &comment.id.to_string()).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
        assert_eq!(rows[0].3["item_id"], item_id.to_string());
    });
}

#[test]
fn user_writes_are_audited() {
    run_test(async {
        let app = app().await;
        let (actor, _) = new_user(app, "b88b-admin").await;
        let name = username("b88b-account");
        let users = app.state.users();

        let user = users
            .create(
                CreateUser {
                    name: name.clone(),
                    password: "test-password-123".to_string(),
                    mail: format!("{name}@example.com"),
                    is_admin: false,
                },
                &admin(actor),
            )
            .await
            .expect("create user");
        users
            .update(
                user.id,
                UpdateUser {
                    mail: Some(format!("new-{name}@example.com")),
                    ..Default::default()
                },
                &admin(actor),
            )
            .await
            .unwrap();
        users.delete(user.id, &admin(actor)).await.unwrap();

        let rows = audit_rows(app, "user", &user.id.to_string()).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
        assert!(rows.iter().all(|r| r.1 == Some(actor)), "{rows:?}");
        assert_eq!(rows[1].3["changed"], serde_json::json!(["mail"]));
    });
}

#[test]
fn role_writes_are_audited() {
    run_test(async {
        let app = app().await;
        let roles = app.state.roles();
        let role = roles
            .create(&unique("b88b_role"))
            .await
            .expect("create role");
        common::track_test_role(&app.db, role.id);
        roles
            .update(role.id, &unique("b88b_renamed"))
            .await
            .unwrap();
        roles
            .save_permissions(role.id, &["access content".to_string()])
            .await
            .unwrap();
        roles.delete(role.id).await.unwrap();

        let rows = audit_rows(app, "role", &role.id.to_string()).await;
        assert_eq!(
            actions(&rows),
            ["create", "update", "permissions", "delete"],
            "{rows:?}"
        );
        assert_eq!(rows[2].3["granted"], serde_json::json!(["access content"]));
    });
}

#[test]
fn category_and_tag_writes_are_audited() {
    run_test(async {
        let app = app().await;
        let categories = app.state.categories();
        let id = unique("b88b_cat");
        let create =
            serde_json::from_value(serde_json::json!({ "id": id, "label": "Audited" })).unwrap();
        categories
            .create_category(create)
            .await
            .expect("create category");
        let tag_input = serde_json::from_value(serde_json::json!({
            "category_id": id, "label": "Audited tag"
        }))
        .unwrap();
        let tag = categories.create_tag(tag_input).await.expect("create tag");
        let tag_update =
            serde_json::from_value(serde_json::json!({ "label": "Renamed tag" })).unwrap();
        categories.update_tag(tag.id, tag_update).await.unwrap();
        categories.delete_tag(tag.id).await.unwrap();
        let update =
            serde_json::from_value(serde_json::json!({ "label": "Audited, renamed" })).unwrap();
        categories.update_category(&id, update).await.unwrap();
        categories.delete_category(&id).await.unwrap();

        let rows = audit_rows(app, "category", &id).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
        let rows = audit_rows(app, "tag", &tag.id.to_string()).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
    });
}

#[test]
fn alias_writes_are_audited() {
    run_test(async {
        let app = app().await;
        let aliases = app.state.aliases();
        let path = unique("b88b-alias");
        let create = serde_json::from_value(serde_json::json!({
            "source": format!("/item/{}", Uuid::now_v7()), "alias": path
        }))
        .unwrap();
        let alias = aliases.create(create).await.expect("create alias");
        let update =
            serde_json::from_value(serde_json::json!({ "alias": format!("{path}-2") })).unwrap();
        aliases.update(alias.id, update).await.unwrap();
        aliases.delete(alias.id).await.unwrap();

        let rows = audit_rows(app, "url_alias", &alias.id.to_string()).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
        assert_eq!(rows[1].3["alias"], format!("/{path}-2"));
    });
}

#[test]
fn menu_link_writes_are_audited() {
    run_test(async {
        let app = app().await;
        let links = app.state.menu_links();
        let create = serde_json::from_value(serde_json::json!({
            "menu_name": "main", "path": "/b88b", "title": unique("Audited link")
        }))
        .unwrap();
        let link = links.create(create).await.expect("create link");
        let update =
            serde_json::from_value(serde_json::json!({ "title": "Renamed link" })).unwrap();
        links.update(link.id, update).await.unwrap();
        links.delete(link.id).await.unwrap();

        let rows = audit_rows(app, "menu_link", &link.id.to_string()).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
    });
}

#[test]
fn tile_writes_are_audited_with_the_request_actor() {
    run_test(async {
        let app = app().await;
        let (actor, _) = new_user(app, "b88b-tile").await;
        let tiles = app.state.tiles();
        let create = serde_json::from_value(serde_json::json!({
            "machine_name": unique("b88b_tile"), "label": "Audited tile"
        }))
        .unwrap();
        // A tile write takes no user: who made it comes from the request.
        let request = AuditActor {
            user_id: Some(actor),
            ip: "203.0.113.9".to_string(),
            user_agent: String::new(),
        };
        let tile = request
            .clone()
            .scope(tiles.create(create))
            .await
            .expect("create tile");
        let update =
            serde_json::from_value(serde_json::json!({ "label": "Renamed tile" })).unwrap();
        request
            .clone()
            .scope(tiles.update(tile.id, update))
            .await
            .unwrap();
        request.scope(tiles.delete(tile.id)).await.unwrap();

        let rows = audit_rows(app, "tile", &tile.id.to_string()).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
        assert!(
            rows.iter()
                .all(|r| r.1 == Some(actor) && r.2 == "203.0.113.9"),
            "{rows:?}"
        );
    });
}

#[test]
fn stage_writes_are_audited() {
    run_test(async {
        let app = app().await;
        let stages = app.state.stage();
        let machine_name = unique("b88b_stage");
        let create = serde_json::from_value(serde_json::json!({
            "label": machine_name, "machine_name": machine_name
        }))
        .unwrap();
        let stage = stages.create_stage(create).await.expect("create stage");
        let update = trovato_kernel::models::stage::UpdateStage {
            label: Some("Renamed stage".to_string()),
            ..Default::default()
        };
        stages.update_stage(stage.id, update).await.unwrap();
        stages.delete_stage(stage.id).await.unwrap();

        let rows = audit_rows(app, "stage", &stage.id.to_string()).await;
        assert_eq!(actions(&rows), ["create", "update", "delete"], "{rows:?}");
    });
}

/// With the plugin off, a write records nothing (and does not fail).
#[test]
fn nothing_is_recorded_while_the_plugin_is_disabled() {
    run_test(async {
        let app = TestApp::new().await;
        common::ensure_plugin_tables(&app, "trovato_audit_log");
        app.state.set_plugin_enabled("trovato_audit_log", false);
        let create = serde_json::from_value(serde_json::json!({
            "machine_name": unique("b88b_off"), "label": "Unaudited tile"
        }))
        .unwrap();
        let tile = app.state.tiles().create(create).await.expect("create tile");
        assert!(
            audit_rows(&app, "tile", &tile.id.to_string())
                .await
                .is_empty()
        );
        app.state.tiles().delete(tile.id).await.unwrap();
    });
}

// ---------------------------------------------------------------------------
// Security events
// ---------------------------------------------------------------------------

async fn post_login(app: &TestApp, name: &str, password: &str) -> StatusCode {
    let ip = test_ip_for(name);
    app.state.lockout().clear_all(name).await.ok();
    app.state.rate_limiter().reset("login", &ip).await.ok();
    app.request(
        Request::post("/user/login/json")
            .header("content-type", "application/json")
            .header("x-forwarded-for", &ip)
            .header("user-agent", "b88b-test-agent")
            .body(Body::from(
                serde_json::json!({ "username": name, "password": password }).to_string(),
            ))
            .unwrap(),
    )
    .await
    .status()
}

#[test]
fn password_login_emits_login_succeeded() {
    run_test(async {
        let app = app().await;
        let (id, name) = new_user(app, "b88b-login-ok").await;
        assert_eq!(
            post_login(app, &name, "test-password-123").await,
            StatusCode::OK
        );

        let events = security_events(app, "auth.login_succeeded", id).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, "success");
        assert_eq!(events[0].2["method"], "password");
        let ua: Option<String> = sqlx::query_scalar(
            "SELECT user_agent FROM security_audit_log WHERE kind = 'auth.login_succeeded' AND user_id = $1",
        )
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(ua.as_deref(), Some("b88b-test-agent"));
    });
}

#[test]
fn password_login_emits_login_failed() {
    run_test(async {
        let app = app().await;
        let (id, name) = new_user(app, "b88b-login-bad").await;
        assert_eq!(
            post_login(app, &name, "not-the-password").await,
            StatusCode::UNAUTHORIZED
        );

        let events = security_events(app, "auth.login_failed", id).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, "failure");
        assert_eq!(events[0].2["method"], "password");
        assert_eq!(events[0].2["reason"], "invalid_credentials");
    });
}

#[test]
fn reset_by_email_emits_password_changed() {
    run_test(async {
        let app = app().await;
        let (id, name) = new_user(app, "b88b-reset").await;
        let (_, token) =
            trovato_kernel::models::password_reset::PasswordResetToken::create(&app.db, id)
                .await
                .unwrap();
        let response = app
            .request(
                Request::post(format!("/user/password-reset/{token}"))
                    .header("content-type", "application/json")
                    .header("accept", "application/json")
                    .header("x-forwarded-for", test_ip_for(&name))
                    .body(Body::from(
                        serde_json::json!({ "password": "a-brand-new-password-1" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let events = security_events(app, "password.changed", id).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].2["via"], "password_reset");
    });
}

#[test]
fn command_line_reset_emits_password_changed() {
    run_test(async {
        let app = app().await;
        let (id, _) = new_user(app, "b88b-cli").await;
        let updated = trovato_kernel::services::user::reset_password_offline(
            &app.db,
            id,
            "a-brand-new-password-2",
        )
        .await
        .unwrap();
        assert!(updated);

        let events = security_events(app, "password.changed", id).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].2["via"], "cli");
        let rows = audit_rows(app, "user", &id.to_string()).await;
        assert_eq!(actions(&rows), ["password"], "{rows:?}");
    });
}

#[test]
fn role_lifecycle_emits_role_events() {
    run_test(async {
        let app = app().await;
        let roles = app.state.roles();
        let role = roles.create(&unique("b88b_sec_role")).await.unwrap();
        common::track_test_role(&app.db, role.id);
        roles
            .update(role.id, &unique("b88b_sec_renamed"))
            .await
            .unwrap();
        roles.delete(role.id).await.unwrap();

        assert_eq!(role_events(app, "role.created", role.id).await.len(), 1);
        assert_eq!(role_events(app, "role.changed", role.id).await.len(), 1);
        assert_eq!(role_events(app, "role.deleted", role.id).await.len(), 1);
    });
}

#[test]
fn permission_grant_changes_emit_an_event() {
    run_test(async {
        let app = app().await;
        let roles = app.state.roles();
        let role = roles.create(&unique("b88b_perm_role")).await.unwrap();
        common::track_test_role(&app.db, role.id);
        roles
            .save_permissions(role.id, &["access content".to_string()])
            .await
            .unwrap();
        roles.save_permissions(role.id, &[]).await.unwrap();
        // Saving the same grants again changes nothing and records nothing.
        roles.save_permissions(role.id, &[]).await.unwrap();

        let events = role_events(app, "role.permissions_changed", role.id).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0]["granted"], serde_json::json!(["access content"]));
        assert_eq!(events[1]["revoked"], serde_json::json!(["access content"]));
    });
}

#[test]
fn an_admin_changing_a_users_roles_emits_an_event() {
    run_test(async {
        let app = app().await;
        let (actor, _) = new_user(app, "b88b-role-admin").await;
        let (target, _) = new_user(app, "b88b-role-target").await;
        let role = app
            .state
            .roles()
            .create(&unique("b88b_member"))
            .await
            .unwrap();
        common::track_test_role(&app.db, role.id);

        let request = AuditActor {
            user_id: Some(actor),
            ..Default::default()
        };
        request
            .clone()
            .scope(app.state.roles().assign_to_user(target, role.id))
            .await
            .unwrap();
        request
            .scope(app.state.roles().remove_from_user(target, role.id))
            .await
            .unwrap();

        let events = security_events(app, "user.roles_changed", target).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events.iter().all(|e| e.1 == Some(actor)), "{events:?}");
        assert_eq!(events[0].2["change"], "removed");
        assert_eq!(events[1].2["change"], "added");
    });
}

#[test]
fn an_admin_blocking_a_user_emits_an_event() {
    run_test(async {
        let app = app().await;
        let (actor, _) = new_user(app, "b88b-block-admin").await;
        let (target, _) = new_user(app, "b88b-block-target").await;
        app.state
            .users()
            .update(
                target,
                UpdateUser {
                    status: Some(0),
                    ..Default::default()
                },
                &admin(actor),
            )
            .await
            .unwrap();

        let events = security_events(app, "user.status_changed", target).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].1, Some(actor));
        assert_eq!(events[0].2["blocked"], true);
    });
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

#[test]
fn the_audit_log_page_lists_the_rows() {
    run_test(async {
        let app = app().await;
        let label = unique("b88b_listed_tile");
        let create = serde_json::from_value(serde_json::json!({
            "machine_name": label, "label": "Listed tile"
        }))
        .unwrap();
        let tile = app.state.tiles().create(create).await.unwrap();

        let (_, cookies) = user_holding(
            app,
            "b88b-auditor",
            &["view audit log", "access administration pages"],
        )
        .await;
        let response = app
            .request_with_cookies(
                Request::get("/admin/reports/audit")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains(&tile.id.to_string()), "the new row is listed");
        assert!(html.contains(&label), "its details are shown");

        let (_, outsider) = user_holding(app, "b88b-outsider", &[]).await;
        let response = app
            .request_with_cookies(
                Request::get("/admin/reports/audit")
                    .body(Body::empty())
                    .unwrap(),
                &outsider,
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        app.state.tiles().delete(tile.id).await.unwrap();
    });
}
