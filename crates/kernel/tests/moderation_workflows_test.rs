#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Configurable moderation workflows (B301).
//!
//! The Draft/Published/Archived workflow used to be compiled into the kernel
//! (`content::moderation`'s `STATES` and `TRANSITIONS` constants) and enforced
//! by a database check (`item_revision_moderation_state_check`), so a site
//! that wanted a review step between Draft and Published had no way to add
//! one. A workflow is now one site configuration variable,
//! `moderation.workflows`, holding any number of named workflows, each with
//! its own states (a `published` flag and a `default_revision` flag) and
//! transitions (each its own `use <workflow id> transition <id>` permission);
//! a content type's `moderation_workflow` setting picks one, defaulting to
//! the built-in `editorial` workflow so an existing site sees no change.
//!
//! `content_moderation_test.rs` is the proof the default workflow behaves
//! exactly as it always did; this file is the proof a custom one works. Some
//! scenarios need a real `config import` (a fresh database, `ScratchDb`,
//! copying `config_import_test.rs`'s pattern); the write-path and page
//! scenarios reuse `shared_app()` and `content_moderation_test.rs`'s helpers,
//! setting `moderation.workflows` directly with `SiteConfig::set` as
//! `stage_workflow_test.rs` sets `workflow.editorial`, under the same
//! site-wide lock, since the variable is global.

mod common;

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use serde_json::json;
use sqlx::PgPool;
use trovato_kernel::config_storage::yaml::{ConfigImportFailed, import_config};
use trovato_kernel::config_storage::{ConfigStorage, DirectConfigStorage};
use trovato_kernel::content::WriteDenied;
use trovato_kernel::content::moderation;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, Item, SiteConfig, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE: &str = "b301_review";
const WORKFLOWS_KEY: &str = moderation::WORKFLOWS_KEY;

/// Serializes every test in this file: `moderation.workflows` is global.
static SITE_WIDE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn site_wide() {
    common::hold_until_cleanup(SITE_WIDE.lock().await);
}

/// The five-state `review` workflow this file tests against: draft, in
/// review, approved, published, archived.
fn review_workflow_value() -> serde_json::Value {
    json!({
        "workflows": [{
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
                {"id": "create", "label": "Start a draft", "from": ["draft"], "to": "draft"},
                {"id": "submit", "label": "Submit for review", "from": ["draft", "published"], "to": "in_review"},
                {"id": "approve", "label": "Approve", "from": ["in_review"], "to": "approved"},
                {"id": "publish", "label": "Publish", "from": ["approved"], "to": "published"},
                {"id": "archive", "label": "Archive", "from": ["published"], "to": "archived"},
                {"id": "restore", "label": "Restore", "from": ["archived"], "to": "published"}
            ]
        }]
    })
}

async fn install_review_workflow(app: &TestApp) -> Option<serde_json::Value> {
    let previous = SiteConfig::get(&app.db, WORKFLOWS_KEY).await.unwrap();
    SiteConfig::set(&app.db, WORKFLOWS_KEY, review_workflow_value())
        .await
        .unwrap();
    previous
}

async fn restore_workflow(app: &TestApp, previous: Option<serde_json::Value>) {
    match previous {
        Some(v) => {
            let _ = SiteConfig::set(&app.db, WORKFLOWS_KEY, v).await;
        }
        None => {
            let _ = SiteConfig::delete(&app.db, WORKFLOWS_KEY).await;
        }
    }
}

async fn ensure_review_type(app: &TestApp) {
    let settings = json!({"fields": [], "moderation": true, "moderation_workflow": "review"});
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'b301_test', $2) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, TYPE, None, settings)
        .await
        .ok();
}

fn user(id: Uuid, permissions: &[&str]) -> UserContext {
    UserContext::authenticated(id, permissions.iter().map(|p| (*p).to_string()).collect())
}

