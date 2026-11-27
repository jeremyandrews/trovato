#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Configurable moderation workflows (B301).
//!
//! The states and transitions a moderated type's items move through used to
//! be the three constants `content::moderation` compiled in (Draft,
//! Published, Archived) and a fixed five-transition table: a site could not
//! add a review step without a kernel change, and the database refused any
//! other state by a check constraint. States, transitions and permissions now
//! come from one site configuration variable, `moderation.workflows`, one
//! workflow per moderated type; the shipped default is exactly the old
//! Editorial workflow, under the same permission names, so an existing site
//! sees no change (`content_moderation_test.rs`, which this file does not
//! touch, is the proof).
//!
//! This file's `review` workflow (draft, in_review, approved, published,
//! archived) exercises a workflow with more than one non-publishing state,
//! which the built-in Editorial workflow never had: a forward revision,
//! permission derivation and the moderated list's filter all have to work for
//! more than "Draft".
//!
//! `TestApp`-backed tests (write path, forms, lists) share the process-wide
//! app; the `moderation.workflows` variable is site-wide, so every test that
//! touches it holds `SITE_WIDE` and restores what it changed, as
//! `stage_workflow_test.rs` does for `workflow.editorial`. Config-import
//! validation tests use their own scratch database instead, as
//! `config_import_test.rs` does, so they can assert on a clean import set.

mod common;

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for, username};
use serde_json::json;
use sqlx::PgPool;
use trovato_kernel::config_storage::yaml::{ConfigImportFailed, export_config, import_config};
use trovato_kernel::config_storage::{ConfigStorage, DirectConfigStorage, entity_types};
use trovato_kernel::content::WriteDenied;
use trovato_kernel::content::moderation::{self, ModerationWorkflow};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, Item, SiteConfig, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const WORKFLOWS_KEY: &str = moderation::WORKFLOWS_KEY;
const TYPE: &str = "b301_review";
const DEFAULT_TYPE: &str = "b301_default";

const SUBMITTER: &[&str] = &[
    "access content",
    "create b301_review content",
    "edit own content",
    "view own content",
    "use review transition start_draft",
    "use review transition submit",
];
const APPROVER: &[&str] = &[
    "access content",
    "edit any content",
    "view any content",
    "use review transition approve",
];
const PUBLISHER: &[&str] = &[
    "access content",
    "edit any content",
    "view any content",
    "use review transition publish",
];
const EDITOR: &[&str] = &[
    "access content",
    "access administration pages",
    "edit any content",
    "view any content",
    "view latest version",
    "use review transition start_draft",
    "use review transition submit",
    "use review transition approve",
    "use review transition publish",
    "use review transition revise",
];

/// The fixture workflow: five states, more than one of which is not a
/// published state and not the default revision (draft and in_review and
/// approved all wait beside a published item rather than replacing it).
fn review_workflow() -> ModerationWorkflow {
    serde_json::from_value(json!({
        "id": "review",
        "label": "Review",
        "default_state": "draft",
        "states": [
            {"id": "draft", "label": "Draft", "published": false, "default_revision": false},
            {"id": "in_review", "label": "In review", "published": false, "default_revision": false},
            {"id": "approved", "label": "Approved", "published": false, "default_revision": false},
            {"id": "published", "label": "Published", "published": true, "default_revision": true},
            {"id": "archived", "label": "Archived", "published": false, "default_revision": true}
        ],
        "transitions": [
            {"id": "start_draft", "label": "Start Draft", "from": ["draft"], "to": "draft"},
            {"id": "submit", "label": "Submit for review", "from": ["draft"], "to": "in_review"},
            {"id": "approve", "label": "Approve", "from": ["in_review"], "to": "approved"},
            {"id": "publish", "label": "Publish", "from": ["approved"], "to": "published"},
            {"id": "archive", "label": "Archive", "from": ["published"], "to": "archived"},
            {"id": "revise", "label": "Revise", "from": ["published"], "to": "in_review"}
        ]
    }))
    .expect("the fixture workflow must parse")
}

