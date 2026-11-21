#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Moving an item between stages, and where new content lands.
//!
//! Nothing moved an item between stages (BL-92): item creation passed no stage,
//! `UpdateItem` had none, no route or host function or bulk action changed one,
//! and the tutorial's transition graph (`workflow.editorial`) was read by nobody.
//! The stage marked default was stored and never consulted (BL-93). And
//! `check_access` treated an item whose stage row was missing as public.
//!
//! The rule under test (`docs/design/stage-workflow.md`): a user moves an item
//! from S to T only if they may edit it and hold the permission of a transition
//! S -> T in `workflow.editorial`; with no workflow only an administrator moves
//! items; no transition is implied by another. The three ways in (the edit form,
//! the "Move to stage" bulk action, `save-item`) are thin callers of
//! `ItemService::update`, so a move gets a revision, `tap_item_update`, a search
//! row on its new stage and its aliases and menu links moved with it.
//!
//! The workflow variable and the default stage are site-wide, so every test in
//! this file holds one lock, and each puts back what it changed.
//!
//! Requires Postgres, Redis and the `test_item_writer` fixture `.wasm` (built
//! by CI before the test job): its `tap_item_update` writes `tiw_log`, which is
//! how a dispatch is seen.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test};
use serde_json::json;
use sqlx::PgPool;
use trovato_kernel::models::stage::{LIVE_STAGE_ID, Stage, UpdateStage};
use trovato_kernel::models::{CreateItem, Item, SiteConfig};
use trovato_kernel::tap::{RequestServices, RequestState, UserContext};
use trovato_sdk::host_errors;
use uuid::Uuid;

const WORKFLOW_KEY: &str = "workflow.editorial";
const WITNESS: &str = "test_item_writer";

/// Serializes this file's tests: the workflow and the default stage are global.
static SITE_WIDE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Hold [`SITE_WIDE`] until this test's deferred cleanups have run.
///
/// The cleanups put the workflow and the default stage back, so they are part
/// of the critical section: released any earlier, one test's restore could
/// land on top of the workflow the next test had just set. Registered first,
/// this cleanup runs last.
async fn site_wide() {
    let guard = SITE_WIDE.lock().await;
    common::defer_cleanup(move || async move { drop(guard) });
}

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("stage workflow app init thread panicked")
    })
}

/// An app with the witness plugin loaded, which is then disabled again so no
/// app built later loads it.
async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = PgPool::connect(&database_url).await.expect("connect");
    ensure_tiw_log(&pool).await;
    trovato_kernel::plugin::status::install_plugin(&pool, WITNESS, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{WITNESS}': {e:#}"));
    let app = TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await;
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(WITNESS)
        .execute(&pool)
        .await
        .expect("disable the witness");
    pool.close().await;
    app
}

/// Serializes creating `tiw_log` across tests, binaries and shards (the same
/// key `plugin_item_api_test` uses for the same table).
const TIW_LOG_LOCK: i64 = 0x0B68_0000_0002;

async fn ensure_tiw_log(pool: &PgPool) {
    let mut tx = pool.begin().await.expect("begin tiw_log setup");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TIW_LOG_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take tiw_log lock");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tiw_log (\
           id BIGSERIAL PRIMARY KEY, tap TEXT NOT NULL, item_id TEXT NOT NULL, \
           title TEXT NOT NULL, note TEXT NOT NULL)",
    )
    .execute(&mut *tx)
    .await
    .expect("create tiw_log");
    tx.commit().await.expect("commit tiw_log setup");
}

// =============================================================================
// Fixture: stages, a workflow, people, items
// =============================================================================

/// A unique suffix for names this test makes.
fn unique() -> String {
    Uuid::now_v7().simple().to_string()[20..].to_string()
}

/// A stage that is removed again when the test ends (after its items).
async fn stage(app: &TestApp, prefix: &str) -> Stage {
    let id = common::create_test_stage(app, prefix).await;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM url_alias WHERE stage_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM menu_link WHERE stage_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE stage_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM stage_config WHERE tag_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM category_tag WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    Stage::find_by_id(&app.db, id)
        .await
        .expect("read stage")
        .expect("stage exists")
}