async fn real(app: &TestApp, prefix: &str, permissions: &[&str]) -> UserContext {
    let (id, _) = common::user_holding(app, prefix, permissions).await;
    user(id, permissions)
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

fn new_item(title: &str, author: &UserContext) -> CreateItem {
    CreateItem {
        item_type: TYPE.to_string(),
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

async fn row(app: &TestApp, id: Uuid) -> (String, i16) {
    sqlx::query_as("SELECT title, status FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
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

fn refused_transition(result: anyhow::Result<Item>, from: &str, to: &str) {
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

// ---------------------------------------------------------------------------
// (a) and (f): config import
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
}

struct TempConfigDir(PathBuf);

impl TempConfigDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "trovato_modwf_{label}_{}",
            Uuid::now_v7().simple()
        ));
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

const REVIEW_WORKFLOW_YAML: &str = r#"
key: moderation.workflows
value:
  workflows:
    - id: review
      label: Review
      default_state: draft
      states:
        - {id: draft, label: Draft, published: false, default_revision: false}
        - {id: in_review, label: "In review", published: false, default_revision: false}
        - {id: approved, label: Approved, published: false, default_revision: false}
        - {id: published, label: Published, published: true, default_revision: true}
        - {id: archived, label: Archived, published: false, default_revision: true}
      transitions:
        - {id: create, label: "Start a draft", from: [draft], to: draft}
        - {id: submit, label: "Submit for review", from: [draft, published], to: in_review}
        - {id: approve, label: Approve, from: [in_review], to: approved}
        - {id: publish, label: Publish, from: [approved], to: published}
        - {id: archive, label: Archive, from: [published], to: archived}
        - {id: restore, label: Restore, from: [archived], to: published}
"#;

fn item_type_yaml(type_name: &str) -> String {
    format!(
        "type: {type_name}\nlabel: {type_name}\ndescription: null\nhas_title: true\n\
         title_label: Title\nplugin: b301_test\nsettings:\n  fields: []\n  moderation: true\n  \
         moderation_workflow: review\n"
    )
}

/// (a) Importing a `moderation.workflows` variable with a five-state workflow
/// and a type naming it succeeds against a fresh database.
#[tokio::test]
async fn importing_a_custom_workflow_and_assigning_it_to_a_type_succeeds() {
    let db = ScratchDb::new("import_ok").await;
    let storage = db.storage();
    let dir = TempConfigDir::new("import_ok");
    dir.write("variable.moderation_workflows.yml", REVIEW_WORKFLOW_YAML);
    dir.write(
        "item_type.b301_review_import.yml",
        &item_type_yaml("b301_review_import"),
    );

    import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect("a well-formed custom workflow and a type naming it must import clean");

    let stored = storage
        .load(
            trovato_kernel::config_storage::entity_types::VARIABLE,
            "moderation.workflows",
        )
        .await
        .unwrap();
    assert!(stored.is_some(), "the workflow variable must be stored");
}

/// (f) `config export` then `config import` into a clean database carries the
/// workflow and the type's assignment unchanged.
#[tokio::test]
async fn export_then_import_round_trips_the_workflow_and_the_assignment() {
    use trovato_kernel::config_storage::yaml::export_config;

    let src = ScratchDb::new("roundtrip_src").await;
    let src_storage = src.storage();
    let dir = TempConfigDir::new("roundtrip_in");
    dir.write("variable.moderation_workflows.yml", REVIEW_WORKFLOW_YAML);
    dir.write(
        "item_type.b301_review_rt.yml",
        &item_type_yaml("b301_review_rt"),
    );
    import_config(&src_storage, src.pool(), dir.path(), false)
        .await
        .expect("initial import must succeed");

    let export_dir = TempConfigDir::new("roundtrip_export");
    export_config(&src_storage, src.pool(), export_dir.path(), false)
        .await
        .expect("export must succeed");

    let dst = ScratchDb::new("roundtrip_dst").await;
    let dst_storage = dst.storage();
    import_config(&dst_storage, dst.pool(), export_dir.path(), false)
        .await
        .expect("the exported set must import clean into a fresh database");

    let value = SiteConfig::get(dst.pool(), WORKFLOWS_KEY)
        .await
        .unwrap()
        .expect("the workflow variable must have round-tripped");
    let workflows = moderation::resolve_workflows(Some(&value));
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].id, "review");

    let imported_type =
        trovato_kernel::models::ItemType::find_by_type(dst.pool(), "b301_review_rt")
            .await
            .unwrap()
            .expect("the type must have round-tripped");
    assert_eq!(moderation::workflow_name(&imported_type.settings), "review");
}

/// (g) `config import` refuses a workflow value that breaks the rules, naming
/// the problem, and refuses a type naming a workflow that does not exist.
#[tokio::test]
async fn config_import_refuses_every_named_problem() {
    async fn refused_containing(label: &str, yaml: &str, needle: &str) {
        let db = ScratchDb::new(&format!("bad_{label}")).await;
        let storage = db.storage();
        let dir = TempConfigDir::new(&format!("bad_{label}"));
        dir.write("variable.moderation_workflows.yml", yaml);
        let err = import_config(&storage, db.pool(), dir.path(), false)
            .await
            .expect_err(&format!("{label}: must be refused"));
        let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
        assert!(
            failed.failures.iter().any(|f| f.error.contains(needle)),
            "{label}: expected a failure containing {needle:?}, got {failed}"
        );
    }

    refused_containing(
        "unknown_transition_state",
        "key: moderation.workflows\nvalue:\n  workflows:\n    - id: w\n      default_state: draft\n      states:\n        - {id: draft, published: false}\n        - {id: published, published: true}\n      transitions:\n        - {id: t, from: [draft], to: nowhere}\n",
        "unknown state 'nowhere'",
    )
    .await;

    refused_containing(
        "no_published_state",
        "key: moderation.workflows\nvalue:\n  workflows:\n    - id: w\n      default_state: draft\n      states:\n        - {id: draft, published: false}\n      transitions: []\n",
        "no published state",
    )
    .await;

    refused_containing(
        "bad_default_state",
        "key: moderation.workflows\nvalue:\n  workflows:\n    - id: w\n      default_state: missing\n      states:\n        - {id: draft, published: false}\n        - {id: published, published: true}\n      transitions: []\n",
        "default_state 'missing'",
    )
    .await;

    refused_containing(
        "state_id_too_long",
        &format!(
            "key: moderation.workflows\nvalue:\n  workflows:\n    - id: w\n      default_state: draft\n      states:\n        - {{id: draft, published: false}}\n        - {{id: published, published: true}}\n        - {{id: {}, published: false}}\n      transitions: []\n",
            "a".repeat(33)
        ),
        "must be at most 32 characters",
    )
    .await;

    refused_containing(
        "state_id_bad_pattern",
        "key: moderation.workflows\nvalue:\n  workflows:\n    - id: w\n      default_state: draft\n      states:\n        - {id: draft, published: false}\n        - {id: published, published: true}\n        - {id: Bad-Id, published: false}\n      transitions: []\n",
        "must be at most 32 characters and match",
    )
    .await;

    refused_containing(
        "inconsistent_flags",
        "key: moderation.workflows\nvalue:\n  workflows:\n    - id: a\n      default_state: draft\n      states:\n        - {id: draft, published: false}\n        - {id: published, published: true}\n      transitions: []\n    - id: b\n      default_state: draft\n      states:\n        - {id: draft, published: false, default_revision: true}\n        - {id: published, published: true}\n      transitions: []\n",
        "workflow 'a'",
    )
    .await;

    // A type naming a workflow this import does not have.
    {
        let db = ScratchDb::new("unknown_workflow_type").await;
        let storage = db.storage();
        let dir = TempConfigDir::new("unknown_workflow_type");
        dir.write(
            "item_type.b301_unknown_wf.yml",
            "type: b301_unknown_wf\nlabel: b301_unknown_wf\ndescription: null\nhas_title: true\n\
             title_label: Title\nplugin: b301_test\nsettings:\n  fields: []\n  moderation: true\n  \
             moderation_workflow: nonexistent\n",
        );
        let err = import_config(&storage, db.pool(), dir.path(), false)
            .await
            .expect_err("a type naming an unknown workflow must be refused");
        let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
        assert!(
            failed
                .failures
                .iter()
                .any(|f| f.error.contains("nonexistent")),
            "{failed}"
        );
    }

    // A workflow change that drops a state a stored revision still holds.
    {
        let db = ScratchDb::new("removed_state_in_use").await;
        let storage = db.storage();
        let dir = TempConfigDir::new("removed_state_in_use_1");
        dir.write("variable.moderation_workflows.yml", REVIEW_WORKFLOW_YAML);
        dir.write(
            "item_type.b301_removed.yml",
            &item_type_yaml("b301_removed"),
        );
        import_config(&storage, db.pool(), dir.path(), false)
            .await
            .expect("the initial import must succeed");

        let item_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO item (id, type, title, status, promote, sticky, language, \
             current_revision_id, author_id, created, changed, stage_id, fields) \
             VALUES ($1, 'b301_removed', 'held', 0, 0, 0, 'en', NULL, \
             '00000000-0000-0000-0000-000000000000', $2, $2, $3, '{}')",
        )
        .bind(item_id)
        .bind(now)
        .bind(LIVE_STAGE_ID)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, \
             moderation_state) VALUES ($1, $2, '00000000-0000-0000-0000-000000000000', 'held', \
             0, '{}', $3, 'in_review')",
        )
        .bind(revision_id)
        .bind(item_id)
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query("UPDATE item SET current_revision_id = $1 WHERE id = $2")
            .bind(revision_id)
            .bind(item_id)
            .execute(db.pool())
            .await
            .unwrap();

        let dir2 = TempConfigDir::new("removed_state_in_use_2");
        dir2.write(
            "variable.moderation_workflows.yml",
            "key: moderation.workflows\nvalue:\n  workflows:\n    - id: review\n      \
             default_state: draft\n      states:\n        - {id: draft, published: false}\n        \
             - {id: published, published: true}\n      transitions: []\n",
        );
        let err = import_config(&storage, db.pool(), dir2.path(), false)
            .await
            .expect_err("dropping 'in_review' while a stored revision holds it must be refused");
        let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
        assert!(
            failed.failures.iter().any(|f| f.error.contains("in_review")),
            "{failed}"
        );
    }
}