// ---------------------------------------------------------------------------
// Site-wide `moderation.workflows`, held and restored like `workflow.editorial`
// in stage_workflow_test.rs: every test here holds this lock.
// ---------------------------------------------------------------------------

static SITE_WIDE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn site_wide() {
    common::hold_until_cleanup(SITE_WIDE.lock().await);
}

/// Add the `review` workflow beside whatever `moderation.workflows` already
/// holds (the built-in editorial workflow the migration seeds, on an
/// otherwise untouched database), and put the previous value back when the
/// test ends.
async fn install_review_workflow(app: &TestApp) {
    site_wide().await;
    let previous = SiteConfig::get(&app.db, WORKFLOWS_KEY)
        .await
        .expect("read moderation.workflows");
    let pool = app.db.clone();
    let restore = previous.clone();
    common::defer_cleanup(move || async move {
        match restore {
            Some(v) => {
                let _ = SiteConfig::set(&pool, WORKFLOWS_KEY, v).await;
            }
            None => {
                let _ = SiteConfig::delete(&pool, WORKFLOWS_KEY).await;
            }
        }
    });
    let mut workflows = moderation::resolve_workflows(previous.as_ref());
    workflows.retain(|w| w.id != "review");
    workflows.push(review_workflow());
    SiteConfig::set(&app.db, WORKFLOWS_KEY, json!({ "workflows": workflows }))
        .await
        .expect("store moderation.workflows");
}

async fn ensure_types(app: &TestApp) {
    for (name, extra) in [
        (TYPE, json!({"moderation_workflow": "review"})),
        (DEFAULT_TYPE, json!({})),
    ] {
        let mut settings = json!({ "fields": [], "moderation": true });
        settings
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $1, '', true, 'Title', 'b301_test', $2) ON CONFLICT (type) DO UPDATE SET settings = $2",
        )
        .bind(name)
        .bind(&settings)
        .execute(&app.db)
        .await
        .unwrap();
        app.state
            .content_types()
            .replace(name, name, None, settings)
            .await
            .ok();
    }
}

fn user(id: Uuid, permissions: &[&str]) -> UserContext {
    UserContext::authenticated(id, permissions.iter().map(|p| (*p).to_string()).collect())
}

async fn real(app: &TestApp, prefix: &str, permissions: &[&str]) -> UserContext {
    let (id, _) = common::user_holding(app, prefix, permissions).await;
    user(id, permissions)
}

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec![])
}

fn anonymous() -> UserContext {
    let mut visitor = UserContext::anonymous();
    visitor.permissions = vec!["access content".to_string()];
    visitor
}