/// Store `transitions` as the workflow, and put back whatever was there when
/// the test ends.
async fn set_workflow(app: &TestApp, transitions: serde_json::Value) {
    remember_workflow(app).await;
    SiteConfig::set(&app.db, WORKFLOW_KEY, json!({ "transitions": transitions }))
        .await
        .expect("store the workflow");
}

/// Remove the workflow, putting it back when the test ends.
async fn clear_workflow(app: &TestApp) {
    remember_workflow(app).await;
    SiteConfig::delete(&app.db, WORKFLOW_KEY)
        .await
        .expect("remove the workflow");
}

async fn remember_workflow(app: &TestApp) {
    let previous = SiteConfig::get(&app.db, WORKFLOW_KEY)
        .await
        .expect("read the workflow");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = match previous {
            Some(value) => SiteConfig::set(&pool, WORKFLOW_KEY, value).await,
            None => SiteConfig::delete(&pool, WORKFLOW_KEY).await.map(|_| ()),
        };
    });
}

fn transition(from: &Stage, to: &Stage, permission: &str) -> serde_json::Value {
    json!({
        "from": from.machine_name,
        "to": to.machine_name,
        "permission": permission,
        "label": format!("To {}", to.label),
    })
}

/// A signed-in editor holding `edit any content`, `access content` and
/// `extra`. Returns their id and cookies.
async fn editor(app: &TestApp, extra: &[&str]) -> (Uuid, String) {
    let mut permissions = vec!["edit any content", "access content"];
    permissions.extend_from_slice(extra);
    common::user_holding(app, "b69st", &permissions).await
}

/// A published page on `stage`, titled with a word nobody else uses, removed
/// when the test ends.
async fn item_on(app: &TestApp, stage_id: Uuid, author: Uuid) -> Item {
    let title = format!("b69 stage {}", unique());
    let item = Item::create(
        &app.db,
        CreateItem {
            item_type: "page".to_string(),
            title,
            author_id: author,
            status: Some(1),
            promote: None,
            sticky: None,
            fields: None,
            stage_id: Some(stage_id),
            language: None,
            log: None,
        },
    )
    .await
    .expect("create item");
    let pool = app.db.clone();
    let id = item.id;
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM tiw_log WHERE item_id = $1")
            .bind(id.to_string())
            .execute(&pool)
            .await;
        let _ = Item::delete(&pool, id).await;
    });
    item
}

async fn stage_of(app: &TestApp, id: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT stage_id FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .expect("read stage_id")
}

async fn revisions_of(app: &TestApp, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM item_revision WHERE item_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .expect("count revisions")
}

// =============================================================================
// The ways in: the admin edit form, the bulk action, save-item
// =============================================================================

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