/// (h) A role file granting `use review transition approve` imports, and the
/// permission grid lists it.
#[tokio::test]
async fn a_role_file_granting_a_workflow_permission_imports_and_the_grid_lists_it() {
    let db = ScratchDb::new("role_grant").await;
    let storage = db.storage();
    let dir = TempConfigDir::new("role_grant");
    dir.write("variable.moderation_workflows.yml", REVIEW_WORKFLOW_YAML);
    let role_id = Uuid::now_v7();
    dir.write(
        &format!("role.{role_id}.yml"),
        &format!(
            "id: {role_id}\nname: b301_reviewer\nis_admin: false\npermissions:\n  - use review transition approve\n"
        ),
    );
    import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect("a role granting a derived workflow permission must import clean");

    let granted = trovato_kernel::models::Role::all_granted_permissions(db.pool())
        .await
        .unwrap();
    assert!(
        granted.contains(&"use review transition approve".to_string()),
        "{granted:?}"
    );
}

// ---------------------------------------------------------------------------
// (b), (c), (d): write path against the shared app
// ---------------------------------------------------------------------------

/// (b) Transition permissions are checked one at a time: a holder of only
/// `use review transition submit` moves draft to in_review and no further; a
/// holder of `approve` makes the next move; a holder of `publish` publishes
/// from approved, and the published item is visible to anonymous visitors.
#[test]
fn transition_permissions_gate_each_step_of_a_custom_workflow() {
    run_test(async {
        let app = shared_app().await;
        site_wide().await;
        let previous = install_review_workflow(app).await;
        ensure_review_type(app).await;

        let submitter = real(app, "b301submit", &[
            "access content",
            "create b301_review content",
            "edit own content",
            "view own content",
            "use review transition create",
            "use review transition submit",
        ]).await;
        let approver = real(app, "b301approve", &[
            "access content",
            "edit any content",
            "view any content",
            "use review transition approve",
        ]).await;
        let publisher = real(app, "b301publish", &[
            "access content",
            "edit any content",
            "view any content",
            "use review transition publish",
        ]).await;

        let item = app
            .state
            .items()
            .create_moderated(new_item("Review me", &submitter), None, None, &submitter)
            .await
            .unwrap();
        forget(app, item.id);
        assert_eq!(item.status, 0, "a new item starts unpublished");
        let (_, status) = row(app, item.id).await;
        assert_eq!(status, 0);

        // draft -> in_review: the submitter's.
        app.state.items().invalidate(item.id);
        let moved = app
            .state
            .items()
            .update_moderated(item.id, edit(None), Some("in_review"), &submitter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(moved.id, item.id);

        // in_review -> approved: refused to the submitter.
        app.state.items().invalidate(item.id);
        refused_transition(
            app.state
                .items()
                .update_moderated(item.id, edit(None), Some("approved"), &submitter)
                .await
                .map(|i| i.unwrap()),
            "in_review",
            "approved",
        );

        // in_review -> approved: the approver's.
        app.state.items().invalidate(item.id);
        app.state
            .items()
            .update_moderated(item.id, edit(None), Some("approved"), &approver)
            .await
            .unwrap()
            .unwrap();

        // approved -> published: the publisher's, and the item becomes
        // visible to anonymous visitors.
        app.state.items().invalidate(item.id);
        app.state
            .items()
            .update_moderated(item.id, edit(None), Some("published"), &publisher)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row(app, item.id).await.1, 1);
        assert!(visible_to_anonymous(app, item.id).await);

        restore_workflow(app, previous).await;
    });
}