fn forget(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

fn new_item(item_type: &str, title: &str, author: &UserContext) -> CreateItem {
    CreateItem {
        item_type: item_type.to_string(),
        title: title.to_string(),
        author_id: author.id,
        status: None,
        promote: None,
        sticky: None,
        fields: Some(json!({})),
        stage_id: Some(LIVE_STAGE_ID),
        language: Some("en".to_string()),
        log: None,
    }
}

fn edit(title: Option<&str>) -> UpdateItem {
    UpdateItem {
        title: title.map(str::to_string),
        status: None,
        promote: None,
        sticky: None,
        fields: None,
        log: None,
        stage_id: None,
        author_id: None,
        created: None,
    }
}

async fn create(
    app: &TestApp,
    item_type: &str,
    title: &str,
    by: &UserContext,
    state: Option<&str>,
) -> anyhow::Result<Item> {
    let item = app
        .state
        .items()
        .create_moderated(new_item(item_type, title, by), None, state, by)
        .await?;
    forget(app, item.id);
    Ok(item)
}

async fn moderate(
    app: &TestApp,
    id: Uuid,
    by: &UserContext,
    title: Option<&str>,
    state: Option<&str>,
) -> anyhow::Result<Item> {
    app.state.items().invalidate(id);
    Ok(app
        .state
        .items()
        .update_moderated(id, edit(title), state, by)
        .await?
        .expect("the item"))
}

async fn row(app: &TestApp, id: Uuid) -> (String, i16) {
    sqlx::query_as("SELECT title, status FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn latest(app: &TestApp, id: Uuid) -> (String, Option<String>) {
    sqlx::query_as(
        "SELECT title, moderation_state FROM item_revision WHERE item_id = $1 \
         ORDER BY created DESC, id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await
    .unwrap()
}

fn refused(result: anyhow::Result<Item>, from: &str, to: &str) {
    let error = result.expect_err("the transition should be refused");
    assert_eq!(
        error.downcast_ref::<WriteDenied>(),
        Some(&WriteDenied::Transition {
            from: from.to_string(),
            to: to.to_string()
        }),
        "{error}"
    );
}

async fn visible_to_anonymous(app: &TestApp, id: Uuid) -> bool {
    app.state.items().invalidate(id);
    let item = app.state.items().load(id).await.unwrap().unwrap();
    app.state
        .items()
        .check_access(&item, "view", &anonymous())
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// (a) + (b): a new item starts in draft; transitions are gated per permission.
// ---------------------------------------------------------------------------

#[test]
fn a_new_item_starts_in_draft_and_transitions_are_gated_per_permission() {
    run_test(async {
        let app = shared_app().await;
        install_review_workflow(app).await;
        ensure_types(app).await;

        let submitter = real(app, "b301submit", SUBMITTER).await;
        let approver = real(app, "b301approve", APPROVER).await;
        let publisher = real(app, "b301publish", PUBLISHER).await;

        // (a) A new item starts on the workflow's default state.
        let item = create(app, TYPE, "Review me", &submitter, None)
            .await
            .unwrap();
        assert_eq!(item.status, 0);
        assert_eq!(latest(app, item.id).await.1.as_deref(), Some("draft"));

        // (b) The submitter holds only `submit`: draft -> in_review succeeds,
        // in_review -> approved is refused.
        moderate(app, item.id, &submitter, None, Some("in_review"))
            .await
            .unwrap();
        assert_eq!(latest(app, item.id).await.1.as_deref(), Some("in_review"));
        refused(
            moderate(app, item.id, &submitter, None, Some("approved")).await,
            "in_review",
            "approved",
        );

        // The approver's transition moves it on.
        moderate(app, item.id, &approver, None, Some("approved"))
            .await
            .unwrap();
        assert_eq!(latest(app, item.id).await.1.as_deref(), Some("approved"));

        // The publisher's transition publishes it: status 1, visible to an
        // anonymous visitor.
        moderate(app, item.id, &publisher, None, Some("published"))
            .await
            .unwrap();
        assert_eq!(row(app, item.id).await.1, 1);
        assert!(visible_to_anonymous(app, item.id).await);
    });
}

// ---------------------------------------------------------------------------
// (c) A save to a non-default-revision state over a published item is a
// forward revision.
// ---------------------------------------------------------------------------

#[test]
fn a_save_to_in_review_over_a_published_item_is_a_forward_revision() {
    run_test(async {
        let app = shared_app().await;
        install_review_workflow(app).await;
        ensure_types(app).await;

        let submitter = real(app, "b301submit", SUBMITTER).await;
        let approver = real(app, "b301approve", APPROVER).await;
        let publisher = real(app, "b301publish", PUBLISHER).await;
        let editor = real(app, "b301edit", EDITOR).await;

        let item = create(app, TYPE, "Live title", &submitter, None)
            .await
            .unwrap();
        moderate(app, item.id, &submitter, None, Some("in_review"))
            .await
            .unwrap();
        moderate(app, item.id, &approver, None, Some("approved"))
            .await
            .unwrap();
        moderate(app, item.id, &publisher, None, Some("published"))
            .await
            .unwrap();
        assert_eq!(row(app, item.id).await, ("Live title".to_string(), 1));

        let returned = moderate(
            app,
            item.id,
            &editor,
            Some("Ahead title"),
            Some("in_review"),
        )
        .await
        .unwrap();
        assert_eq!(
            returned.title, "Live title",
            "the item row stays the published version"
        );
        assert_eq!(row(app, item.id).await, ("Live title".to_string(), 1));
        assert_eq!(
            latest(app, item.id).await,
            ("Ahead title".to_string(), Some("in_review".to_string()))
        );
        assert!(
            visible_to_anonymous(app, item.id).await,
            "an anonymous visitor still sees the published version"
        );
        app.state.items().invalidate(item.id);
        let stored = app.state.items().load(item.id).await.unwrap().unwrap();
        assert_eq!(stored.title, "Live title");
        let (state, working) = app
            .state
            .items()
            .load_for_moderation(&stored)
            .await
            .unwrap()
            .expect("a moderated type");
        assert_eq!(state, "in_review");
        assert_eq!(working.title, "Ahead title");
    });
}

// ---------------------------------------------------------------------------
// (d) A background save of `{"status": 1}` (what scheduled publishing sends):
// publishes from a state with a transition to a published state, refused
// where there is none.
// ---------------------------------------------------------------------------

#[test]
fn a_background_status_only_save_follows_the_workflows_transitions() {
    run_test(async {
        let app = shared_app().await;
        install_review_workflow(app).await;
        ensure_types(app).await;

        let submitter = real(app, "b301submit", SUBMITTER).await;
        let approver = real(app, "b301approve", APPROVER).await;
        let background = UserContext::background();

        // Approved -> published: there is a transition, and the background
        // principal always holds it.
        let item = create(app, TYPE, "Scheduled", &submitter, None)
            .await
            .unwrap();
        moderate(app, item.id, &submitter, None, Some("in_review"))
            .await
            .unwrap();
        moderate(app, item.id, &approver, None, Some("approved"))
            .await
            .unwrap();
        let mut publish = edit(None);
        publish.status = Some(1);
        app.state.items().invalidate(item.id);
        let published = app
            .state
            .items()
            .update_moderated(item.id, publish, None, &background)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(published.status, 1);
        assert_eq!(latest(app, item.id).await.1.as_deref(), Some("published"));

        // Draft -> published: the workflow has no transition that reaches a
        // published state directly from draft (publishing is a pipeline:
        // submit, approve, then publish), so a bare status save stays on
        // draft rather than skipping the pipeline; the item is never
        // published by this save.
        let item2 = create(app, TYPE, "Scheduled too soon", &submitter, None)
            .await
            .unwrap();
        let mut publish2 = edit(None);
        publish2.status = Some(1);
        app.state.items().invalidate(item2.id);
        let _ = app
            .state
            .items()
            .update_moderated(item2.id, publish2, None, &background)
            .await;
        assert_eq!(
            row(app, item2.id).await.1,
            0,
            "a background save must never publish an item with no path from its state"
        );
        assert_eq!(latest(app, item2.id).await.1.as_deref(), Some("draft"));
    });
}

// ---------------------------------------------------------------------------
// (e) The item form, the moderated list and the revision list all read the
// real workflow's labels and targets.
// ---------------------------------------------------------------------------

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get(app: &TestApp, cookies: &str, uri: &str, ip: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

/// The `moderation_state` select's option labels (not values), in order.
fn moderation_option_labels(html: &str) -> Vec<String> {
    let Some(at) = html.find(r#"name="moderation_state""#) else {
        return Vec::new();
    };
    let end = at + html[at..].find("</select>").unwrap();
    html[at..end]
        .split("<option")
        .skip(1)
        .filter_map(|o| {
            let gt = o.find('>')? + 1;
            let close = o[gt..].find("</option>")?;
            Some(o[gt..gt + close].trim().to_string())
        })
        .collect()
}

#[test]
fn the_form_list_and_revisions_show_the_real_workflows_labels() {
    run_test(async {
        let app = shared_app().await;
        install_review_workflow(app).await;
        ensure_types(app).await;

        let (submitter_id, submitter_cookies) =
            common::user_holding(app, "b301formsub", SUBMITTER).await;
        let (_, editor_cookies) = common::user_holding(app, "b301formedit", EDITOR).await;
        let ip = test_ip_for("b301form");
        let submitter = user(submitter_id, SUBMITTER);
        let approver = real(app, "b301formapp", APPROVER).await;

        let item = create(app, TYPE, "Form probe", &submitter, None)
            .await
            .unwrap();
        moderate(app, item.id, &submitter, None, Some("in_review"))
            .await
            .unwrap();

        // The submitter's edit form offers no further state (they hold only
        // `submit`, already used).
        let edit_path = format!("/item/{}/edit", item.id);
        let (status, html) = get(app, &submitter_cookies, &edit_path, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Current state:") && html.contains("In review"));

        // The editor (submit + approve + publish) is offered Approved.
        let (_, html) = get(app, &editor_cookies, &edit_path, &ip).await;
        assert!(
            moderation_option_labels(&html).contains(&"Approved".to_string()),
            "{html}"
        );

        moderate(app, item.id, &approver, None, Some("approved"))
            .await
            .unwrap();

        // The moderated list, filtered to in_review on an earlier item, and
        // unfiltered showing both: the filter offers In review and Approved.
        let item2 = create(app, TYPE, "Second", &submitter, None).await.unwrap();
        moderate(app, item2.id, &submitter, None, Some("in_review"))
            .await
            .unwrap();
        let (status, html) = get(
            app,
            &editor_cookies,
            "/admin/content/moderated?state=in_review",
            &ip,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Second"), "{html}");
        assert!(!html.contains("Form probe"), "{html}");

        let (status, html) = get(app, &editor_cookies, "/admin/content/moderated", &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("In review"), "{html}");
        assert!(html.contains("Approved"), "{html}");

        // The revision list shows the state against the revision.
        let (status, html) = get(
            app,
            &editor_cookies,
            &format!("/item/{}/revisions", item.id),
            &ip,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Approved"), "{html}");
    });
}

// ---------------------------------------------------------------------------
// (i) A stage move and a draft-over-published save in the same request: the
// default (editorial) workflow refuses the combination rather than silently
// dropping the move.
// ---------------------------------------------------------------------------

#[test]
fn a_stage_move_with_a_forward_revision_is_refused() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let target_stage = common::create_test_stage(app, "b301stage").await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM stage_config WHERE tag_id = $1")
                .bind(target_stage)
                .execute(&pool)
                .await;
        });

        let admin = admin();
        let item = create(app, DEFAULT_TYPE, "Stage probe", &admin, None)
            .await
            .unwrap();
        moderate(app, item.id, &admin, None, Some("published"))
            .await
            .unwrap();

        app.state.items().invalidate(item.id);
        let mut input = edit(Some("Draft while moving"));
        input.stage_id = Some(target_stage);
        let result = app
            .state
            .items()
            .update_moderated(item.id, input, Some("draft"), &admin)
            .await;
        let error = result.expect_err("the combination must be refused");
        assert_eq!(
            error.downcast_ref::<WriteDenied>(),
            Some(&WriteDenied::StageMoveWithForwardRevision),
            "{error}"
        );
        // Neither the move nor the draft silently applied.
        assert_eq!(row(app, item.id).await, ("Stage probe".to_string(), 1));
    });
}

// ---------------------------------------------------------------------------
// (f), (g): config import validation, against a scratch database.
// ---------------------------------------------------------------------------

struct ScratchDb(trovato_test_utils::ScratchDb);

impl ScratchDb {
    async fn new(label: &str) -> Self {
        let db = trovato_test_utils::ScratchDb::create(&format!("modwf_{label}")).await;
        trovato_kernel::db::run_migrations(db.pool())
            .await
            .expect("failed to migrate the scratch database");
        Self(db)
    }

    fn pool(&self) -> &PgPool {
        self.0.pool()
    }

    fn storage(&self) -> DirectConfigStorage {
        DirectConfigStorage::new(self.pool().clone())
    }

    async fn cleanup(self) {
        drop(self);
    }
}

struct TempConfigDir(PathBuf);

impl TempConfigDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("trovato_modwf_{label}_{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, filename: &str, contents: &str) {
        std::fs::write(self.0.join(filename), contents).unwrap();
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempConfigDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const REVIEW_YAML: &str = r#"key: moderation.workflows
value: {"workflows": [{"id": "review", "label": "Review", "default_state": "draft", "states": [{"id": "draft", "label": "Draft", "published": false, "default_revision": false}, {"id": "in_review", "label": "In review", "published": false, "default_revision": false}, {"id": "approved", "label": "Approved", "published": false, "default_revision": false}, {"id": "published", "label": "Published", "published": true, "default_revision": true}, {"id": "archived", "label": "Archived", "published": false, "default_revision": true}], "transitions": [{"id": "start_draft", "label": "Start Draft", "from": ["draft"], "to": "draft"}, {"id": "submit", "label": "Submit for review", "from": ["draft"], "to": "in_review"}, {"id": "approve", "label": "Approve", "from": ["in_review"], "to": "approved"}, {"id": "publish", "label": "Publish", "from": ["approved"], "to": "published"}, {"id": "archive", "label": "Archive", "from": ["published"], "to": "archived"}, {"id": "revise", "label": "Revise", "from": ["published"], "to": "in_review"}]}]}
"#;

const REVIEW_TYPE_YAML: &str = r#"type: b301_cfg_review
label: Review Item
description: ""
has_title: true
title_label: Title
plugin: core
settings:
  fields: []
  moderation: true
  moderation_workflow: review
"#;

#[tokio::test]
async fn config_import_carries_a_workflow_and_its_types_assignment() {
    let db = ScratchDb::new("import").await;
    let storage = db.storage();
    let dir = TempConfigDir::new("import");
    dir.write("variable.moderation.workflows.yml", REVIEW_YAML);
    dir.write("item_type.b301_cfg_review.yml", REVIEW_TYPE_YAML);

    import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect("import must succeed");

    let stored = SiteConfig::get(db.pool(), WORKFLOWS_KEY)
        .await
        .expect("read")
        .expect("the variable was imported");
    let workflows = moderation::resolve_workflows(Some(&stored));
    assert!(workflows.iter().any(|w| w.id == "review"));

    let item_type = storage
        .load(entity_types::ITEM_TYPE, "b301_cfg_review")
        .await
        .expect("load")
        .expect("the type was imported");
    let _ = item_type;

    db.cleanup().await;
}

#[tokio::test]
async fn config_export_then_import_carries_the_workflow_unchanged() {
    let db = ScratchDb::new("roundtrip").await;
    let storage = db.storage();
    let dir = TempConfigDir::new("roundtrip_in");
    dir.write("variable.moderation.workflows.yml", REVIEW_YAML);
    dir.write("item_type.b301_cfg_review.yml", REVIEW_TYPE_YAML);
    import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect("import must succeed");

    let export_dir = TempConfigDir::new("roundtrip_out");
    export_config(&storage, db.pool(), export_dir.path(), false)
        .await
        .expect("export must succeed");

    let db2 = ScratchDb::new("roundtrip2").await;
    let storage2 = db2.storage();
    import_config(&storage2, db2.pool(), export_dir.path(), false)
        .await
        .expect("re-import of the export must succeed");

    let original = moderation::resolve_workflows(
        SiteConfig::get(db.pool(), WORKFLOWS_KEY)
            .await
            .unwrap()
            .as_ref(),
    );
    let round_tripped = moderation::resolve_workflows(
        SiteConfig::get(db2.pool(), WORKFLOWS_KEY)
            .await
            .unwrap()
            .as_ref(),
    );
    assert_eq!(original, round_tripped);

    db.cleanup().await;
    db2.cleanup().await;
}

async fn import_err(db: &ScratchDb, dir: &TempConfigDir) -> ConfigImportFailed {
    let storage = db.storage();
    let err = import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect_err("the import must fail");
    err.downcast::<ConfigImportFailed>()
        .expect("the error must be a ConfigImportFailed")
}

#[tokio::test]
async fn config_import_refuses_a_malformed_workflow() {
    let db = ScratchDb::new("badstate").await;

    // A transition naming an unknown state.
    let dir = TempConfigDir::new("badstate_transition");
    dir.write(
        "variable.moderation.workflows.yml",
        r#"key: moderation.workflows
value: {"workflows": [{"id": "r1", "label": "R", "default_state": "draft", "states": [{"id": "draft", "label": "Draft", "published": false, "default_revision": false}, {"id": "published", "label": "Published", "published": true, "default_revision": true}], "transitions": [{"id": "publish", "label": "Publish", "from": ["draft"], "to": "nowhere"}]}]}
"#,
    );
    let failed = import_err(&db, &dir).await;
    assert!(
        failed.failures.iter().any(|f| f.error.contains("nowhere")),
        "{failed}"
    );

    // A workflow with no published state.
    let dir = TempConfigDir::new("badstate_nopublished");
    dir.write(
        "variable.moderation.workflows.yml",
        r#"key: moderation.workflows
value: {"workflows": [{"id": "r2", "label": "R", "default_state": "draft", "states": [{"id": "draft", "label": "Draft", "published": false, "default_revision": false}], "transitions": []}]}
"#,
    );
    let failed = import_err(&db, &dir).await;
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.error.contains("no published state")),
        "{failed}"
    );

    // A default_state that is not a state.
    let dir = TempConfigDir::new("badstate_default");
    dir.write(
        "variable.moderation.workflows.yml",
        r#"key: moderation.workflows
value: {"workflows": [{"id": "r3", "label": "R", "default_state": "missing", "states": [{"id": "draft", "label": "Draft", "published": false, "default_revision": false}, {"id": "published", "label": "Published", "published": true, "default_revision": true}], "transitions": []}]}
"#,
    );
    let failed = import_err(&db, &dir).await;
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.error.contains("default_state")),
        "{failed}"
    );

    // A state id longer than 32 characters.
    let long_id = "a".repeat(33);
    let dir = TempConfigDir::new("badstate_long");
    dir.write(
        "variable.moderation.workflows.yml",
        &format!(
            r#"key: moderation.workflows
value: {{"workflows": [{{"id": "r4", "label": "R", "default_state": "{long_id}", "states": [{{"id": "{long_id}", "label": "Draft", "published": false, "default_revision": false}}, {{"id": "published", "label": "Published", "published": true, "default_revision": true}}], "transitions": []}}]}}
"#
        ),
    );
    let failed = import_err(&db, &dir).await;
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.error.contains("32 characters")),
        "{failed}"
    );

    // A state id not matching ^[a-z][a-z0-9_]*$.
    let dir = TempConfigDir::new("badstate_pattern");
    dir.write(
        "variable.moderation.workflows.yml",
        r#"key: moderation.workflows
value: {"workflows": [{"id": "r5", "label": "R", "default_state": "Draft-1", "states": [{"id": "Draft-1", "label": "Draft", "published": false, "default_revision": false}, {"id": "published", "label": "Published", "published": true, "default_revision": true}], "transitions": []}]}
"#,
    );
    let failed = import_err(&db, &dir).await;
    assert!(
        failed.failures.iter().any(|f| f.error.contains("a-z0-9_")),
        "{failed}"
    );

    // Two workflows disagreeing about the same state id's flags.
    let dir = TempConfigDir::new("badstate_disagree");
    dir.write(
        "variable.moderation.workflows.yml",
        r#"key: moderation.workflows
value: {"workflows": [{"id": "r6a", "label": "R", "default_state": "draft", "states": [{"id": "draft", "label": "Draft", "published": false, "default_revision": false}, {"id": "published", "label": "Published", "published": true, "default_revision": true}], "transitions": []}, {"id": "r6b", "label": "R", "default_state": "draft", "states": [{"id": "draft", "label": "Draft", "published": true, "default_revision": true}, {"id": "published", "label": "Published", "published": true, "default_revision": true}], "transitions": []}]}
"#,
    );
    let failed = import_err(&db, &dir).await;
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.error.contains("mean the same thing everywhere")),
        "{failed}"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn config_import_refuses_a_type_naming_an_unknown_workflow() {
    let db = ScratchDb::new("unknownwf").await;
    let dir = TempConfigDir::new("unknownwf");
    dir.write(
        "item_type.b301_cfg_unknown.yml",
        r#"type: b301_cfg_unknown
label: Unknown workflow
description: ""
has_title: true
title_label: Title
plugin: core
settings:
  fields: []
  moderation: true
  moderation_workflow: nonexistent
"#,
    );
    let failed = import_err(&db, &dir).await;
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.error.contains("nonexistent") && f.error.contains("does not exist")),
        "{failed}"
    );
    db.cleanup().await;
}