async fn post_form(app: &TestApp, path: &str, cookies: &str, body: String) -> StatusCode {
    app.request_with_cookies(
        Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", common::test_ip_for(cookies))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

fn enc(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Submit the admin edit form for `item` with `_stage` set to `target`, as the
/// form's stage selector does.
async fn move_with_the_form(app: &TestApp, cookies: &str, item: &Item, target: Uuid) -> StatusCode {
    // The form renders for an editor who may edit the item; its CSRF token is
    // what a browser would submit.
    let (_, html) = get(app, &format!("/admin/content/{}/edit", item.id), cookies).await;
    let token = input_value(&html, "_token").unwrap_or_default();
    let build = input_value(&html, "_form_build_id").unwrap_or_default();
    let body = format!(
        "_token={}&_form_build_id={}&title={}&_stage={}",
        enc(&token),
        enc(&build),
        enc(&item.title),
        target
    );
    post_form(
        app,
        &format!("/admin/content/{}/edit", item.id),
        cookies,
        body,
    )
    .await
}

/// Submit the content list's "Move to stage" bulk action for `ids`.
async fn move_with_the_bulk_action(
    app: &TestApp,
    cookies: &str,
    ids: &[Uuid],
    target: Uuid,
) -> StatusCode {
    let (_, html) = get(app, "/admin/content", cookies).await;
    let token = input_value(&html, "_token").unwrap_or_default();
    let mut body = format!("_token={}&action=move_to_stage&stage={target}", enc(&token));
    for id in ids {
        body.push_str(&format!("&ids%5B%5D={id}"));
    }
    post_form(app, "/admin/content/bulk", cookies, body).await
}

/// Call `save-item` from a plugin acting as `user`, and return the host code.
async fn save_item_as(app: &TestApp, user: UserContext, payload: serde_json::Value) -> i32 {
    use trovato_kernel::plugin::limits::ResourceLimits;
    use trovato_kernel::plugin::{DbPolicy, PluginState};
    use wasmtime::{Engine, Linker, Module, Store};

    let payload = payload.to_string();
    let escaped = payload.replace('\\', "\\\\").replace('"', "\\\"");
    let wat = format!(
        r#"
(module
  (import "trovato:kernel/item-api" "save-item"
    (func $call (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 8)
  (data (i32.const 0) "{escaped}")
  (func (export "run") (result i32)
    (call $call (i32.const 0) (i32.const {len}) (i32.const 8192) (i32.const 16384))))
"#,
        len = payload.len()
    );

    let services =
        RequestServices::for_background(app.db.clone(), None, None, reqwest::Client::new());
    services.set_item_service(app.state.items());
    let state = PluginState::with_db_policy(
        RequestState::new(user, services),
        "b69_probe".to_string(),
        Arc::new(DbPolicy::default()),
        ResourceLimits::default(),
    );
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    trovato_kernel::host::register_all(&mut linker).expect("register host functions");
    let module = Module::new(&engine, &wat).expect("compile probe");
    let mut store = Store::new(&engine, state);
    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("instantiate probe");
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .expect("probe exports run");
    run.call_async(&mut store, ()).await.expect("run probe")
}

fn context_of(user_id: Uuid, permissions: &[&str]) -> UserContext {
    UserContext::authenticated(
        user_id,
        permissions.iter().map(|p| (*p).to_string()).collect(),
    )
}

// =============================================================================
// BL-93: new content lands on the stage marked default
// =============================================================================

/// A create that names no stage lands on the stage marked default, not on Live.
#[test]
fn a_create_with_no_stage_lands_on_the_stage_marked_default() {
    run_test(async {
        let app = app();
        site_wide().await;
        let draft = stage(app, "b69default").await;

        // Put Live back as the default when the test ends, whatever happens.
        {
            let pool = app.db.clone();
            common::defer_cleanup(move || async move {
                let _ = Stage::update(
                    &pool,
                    LIVE_STAGE_ID,
                    UpdateStage {
                        label: None,
                        machine_name: None,
                        description: None,
                        visibility: None,
                        is_default: Some(true),
                        weight: None,
                    },
                )
                .await;
            });
        }
        Stage::update(
            &app.db,
            draft.id,
            UpdateStage {
                label: None,
                machine_name: None,
                description: None,
                visibility: None,
                is_default: Some(true),
                weight: None,
            },
        )
        .await
        .expect("mark the stage default");

        let admin = UserContext::administrator(Uuid::nil(), vec![]);
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    title: format!("b69 default {}", unique()),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: None,
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin,
            )
            .await
            .expect("create");
        let id = item.id;
        {
            let pool = app.db.clone();
            common::defer_cleanup(move || async move {
                let _ = Item::delete(&pool, id).await;
            });
        }

        assert_eq!(
            item.stage_id, draft.id,
            "an item created with no stage must land on the stage marked default"
        );
    });
}

// =============================================================================
// BL-92: a permitted move, and everything that comes with it
// =============================================================================

/// An editor holding a transition's permission moves an item through the edit
/// form, and the move is an edit: a revision, a `tap_item_update` dispatch, and
/// a search row on the new stage.
#[test]
fn a_user_holding_the_transition_permission_moves_an_item() {
    run_test(async {
        let app = app();
        site_wide().await;
        let (from, to) = (stage(app, "b69from").await, stage(app, "b69to").await);
        let permission = format!("b69 move {}", unique());
        set_workflow(app, json!([transition(&from, &to, &permission)])).await;
        let (editor_id, cookies) = editor(app, &[&permission]).await;
        let item = item_on(app, from.id, editor_id).await;
        let revisions_before = revisions_of(app, item.id).await;

        let status = move_with_the_form(app, &cookies, &item, to.id).await;
        assert!(
            status.is_redirection(),
            "a permitted move through the form must save, got {status}"
        );
        assert_eq!(stage_of(app, item.id).await, to.id, "the item moved");
        assert_eq!(
            revisions_of(app, item.id).await,
            revisions_before + 1,
            "a move is an edit and gets a revision"
        );

        let taps: Vec<String> =
            sqlx::query_scalar("SELECT tap FROM tiw_log WHERE item_id = $1 ORDER BY id")
                .bind(item.id.to_string())
                .fetch_all(&app.db)
                .await
                .expect("read tiw_log");
        assert!(
            taps.iter().any(|t| t == "update"),
            "a move must dispatch tap_item_update: {taps:?}"
        );

        let word = item.title.rsplit(' ').next().unwrap().to_string();
        let found_on_target = app
            .state
            .search()
            .search(&word, &[to.id], None, 10, 0)
            .await
            .expect("search the target stage");
        assert!(
            found_on_target.results.iter().any(|r| r.id == item.id),
            "the item's search row is on its new stage"
        );
        let found_on_source = app
            .state
            .search()
            .search(&word, &[from.id], None, 10, 0)
            .await
            .expect("search the source stage");
        assert!(
            !found_on_source.results.iter().any(|r| r.id == item.id),
            "the item's search row has left its old stage"
        );
    });
}

/// The edit form's stage selector lists only the stages this user may move this
/// item to, and is absent when there are none.
#[test]
fn the_stage_selector_lists_only_the_reachable_stages() {
    run_test(async {
        let app = app();
        site_wide().await;
        let a = stage(app, "b69sela").await;
        let b = stage(app, "b69selb").await;
        let c = stage(app, "b69selc").await;
        let held = format!("b69 held {}", unique());
        let not_held = format!("b69 notheld {}", unique());
        set_workflow(
            app,
            json!([transition(&a, &b, &held), transition(&a, &c, &not_held)]),
        )
        .await;
        let (editor_id, cookies) = editor(app, &[&held]).await;
        let item = item_on(app, a.id, editor_id).await;

        let (status, html) = get(app, &format!("/admin/content/{}/edit", item.id), &cookies).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("name=\"_stage\""), "the selector is offered");
        assert!(
            html.contains(&format!("value=\"{}\"", b.id)),
            "a reachable stage is listed"
        );
        assert!(
            !html.contains(&format!("value=\"{}\"", c.id)),
            "a stage whose transition the user does not hold is not listed"
        );

        // An editor who may reach nothing gets no selector at all.
        let (other_id, other_cookies) = editor(app, &[]).await;
        let other_item = item_on(app, a.id, other_id).await;
        let (_, html) = get(
            app,
            &format!("/admin/content/{}/edit", other_item.id),
            &other_cookies,
        )
        .await;
        assert!(
            !html.contains("name=\"_stage\""),
            "no reachable stage, no selector"
        );
    });
}

// =============================================================================
// Refusals: the form, the bulk action, save-item
// =============================================================================

/// Without the transition's permission, the edit form refuses the move.
#[test]
fn a_user_without_the_permission_is_refused_through_the_form() {
    run_test(async {
        let app = app();
        site_wide().await;
        let (from, to) = (stage(app, "b69rf").await, stage(app, "b69rt").await);
        set_workflow(
            app,
            json!([transition(&from, &to, &format!("b69 move {}", unique()))]),
        )
        .await;
        let (editor_id, cookies) = editor(app, &[]).await;
        let item = item_on(app, from.id, editor_id).await;

        let status = move_with_the_form(app, &cookies, &item, to.id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "the move must be refused");
        assert_eq!(
            stage_of(app, item.id).await,
            from.id,
            "the item did not move"
        );
    });
}

/// No transition is implied by another: holding a -> b and b -> c does not
/// allow a -> c.
#[test]
fn no_transition_is_implied_by_another() {
    run_test(async {
        let app = app();
        site_wide().await;
        let a = stage(app, "b69ia").await;
        let b = stage(app, "b69ib").await;
        let c = stage(app, "b69ic").await;
        let permission = format!("b69 move {}", unique());
        set_workflow(
            app,
            json!([
                transition(&a, &b, &permission),
                transition(&b, &c, &permission)
            ]),
        )
        .await;
        let (editor_id, cookies) = editor(app, &[&permission]).await;
        let item = item_on(app, a.id, editor_id).await;

        let status = move_with_the_form(app, &cookies, &item, c.id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a -> c is not a transition");
        assert_eq!(stage_of(app, item.id).await, a.id);

        // And b -> a, the reverse of a granted transition, is not one either.
        let back = item_on(app, b.id, editor_id).await;
        let status = move_with_the_form(app, &cookies, &back, a.id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "b -> a is not a transition");
        assert_eq!(stage_of(app, back.id).await, b.id);
    });
}

/// The bulk action decides each item: refused without the permission, done
/// with it.
#[test]
fn the_bulk_action_moves_only_what_the_user_may_move() {
    run_test(async {
        let app = app();
        site_wide().await;
        let (from, to) = (stage(app, "b69bf").await, stage(app, "b69bt").await);
        let permission = format!("b69 move {}", unique());
        set_workflow(app, json!([transition(&from, &to, &permission)])).await;

        let (without_id, without) = editor(app, &[]).await;
        let refused = item_on(app, from.id, without_id).await;
        move_with_the_bulk_action(app, &without, &[refused.id], to.id).await;
        assert_eq!(
            stage_of(app, refused.id).await,
            from.id,
            "a user without the transition's permission moves nothing"
        );

        let (with_id, with) = editor(app, &[&permission]).await;
        let moved = item_on(app, from.id, with_id).await;
        let status = move_with_the_bulk_action(app, &with, &[moved.id], to.id).await;
        assert!(status.is_redirection(), "the bulk action answers {status}");
        assert_eq!(
            stage_of(app, moved.id).await,
            to.id,
            "a user holding it moves the item"
        );
    });
}

/// `save-item` accepts a stage machine name and decides it by the same rule.
#[test]
fn save_item_moves_by_the_same_rule() {
    run_test(async {
        let app = app();
        site_wide().await;
        let (from, to) = (stage(app, "b69sf").await, stage(app, "b69sv").await);
        let permission = format!("b69 move {}", unique());
        set_workflow(app, json!([transition(&from, &to, &permission)])).await;
        let (editor_id, _) = editor(app, &[]).await;
        let item = item_on(app, from.id, editor_id).await;
        let payload = json!({ "id": item.id.to_string(), "stage": to.machine_name });

        let code = save_item_as(
            app,
            context_of(editor_id, &["edit any content", "access content"]),
            payload.clone(),
        )
        .await;
        assert_eq!(
            code,
            host_errors::ERR_ITEM_ACCESS_DENIED,
            "a plugin acting for a user without the permission is refused"
        );
        assert_eq!(stage_of(app, item.id).await, from.id);

        let code = save_item_as(
            app,
            context_of(
                editor_id,
                &["edit any content", "access content", &permission],
            ),
            payload,
        )
        .await;
        assert!(code >= 0, "with the permission the move succeeds: {code}");
        assert_eq!(stage_of(app, item.id).await, to.id);
    });
}

/// With no workflow configured, only an administrator moves items.
#[test]
fn with_no_workflow_only_an_administrator_moves_items() {
    run_test(async {
        let app = app();
        site_wide().await;
        let (from, to) = (stage(app, "b69nf").await, stage(app, "b69nt").await);
        clear_workflow(app).await;

        let (editor_id, cookies) = editor(app, &[]).await;
        let item = item_on(app, from.id, editor_id).await;
        let status = move_with_the_form(app, &cookies, &item, to.id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "no workflow, no move");
        assert_eq!(stage_of(app, item.id).await, from.id);

        let name = common::username("b69adm");
        let admin_cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let status = move_with_the_form(app, &admin_cookies, &item, to.id).await;
        assert!(
            status.is_redirection(),
            "an administrator moves it, got {status}"
        );
        assert_eq!(stage_of(app, item.id).await, to.id);
    });
}

// =============================================================================
// What moves with the item
// =============================================================================

/// The item's URL alias and menu link on the source stage move to the target.
#[test]
fn the_items_alias_and_menu_link_move_with_it() {
    run_test(async {
        let app = app();
        site_wide().await;
        let (from, to) = (stage(app, "b69af").await, stage(app, "b69at").await);
        let permission = format!("b69 move {}", unique());
        set_workflow(app, json!([transition(&from, &to, &permission)])).await;
        let (editor_id, cookies) = editor(app, &[&permission]).await;
        let item = item_on(app, from.id, editor_id).await;
        let source = format!("/item/{}", item.id);
        let now = chrono::Utc::now().timestamp();

        let alias_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO url_alias (id, source, alias, language, stage_id, created) \
             VALUES ($1, $2, $3, 'en', $4, $5)",
        )
        .bind(alias_id)
        .bind(&source)
        .bind(format!("/b69-alias-{}", unique()))
        .bind(from.id)
        .bind(now)
        .execute(&app.db)
        .await
        .expect("insert alias");
        let link_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO menu_link (id, menu_name, path, title, stage_id, created, changed) \
             VALUES ($1, 'main', $2, 'b69 link', $3, $4, $4)",
        )
        .bind(link_id)
        .bind(&source)
        .bind(from.id)
        .bind(now)
        .execute(&app.db)
        .await
        .expect("insert menu link");

        let status = move_with_the_form(app, &cookies, &item, to.id).await;
        assert!(status.is_redirection(), "the move saves, got {status}");

        let alias_stage: Uuid = sqlx::query_scalar("SELECT stage_id FROM url_alias WHERE id = $1")
            .bind(alias_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let link_stage: Uuid = sqlx::query_scalar("SELECT stage_id FROM menu_link WHERE id = $1")
            .bind(link_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(alias_stage, to.id, "the alias moved with the item");
        assert_eq!(link_stage, to.id, "the menu link moved with the item");
    });
}

// =============================================================================
// check_access fails closed on a missing stage
// =============================================================================

/// An item whose stage row is gone is denied, not treated as public.
#[test]
fn an_item_whose_stage_row_is_gone_is_denied() {
    run_test(async {
        let app = app();
        site_wide().await;
        let orphaned = stage(app, "b69gone").await;
        let (viewer_id, _) = editor(app, &[]).await;
        let item = item_on(app, orphaned.id, viewer_id).await;

        // The tag stays (the item's foreign key points at it); the stage
        // configuration that makes it a stage is gone.
        sqlx::query("DELETE FROM stage_config WHERE tag_id = $1")
            .bind(orphaned.id)
            .execute(&app.db)
            .await
            .expect("remove the stage row");

        let viewer = context_of(viewer_id, &["access content", "edit any content"]);
        let allowed = app
            .state
            .items()
            .check_access(&item, "view", &viewer)
            .await
            .expect("decide");
        assert!(
            !allowed,
            "an item on a stage that cannot be found is denied"
        );
    });
}

// =============================================================================
// An author on an internal default stage
// =============================================================================

/// An author whose new item lands on an internal default stage can still open
/// its edit form and save it, with no plugin enabled.
#[test]
fn an_author_on_an_internal_default_stage_can_open_and_edit_their_item() {
    run_test(async {
        let app = app();
        site_wide().await;
        let internal = stage(app, "b69author").await;
        {
            let pool = app.db.clone();
            common::defer_cleanup(move || async move {
                let _ = Stage::update(
                    &pool,
                    LIVE_STAGE_ID,
                    UpdateStage {
                        label: None,
                        machine_name: None,
                        description: None,
                        visibility: None,
                        is_default: Some(true),
                        weight: None,
                    },
                )
                .await;
            });
        }
        Stage::update(
            &app.db,
            internal.id,
            UpdateStage {
                label: None,
                machine_name: None,
                description: None,
                visibility: None,
                is_default: Some(true),
                weight: None,
            },
        )
        .await
        .expect("mark the internal stage default");

        let permissions = ["access content", "create page content", "edit own content"];
        let (author_id, cookies) = common::user_holding(app, "b69au", &permissions).await;
        let author = context_of(author_id, &permissions);
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    title: format!("b69 author {}", unique()),
                    author_id,
                    status: Some(0),
                    promote: None,
                    sticky: None,
                    fields: None,
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &author,
            )
            .await
            .expect("the author creates");
        let id = item.id;
        {
            let pool = app.db.clone();
            common::defer_cleanup(move || async move {
                let _ = Item::delete(&pool, id).await;
            });
        }
        assert_eq!(
            item.stage_id, internal.id,
            "it landed on the internal stage"
        );

        let (status, _) = get(app, &format!("/item/{id}/edit"), &cookies).await;
        assert_eq!(status, StatusCode::OK, "the author opens the edit form");
        assert!(
            app.state
                .items()
                .check_access(&item, "edit", &author)
                .await
                .expect("decide"),
            "the author may edit it"
        );
    });
}