/// (c) On a published item, a save to a non-default-revision state
/// (`in_review`) is a forward revision: the item row and what anonymous sees
/// stay the published version.
#[test]
fn a_forward_revision_on_a_custom_workflow_leaves_the_published_row_alone() {
    run_test(async {
        let app = shared_app().await;
        site_wide().await;
        let previous = install_review_workflow(app).await;
        ensure_review_type(app).await;

        let submitter = real(app, "b301fwdsub", &[
            "access content",
            "create b301_review content",
            "edit own content",
            "view own content",
            "use review transition create",
            "use review transition submit",
        ]).await;
        let approver = real(app, "b301fwdapp", &[
            "access content",
            "edit any content",
            "view any content",
            "use review transition approve",
        ]).await;
        let publisher = real(app, "b301fwdpub", &[
            "access content",
            "edit any content",
            "view any content",
            "use review transition publish",
        ]).await;

        let item = app
            .state
            .items()
            .create_moderated(
                new_item("Published original", &submitter),
                None,
                None,
                &submitter,
            )
            .await
            .unwrap();
        forget(app, item.id);
        app.state.items().invalidate(item.id);
        app.state
            .items()
            .update_moderated(item.id, edit(None), Some("in_review"), &submitter)
            .await
            .unwrap();
        app.state.items().invalidate(item.id);
        app.state
            .items()
            .update_moderated(item.id, edit(None), Some("approved"), &approver)
            .await
            .unwrap();
        app.state.items().invalidate(item.id);
        app.state
            .items()
            .update_moderated(item.id, edit(None), Some("published"), &publisher)
            .await
            .unwrap();
        assert_eq!(row(app, item.id).await, ("Published original".to_string(), 1));

        // A later save to in_review over the published item is a forward
        // revision: the row and what anonymous sees do not change.
        app.state.items().invalidate(item.id);
        let returned = app
            .state
            .items()
            .update_moderated(
                item.id,
                edit(Some("New working title")),
                Some("in_review"),
                &submitter,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(returned.title, "Published original");
        assert_eq!(
            row(app, item.id).await,
            ("Published original".to_string(), 1)
        );
        assert!(visible_to_anonymous(app, item.id).await);
        app.state.items().invalidate(item.id);
        let loaded = app.state.items().load(item.id).await.unwrap().unwrap();
        let anon_visible_title = loaded.title.clone();
        assert_eq!(anon_visible_title, "Published original");

        restore_workflow(app, previous).await;
    });
}

/// (d) A background save of `{"status": 1}` (what scheduled publishing sends)
/// publishes an item in `approved` (the one transition that reaches a
/// published state), and is refused for an item still in `draft`, which has
/// no direct transition to Published.
#[test]
fn a_background_status_only_save_follows_the_one_reachable_published_transition() {
    run_test(async {
        let app = shared_app().await;
        site_wide().await;
        let previous = install_review_workflow(app).await;
        ensure_review_type(app).await;

        let submitter = real(app, "b301bgsub", &[
            "access content",
            "create b301_review content",
            "edit own content",
            "view own content",
            "use review transition create",
            "use review transition submit",
        ]).await;
        let approver = real(app, "b301bgapp", &[
            "access content",
            "edit any content",
            "view any content",
            "use review transition approve",
        ]).await;
        let background = UserContext::background();

        // An item still in draft: refused.
        let draft_item = app
            .state
            .items()
            .create_moderated(new_item("Still draft", &submitter), None, None, &submitter)
            .await
            .unwrap();
        forget(app, draft_item.id);
        app.state.items().invalidate(draft_item.id);
        let mut status_only = edit(None);
        status_only.status = Some(1);
        let result = app
            .state
            .items()
            .update_moderated(draft_item.id, status_only.clone(), None, &background)
            .await;
        refused_transition(result.map(|i| i.unwrap()), "draft", "draft");

        // An item in approved: the background save publishes it.
        let approved_item = app
            .state
            .items()
            .create_moderated(
                new_item("Ready to publish", &submitter),
                None,
                None,
                &submitter,
            )
            .await
            .unwrap();
        forget(app, approved_item.id);
        app.state.items().invalidate(approved_item.id);
        app.state
            .items()
            .update_moderated(approved_item.id, edit(None), Some("in_review"), &submitter)
            .await
            .unwrap();
        app.state.items().invalidate(approved_item.id);
        app.state
            .items()
            .update_moderated(approved_item.id, edit(None), Some("approved"), &approver)
            .await
            .unwrap();
        app.state.items().invalidate(approved_item.id);
        app.state
            .items()
            .update_moderated(approved_item.id, status_only, None, &background)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row(app, approved_item.id).await.1, 1);

        restore_workflow(app, previous).await;
    });
}