#[tokio::test]
async fn config_import_refuses_removing_a_state_a_stored_revision_still_holds() {
    let db = ScratchDb::new("removedstate").await;
    let storage = db.storage();

    // Import the review workflow and its type.
    let dir = TempConfigDir::new("removedstate_in");
    dir.write("variable.moderation.workflows.yml", REVIEW_YAML);
    dir.write("item_type.b301_cfg_review.yml", REVIEW_TYPE_YAML);
    import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect("import must succeed");

    // A stored item and a revision in `in_review`, as a real save would leave.
    let item_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, fields, stage_id, language) \
         VALUES ($1, 'b301_cfg_review', 'Held', $2, 0, 0, 0, '{}', $3, 'en')",
    )
    .bind(item_id)
    .bind(Uuid::nil())
    .bind(trovato_kernel::models::stage::LIVE_STAGE_ID)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, moderation_state) \
         VALUES ($1, $2, $3, 'Held', 0, '{}', 0, 'in_review')",
    )
    .bind(revision_id)
    .bind(item_id)
    .bind(Uuid::nil())
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE item SET current_revision_id = $1 WHERE id = $2")
        .bind(revision_id)
        .bind(item_id)
        .execute(db.pool())
        .await
        .unwrap();

    // Re-import the same workflow with `in_review` (and everything that
    // names it) removed.
    let dir2 = TempConfigDir::new("removedstate_out");
    dir2.write(
        "variable.moderation.workflows.yml",
        r#"key: moderation.workflows
value: {"workflows": [{"id": "review", "label": "Review", "default_state": "draft", "states": [{"id": "draft", "label": "Draft", "published": false, "default_revision": false}, {"id": "approved", "label": "Approved", "published": false, "default_revision": false}, {"id": "published", "label": "Published", "published": true, "default_revision": true}, {"id": "archived", "label": "Archived", "published": false, "default_revision": true}], "transitions": [{"id": "publish", "label": "Publish", "from": ["approved"], "to": "published"}, {"id": "archive", "label": "Archive", "from": ["published"], "to": "archived"}]}]}
"#,
    );
    let failed = import_err(&db, &dir2).await;
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.error.contains("in_review") && f.error.contains("still hold")),
        "{failed}"
    );

    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// (h) A role file granting a derived transition permission imports, and the
// permission grid lists it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_import_accepts_a_role_granting_a_workflow_transition() {
    let db = ScratchDb::new("rolegrant").await;
    let storage = db.storage();
    let dir = TempConfigDir::new("rolegrant");
    dir.write("variable.moderation.workflows.yml", REVIEW_YAML);
    let role_id = Uuid::now_v7();
    dir.write(
        &format!("role.{role_id}.yml"),
        &format!(
            "id: {role_id}\nname: b301_reviewer\ncreated: \"2026-01-01T00:00:00Z\"\npermissions:\n- use review transition approve\n"
        ),
    );
    import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect("a role granting a workflow transition permission must import");

    let granted: Vec<String> =
        sqlx::query_scalar("SELECT permission FROM role_permissions WHERE role_id = $1")
            .bind(role_id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(granted.iter().any(|p| p == "use review transition approve"));

    db.cleanup().await;
}

#[test]
fn the_permission_grid_lists_a_stored_workflows_transitions() {
    run_test(async {
        let app = shared_app().await;
        install_review_workflow(app).await;

        let name = username("b301-grid-admin");
        app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;

        let (status, html) = get(
            app,
            &cookies,
            "/admin/people/permissions",
            &test_ip_for("b301grid"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("use review transition approve"),
            "the grid must list the stored workflow's derived permission"
        );
    });
}