// ---------------------------------------------------------------------------
// (e): pages
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

/// The `option` values of the `moderation_state` select on `html`.
fn moderation_options(html: &str) -> Vec<String> {
    let Some(at) = html.find(r#"name="moderation_state""#) else {
        return Vec::new();
    };
    let end = at + html[at..].find("</select>").unwrap();
    html[at..end]
        .split("<option")
        .skip(1)
        .filter_map(|o| {
            let v = o.find(r#"value=""#)? + 7;
            Some(o[v..v + o[v..].find('"')?].to_string())
        })
        .collect()
}

/// (e) The item form offers exactly the states the user may reach, by their
/// labels; the moderated list filtered to `in_review` shows the item, and its
/// filter offers "In review" and "Approved" among its options; the revision
/// list shows "In review" against that revision.
#[test]
fn the_form_list_and_revision_history_use_the_workflows_own_labels() {
    run_test(async {
        let app = shared_app().await;
        site_wide().await;
        let previous = install_review_workflow(app).await;
        ensure_review_type(app).await;

        let (author_id, author_cookies) =
            common::user_holding(app, "b301pageauth", &[
            "access content",
            "create b301_review content",
            "edit own content",
            "view own content",
            "use review transition create",
            "use review transition submit",
        ]).await;
        let submitter = user(author_id, &[
            "access content",
            "create b301_review content",
            "edit own content",
            "view own content",
            "use review transition create",
            "use review transition submit",
        ]);
        let approver = real(app, "b301pageapp", &[
            "access content",
            "edit any content",
            "view any content",
            "use review transition approve",
        ]).await;
        let (_, moderator_cookies) = common::user_holding(
            app,
            "b301pagemod",
            &[
                "access content",
                "access administration pages",
                "view latest version",
                "view any content",
                "view revisions",
                "use review transition submit",
                "use review transition approve",
            ],
        )
        .await;

        let item = app
            .state
            .items()
            .create_moderated(new_item("Review page probe", &submitter), None, None, &submitter)
            .await
            .unwrap();
        forget(app, item.id);
        app.state.items().invalidate(item.id);
        app.state
            .items()
            .update_moderated(item.id, edit(None), Some("in_review"), &submitter)
            .await
            .unwrap();

        let ip = test_ip_for("b301page");
        let edit_path = format!("/item/{}/edit", item.id);
        let (status, html) = get(app, &author_cookies, &edit_path, &ip).await;
        assert_eq!(status, StatusCode::OK);
        // The submitter holds only `submit`, which does not leave `in_review`,
        // so no further move is offered; only the current state shows.
        assert!(html.contains("Current state:"));
        assert!(html.contains("In review"));

        // The approver's form offers moving to Approved.
        let (_, approver_cookies) =
            common::user_holding(app, "b301pageapp2", &[
            "access content",
            "edit any content",
            "view any content",
            "use review transition approve",
        ]).await;
        let (status, html) = get(app, &approver_cookies, &edit_path, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(moderation_options(&html), vec!["approved"]);

        // The moderated list filtered to in_review.
        let (status, html) = get(
            app,
            &moderator_cookies,
            "/admin/content/moderated?state=in_review",
            &ip,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Review page probe"), "the item is not listed");

        // The unfiltered list's filter options include both labels.
        let (status, html) = get(app, &moderator_cookies, "/admin/content/moderated", &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("In review"), "{html}");
        assert!(html.contains("Approved"), "{html}");

        // The revision list shows "In review" against the latest revision.
        let (status, html) = get(
            app,
            &moderator_cookies,
            &format!("/item/{}/revisions", item.id),
            &ip,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("In review"), "{html}");

        let _ = approver;
        restore_workflow(app, previous).await;
    });
}

// ---------------------------------------------------------------------------
// (i): stage move + forward revision, on the default workflow
// ---------------------------------------------------------------------------

/// (i) On the default (`editorial`) workflow, a save that asks for a stage
/// move together with a draft over a published item is refused: accepting it
/// would check the move and then silently drop it, because the forward-draft
/// branch returns before `Item::update` ever applies the move.
#[test]
fn a_stage_move_with_a_forward_draft_is_refused_not_silently_dropped() {
    run_test(async {
        let app = shared_app().await;
        let settings = json!({ "fields": [], "moderation": true });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $1, '', true, 'Title', 'b301_test', $2) ON CONFLICT (type) DO NOTHING",
        )
        .bind("b301_stage_fwd")
        .bind(&settings)
        .execute(&app.db)
        .await
        .unwrap();
        app.state
            .content_types()
            .replace("b301_stage_fwd", "b301_stage_fwd", None, settings)
            .await
            .ok();

        let editor = real(
            app,
            "b301stagefwd",
            &[
                "access content",
                "edit any content",
                "view any content",
                "use editorial transition create_new_draft",
                "use editorial transition publish",
            ],
        )
        .await;

        let other_stage = trovato_kernel::models::stage::Stage::create(
            &app.db,
            trovato_kernel::models::stage::CreateStage {
                label: "B301 stage".to_string(),
                machine_name: format!("b301_stage_fwd_{}", Uuid::now_v7().simple()),
                description: None,
                visibility: Some("internal".to_string()),
                is_default: Some(false),
                weight: Some(0),
            },
        )
        .await
        .unwrap();
        let other_stage_id = other_stage.id;

        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "b301_stage_fwd".to_string(),
                    title: "Stage and forward probe".to_string(),
                    author_id: editor.id,
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: None,
                },
                &editor,
            )
            .await
            .unwrap();
        forget(app, item.id);
        app.state.items().invalidate(item.id);
        app.state
            .items()
            .update_moderated(item.id, edit(None), Some("published"), &editor)
            .await
            .unwrap();

        app.state.items().invalidate(item.id);
        let mut input = edit(Some("Draft over published, moving stage"));
        input.stage_id = Some(other_stage_id);
        // An administrator may move any item to any stage with no stage
        // workflow configured; this test is about the moderation write path's
        // own refusal, not the separate stage-workflow permission.
        let result = app
            .state
            .items()
            .update_moderated(item.id, input, Some("draft"), &trovato_kernel::tap::UserContext::administrator(Uuid::nil(), vec![]))
            .await;
        let error = result.expect_err("a stage move with a forward draft must be refused");
        assert_eq!(
            error.downcast_ref::<WriteDenied>(),
            Some(&WriteDenied::TransitionWithStageMove),
            "{error}"
        );

        // Confirm it was refused, not silently dropped: the item is unchanged.
        let (title, status) = row(app, item.id).await;
        assert_eq!(title, "Stage and forward probe");
        assert_eq!(status, 1);
    });
}
