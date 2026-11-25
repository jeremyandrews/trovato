#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every writer of searchable content queues the items it touched, and one
//! incremental update of the static index then answers searches for the
//! change (B218).
//!
//! Before this, every change asked for a full rebuild of the index (a flag
//! set by the item and comment services, the type screen, and a cron poll of
//! `MAX(changed)`), and scheduled publishing changed `item.status` with raw
//! SQL that no writer saw. Now each write queues its items in
//! `search_index_queue` with a reason (the database itself, in the write's
//! own transaction, for items, comments and URL aliases), and the cron updater
//! rewrites only those pages.
//!
//! One test per writer: the item service (create, edit, unpublish, delete),
//! the comment service (create, edit, delete), the content type search
//! switches and the search field config, URL aliases, workspace publishing,
//! scheduled publishing and unpublishing (through the real cron and the
//! plugin, whose status changes go through the item API), and a plugin writing
//! with raw SQL, with and without `search_index_enqueue`. Each asserts the
//! queue row the write left, then moves it into a private index
//! (`common::search_index`), runs one update, and asserts what the index
//! finds. Two more test the queue's triggers inside a transaction, and that a
//! scheduled date is applied once.
//!
//! Requires Postgres, Redis and, for the two plugin paths, the fixture
//! `.wasm` files (built by CI before the test job).

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::search_index::{Index, by_item, unique_word, urls};
use common::{TestApp, run_test, shared_app};
use serde_json::{Value, json};
use sqlx::PgPool;
use trovato_kernel::cron::{
    CronResult, DocumentSource, IndexDocument, RunOptions, maybe_rebuild_index_with,
    search_index_run,
};
use trovato_kernel::models::comment::CommentStatus;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::url_alias::{CreateUrlAlias, UpdateUrlAlias};
use trovato_kernel::models::{CreateComment, CreateItem, SiteConfig, UpdateComment, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// The item service's update input, from the keys given.
fn update(changes: Value) -> UpdateItem {
    serde_json::from_value(changes).expect("an item update")
}

/// A published (or not) `page` on `stage` with `title` and `body`.
async fn create_page(app: &TestApp, title: &str, body: &str, status: i16, stage: Uuid) -> Uuid {
    create_typed(app, "page", title, body, status, stage).await
}

/// [`create_page`] on `item_type`.
async fn create_typed(
    app: &TestApp,
    item_type: &str,
    title: &str,
    body: &str,
    status: i16,
    stage: Uuid,
) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(json!({ "field_body": { "value": body, "format": "plain_text" } })),
                stage_id: Some(stage),
                language: Some("en".to_string()),
                log: Some("search index writer fixture".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create a page")
        .id;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM search_index_queue WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    });
    id
}

/// A private index holding one published anchor page, so no write in a test
/// empties its only language, which is a full build by design.
async fn anchored(app: &TestApp) -> Index {
    let index = Index::new(&app.db, app.state.items()).await;
    let anchor = create_page(app, "Anchor page", "Always here", 1, LIVE_STAGE_ID).await;
    index.track(anchor);
    index.take(&[anchor]).await;
    index.update().await;
    assert_eq!(index.search("en", "anchor"), urls([&item_url(anchor)]));
    index
}

fn item_url(id: Uuid) -> String {
    format!("/item/{id}")
}

/// One queue row for `id`, with this op and reason.
#[track_caller]
fn assert_queued(rows: &[common::search_index::QueuedRow], id: Uuid, op: i16, reason: &str) {
    assert_eq!(
        by_item(rows),
        [(id, (op, reason.to_string()))].into_iter().collect(),
        "the write queues its item"
    );
}

#[test]
fn item_service_create_edit_unpublish_and_delete_update_the_index() {
    run_test(async {
        let app = shared_app().await;
        let index = anchored(app).await;
        let first = unique_word("first");
        let second = unique_word("second");
        let body = unique_word("body");

        // Create: queued as an upsert, found after one update.
        let id = create_page(app, &format!("Probe {first}"), &body, 1, LIVE_STAGE_ID).await;
        index.track(id);
        assert_queued(&index.take(&[id]).await, id, 1, "item");
        let report = index.update().await;
        assert_eq!(report.updates[0].pages_changed, 1);
        assert_eq!(index.search("en", &first), urls([&item_url(id)]));
        assert_eq!(index.search("en", &body), urls([&item_url(id)]));

        // A title edit: the new title is found, the old one is gone.
        app.state
            .items()
            .update(
                id,
                update(json!({ "title": format!("Probe {second}") })),
                &admin(),
            )
            .await
            .expect("edit the title")
            .expect("the item");
        assert_queued(&index.take(&[id]).await, id, 1, "item");
        index.update().await;
        assert_eq!(index.search("en", &second), urls([&item_url(id)]));
        assert!(
            index.search("en", &first).is_empty(),
            "the old title is gone"
        );

        // Unpublish: still an upsert of the item, which the export now leaves
        // out, so its page goes.
        app.state
            .items()
            .update(id, update(json!({ "status": 0 })), &admin())
            .await
            .expect("unpublish")
            .expect("the item");
        assert_queued(&index.take(&[id]).await, id, 1, "item");
        index.update().await;
        assert!(index.search("en", &second).is_empty(), "unpublished is out");
        assert!(index.search("en", &body).is_empty());

        // Republish, then delete: a delete op.
        app.state
            .items()
            .update(id, update(json!({ "status": 1 })), &admin())
            .await
            .expect("republish")
            .expect("the item");
        index.take(&[id]).await;
        index.update().await;
        assert_eq!(index.search("en", &second), urls([&item_url(id)]));
        assert!(
            app.state
                .items()
                .delete(id, &admin())
                .await
                .expect("delete"),
            "deleted"
        );
        assert_queued(&index.take(&[id]).await, id, 2, "item");
        index.update().await;
        assert!(index.search("en", &second).is_empty(), "deleted is out");
    });
}

#[test]
fn comment_create_edit_and_delete_update_the_index() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        let index = anchored(app).await;
        let said = unique_word("said");
        let edited = unique_word("edited");

        let id = create_page(
            app,
            "Comment probe",
            "A page to comment on",
            1,
            LIVE_STAGE_ID,
        )
        .await;
        index.track(id);
        index.take(&[id]).await;
        index.update().await;

        // A published comment: its item is queued, and the comment's words
        // find the item.
        let comment = app
            .state
            .comments()
            .create(
                CreateComment {
                    item_id: id,
                    parent_id: None,
                    author_id: Uuid::nil(),
                    subject: None,
                    body: format!("I would add {said} here."),
                    body_format: None,
                    status: Some(CommentStatus::Published.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("comment")
            .id;
        assert_queued(&index.take(&[id]).await, id, 1, "comment");
        let report = index.update().await;
        assert_eq!(report.updates[0].pages_changed, 1);
        assert_eq!(index.search("en", &said), urls([&item_url(id)]));

        // An edit.
        app.state
            .comments()
            .update(
                comment,
                UpdateComment {
                    body: Some(format!("On reflection, {edited}.")),
                    ..UpdateComment::default()
                },
                &admin(),
            )
            .await
            .expect("edit the comment")
            .expect("the comment");
        assert_queued(&index.take(&[id]).await, id, 1, "comment");
        index.update().await;
        assert_eq!(index.search("en", &edited), urls([&item_url(id)]));
        assert!(index.search("en", &said).is_empty(), "the old text is gone");

        // A delete: the item stays, the comment's words go.
        assert!(
            app.state
                .comments()
                .delete(comment, &admin())
                .await
                .expect("delete the comment")
        );
        assert_queued(&index.take(&[id]).await, id, 1, "comment");
        index.update().await;
        assert!(
            index.search("en", &edited).is_empty(),
            "the comment is gone"
        );
        assert_eq!(
            index.search("en", "comment"),
            urls([&item_url(id)]),
            "the item stays"
        );
    });
}

#[test]
fn a_search_field_config_change_queues_every_item_of_the_type() {
    run_test(async {
        use trovato_sdk::types::{FieldDefinition, FieldType};

        let app = shared_app().await;
        let index = anchored(app).await;

        // A type of this test's own, so the change queues only its items.
        let item_type = format!("b218fc_{}", &Uuid::now_v7().simple().to_string()[20..]);
        let fields = vec![FieldDefinition::new("field_body", FieldType::TextLong).label("Body")];
        app.state
            .content_types()
            .create(
                &item_type,
                "Field config probe",
                Some("search index writers fixture"),
                json!({ "fields": serde_json::to_value(&fields).unwrap() }),
            )
            .await
            .expect("create the fixture type");
        let pool = app.db.clone();
        let machine_name = item_type.clone();
        common::defer_cleanup(move || async move {
            for sql in [
                "DELETE FROM search_index_queue WHERE item_id IN (SELECT id FROM item WHERE type = $1)",
                "DELETE FROM item WHERE type = $1",
                "DELETE FROM search_field_config WHERE bundle = $1",
                "DELETE FROM item_type WHERE type = $1",
            ] {
                sqlx::query(sql)
                    .bind(&machine_name)
                    .execute(&pool)
                    .await
                    .ok();
            }
        });

        let word = unique_word("typed");
        let ids = [
            create_typed(
                app,
                &item_type,
                &format!("Typed {word}"),
                "Body",
                1,
                LIVE_STAGE_ID,
            )
            .await,
            create_typed(app, &item_type, "Typed too", &word, 1, LIVE_STAGE_ID).await,
        ];
        for id in ids {
            index.track(id);
        }
        index.take(&ids).await;
        index.update().await;

        // A field's weight is in every page of the type: the whole type is
        // queued.
        app.state
            .search()
            .configure_field(&item_type, "field_body", 'A')
            .await
            .expect("configure a field");
        let rows = index.take(&ids).await;
        assert_eq!(
            by_item(&rows),
            ids.iter()
                .map(|id| (*id, (1, "field_config".to_string())))
                .collect(),
            "every item of the type is queued"
        );
        let report = index.update().await;
        assert_eq!(report.updates[0].queued, 2);
        assert_eq!(
            index.search("en", &word),
            urls([&item_url(ids[0]), &item_url(ids[1])])
        );

        // Removing it queues the type again.
        assert!(
            app.state
                .search()
                .remove_field_config(&item_type, "field_body")
                .await
                .expect("remove the field config")
        );
        assert_eq!(index.take(&ids).await.len(), 2, "both queued again");
        index.update().await;
    });
}

#[test]
fn url_alias_writes_update_the_result_link() {
    run_test(async {
        let app = shared_app().await;
        let index = anchored(app).await;
        let word = unique_word("aliased");
        let id = create_page(app, &format!("Aliased {word}"), "Body", 1, LIVE_STAGE_ID).await;
        index.track(id);
        index.take(&[id]).await;
        index.update().await;
        assert_eq!(index.search("en", &word), urls([&item_url(id)]));

        // The alias is the result card's link.
        let path = format!("/b218-{}", &Uuid::now_v7().simple().to_string()[20..]);
        let alias = app
            .state
            .aliases()
            .create(CreateUrlAlias {
                source: item_url(id),
                alias: path.clone(),
                language: Some("en".to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            })
            .await
            .expect("create an alias");
        assert_queued(&index.take(&[id]).await, id, 1, "url_alias");
        index.update().await;
        assert_eq!(index.search("en", &word), urls([&path]));

        // An update to the alias.
        let moved = format!("{path}-moved");
        app.state
            .aliases()
            .update(
                alias.id,
                UpdateUrlAlias {
                    source: None,
                    alias: Some(moved.clone()),
                    language: None,
                    stage_id: None,
                },
            )
            .await
            .expect("update the alias")
            .expect("the alias");
        assert_queued(&index.take(&[id]).await, id, 1, "url_alias");
        index.update().await;
        assert_eq!(index.search("en", &word), urls([&moved]));

        // Its delete: back to the item's own path.
        assert!(app.state.aliases().delete(alias.id).await.expect("delete"));
        assert_queued(&index.take(&[id]).await, id, 1, "url_alias");
        index.update().await;
        assert_eq!(index.search("en", &word), urls([&item_url(id)]));
    });
}

#[test]
fn publishing_a_workspace_queues_its_items_with_the_move() {
    run_test(async {
        let app = shared_app().await;
        let index = anchored(app).await;
        let word = unique_word("staged");
        let stage = common::create_test_stage(app, "b218").await;
        let id = create_page(app, &format!("Staged {word}"), "Body", 1, stage).await;
        index.track(id);
        // Created on a workspace: not in the index, which holds Live.
        index.take(&[id]).await;
        index.update().await;
        assert!(index.search("en", &word).is_empty(), "not live yet");

        // An alias whose source looks like an item path but is no UUID: the
        // publish must not try to read it as one.
        let junk = format!("/b218-junk-{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO url_alias (source, alias, language, stage_id, created) \
             VALUES ('/item/' || repeat('-', 36), $1, 'en', $2, 0)",
        )
        .bind(&junk)
        .bind(stage)
        .execute(&app.db)
        .await
        .expect("an alias with a malformed item source");

        let result = app.stage().publish(stage).await.expect("publish");
        assert!(result.success, "{result:?}");
        assert_queued(&index.take(&[id]).await, id, 1, "stage_publish");
        index.update().await;
        assert_eq!(
            index.search("en", &word),
            urls([&item_url(id)]),
            "found once published"
        );
        sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&junk)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

/// The queue row for `id` in the shared queue, as `(op, reason)`.
async fn queued_row<'e, E: sqlx::PgExecutor<'e>>(db: E, id: Uuid) -> Option<(i16, String)> {
    sqlx::query_as("SELECT op, reason FROM search_index_queue WHERE item_id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .expect("read the queue")
}

#[test]
fn the_database_queues_a_write_in_the_write_s_own_transaction() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        let id = create_page(app, "Trigger probe", "Body", 1, LIVE_STAGE_ID).await;
        let clear = || async {
            sqlx::query("DELETE FROM search_index_queue WHERE item_id = $1")
                .bind(id)
                .execute(&app.db)
                .await
                .unwrap();
        };
        clear().await;

        // A write that rolls back queues nothing; inside it, the row is there.
        let mut tx = app.db.begin().await.unwrap();
        sqlx::query("UPDATE item SET title = 'Rolled back' WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            queued_row(&mut *tx, id).await,
            Some((1, "item".to_string())),
            "queued by the write itself"
        );
        tx.rollback().await.unwrap();
        assert_eq!(queued_row(&app.db, id).await, None, "rolled back with it");

        // A write that commits is queued, whatever runs after the commit.
        sqlx::query("UPDATE item SET title = 'Committed' WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(queued_row(&app.db, id).await, Some((1, "item".to_string())));
        clear().await;

        // A comment awaiting moderation is not on the page: its insert, and
        // the comment count it moves on the item, queue nothing.
        app.state
            .comments()
            .create(
                CreateComment {
                    item_id: id,
                    parent_id: None,
                    author_id: Uuid::nil(),
                    subject: None,
                    body: "Pending".to_string(),
                    body_format: None,
                    status: Some(CommentStatus::Pending.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("a pending comment");
        assert_eq!(queued_row(&app.db, id).await, None, "a pending comment");

        // An unpublished item is not in the index's reach.
        let draft = create_page(app, "Trigger draft", "Body", 0, LIVE_STAGE_ID).await;
        sqlx::query("UPDATE item SET title = 'Still a draft' WHERE id = $1")
            .bind(draft)
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(queued_row(&app.db, draft).await, None, "a draft");

        // Its delete is queued as a delete.
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(queued_row(&app.db, id).await, Some((2, "item".to_string())));
        clear().await;
    });
}

// -----------------------------------------------------------------------------
// The plugin paths: the real cron cycle, on an app whose only plugins are
// `trovato_scheduled_publishing` and `test_search_enqueue`.
// -----------------------------------------------------------------------------

const PLUGINS: &[&str] = &["trovato_scheduled_publishing", "test_search_enqueue"];

/// Serializes enabling the fixtures and building the app, as
/// `background_item_api_test` does with the same lock.
const FIXTURE_LOCK: i64 = 0x0040_0000_0001;

static PLUGIN_APP: tokio::sync::OnceCell<TestApp> = tokio::sync::OnceCell::const_new();

/// A fresh plugins directory holding a copy of each of [`PLUGINS`].
fn plugins_dir() -> PathBuf {
    let parent = std::env::temp_dir().join(format!("trovato-b218-{}", Uuid::now_v7().simple()));
    for name in PLUGINS {
        let source = common::project_root().join("plugins").join(name);
        let target = parent.join(name);
        std::fs::create_dir_all(&target).expect("create the plugin directory");
        std::fs::copy(
            source.join(format!("{name}.wasm")),
            target.join(format!("{name}.wasm")),
        )
        .unwrap_or_else(|e| {
            panic!(
                "copy {name}.wasm: {e}\n\
                 build it first: cargo build -p {name} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{name}.wasm plugins/{name}/"
            )
        });
        std::fs::copy(
            source.join(format!("{name}.info.toml")),
            target.join(format!("{name}.info.toml")),
        )
        .expect("copy the manifest");
        if source.join("migrations").is_dir() {
            let migrations = target.join("migrations");
            std::fs::create_dir_all(&migrations).unwrap();
            for entry in std::fs::read_dir(source.join("migrations")).unwrap() {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), migrations.join(entry.file_name())).unwrap();
            }
        }
    }
    parent
}

/// The app with both plugins enabled, built once for this file.
async fn plugin_app() -> &'static TestApp {
    PLUGIN_APP
        .get_or_init(|| async {
            trovato_test_utils::env::load_dotenv();
            let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
            common::ensure_database_migrated(&url).await;
            let pool = PgPool::connect(&url).await.expect("connect");
            let mut lock = pool.acquire().await.expect("a lock connection");
            sqlx::query("SELECT pg_advisory_lock($1)")
                .bind(FIXTURE_LOCK)
                .execute(&mut *lock)
                .await
                .expect("take the fixture lock");
            let mut saved = Vec::new();
            for name in PLUGINS {
                let status: Option<(i16, String, i64, bool)> = sqlx::query_as(
                    "SELECT status, version, updated_at, tap_install_called \
                     FROM plugin_status WHERE name = $1",
                )
                .bind(name)
                .fetch_optional(&pool)
                .await
                .expect("read the plugin status");
                saved.push((*name, status));
                trovato_kernel::plugin::status::install_plugin(&pool, name, "1.0.0")
                    .await
                    .expect("enable the fixture");
            }

            let dir = plugins_dir();
            let plugins = dir.clone();
            let app = TestApp::with_config(move |config| config.plugins_dirs = vec![plugins]).await;

            for (name, status) in saved {
                match status {
                    Some((status, version, updated_at, called)) => {
                        sqlx::query(
                            "UPDATE plugin_status SET status = $1, version = $2, \
                             updated_at = $3, tap_install_called = $4 WHERE name = $5",
                        )
                        .bind(status)
                        .bind(version)
                        .bind(updated_at)
                        .bind(called)
                        .bind(name)
                        .execute(&pool)
                        .await
                        .expect("restore the plugin status");
                    }
                    None => {
                        sqlx::query("DELETE FROM plugin_status WHERE name = $1")
                            .bind(name)
                            .execute(&pool)
                            .await
                            .expect("remove the plugin status");
                    }
                }
            }
            sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(FIXTURE_LOCK)
                .execute(&mut *lock)
                .await
                .expect("release the fixture lock");
            drop(lock);
            pool.close().await;
            std::fs::remove_dir_all(&dir).ok();
            app
        })
        .await
}

/// Run the cron cycle until `done` holds. The cron lock is site wide and other
/// suites run cron too: a skipped cycle is another holder, not a failure.
async fn cron_until<F, Fut>(app: &TestApp, what: &str, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..240 {
        let ran = !matches!(app.state.cron().run().await, CronResult::Skipped);
        if done().await {
            return;
        }
        if !ran {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    panic!("cron never {what}");
}

async fn status(pool: &PgPool, id: Uuid) -> i16 {
    sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("the item's status")
}

/// Set a scheduling field on an item directly (the fixture's `page` type does
/// not declare it; the plugin reads the JSON).
async fn schedule(pool: &PgPool, id: Uuid, field: &str, at: i64) {
    sqlx::query(
        "UPDATE item SET fields = fields || jsonb_build_object($2::text, $3::text) WHERE id = $1",
    )
    .bind(id)
    .bind(field)
    .bind(at.to_string())
    .execute(pool)
    .await
    .expect("schedule");
}

/// The scheduling dates an item still carries.
async fn dates(pool: &PgPool, id: Uuid) -> (Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT fields->>'field_publish_on', fields->>'field_unpublish_on' FROM item WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("the item's dates")
}

/// How many revisions `id` has.
async fn revisions(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM item_revision WHERE item_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("count revisions")
}

#[test]
fn scheduled_publishing_goes_through_the_item_api_and_reaches_the_index() {
    run_test(async {
        let app = plugin_app().await;
        let index = anchored(app).await;
        let word = unique_word("scheduled");
        let id = create_page(app, &format!("Scheduled {word}"), "Body", 0, LIVE_STAGE_ID).await;
        index.track(id);
        index.take(&[id]).await;
        index.update().await;
        assert!(index.search("en", &word).is_empty(), "unpublished");

        // Due a minute ago.
        let past = chrono::Utc::now().timestamp() - 60;
        schedule(&app.db, id, "field_publish_on", past).await;
        cron_until(app, "published the item", || async {
            status(&app.db, id).await == 1
        })
        .await;
        // An item service update, so queued as any editor's save is, with a
        // revision recorded; and the date it applied is gone.
        assert_queued(&index.take(&[id]).await, id, 1, "item");
        let log: Option<String> = sqlx::query_scalar(
            "SELECT log FROM item_revision WHERE item_id = $1 ORDER BY created DESC, id DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&app.db)
        .await
        .unwrap();
        assert_eq!(log.as_deref(), Some("Scheduled publish"));
        assert_eq!(
            dates(&app.db, id).await,
            (None, None),
            "the date is applied once"
        );
        index.update().await;
        assert_eq!(index.search("en", &word), urls([&item_url(id)]));

        // Scheduled unpublish: the item leaves the results.
        schedule(&app.db, id, "field_unpublish_on", past).await;
        cron_until(app, "unpublished the item", || async {
            status(&app.db, id).await == 0
        })
        .await;
        assert_queued(&index.take(&[id]).await, id, 1, "item");
        assert_eq!(
            dates(&app.db, id).await,
            (None, None),
            "the date is applied once"
        );
        index.update().await;
        assert!(
            index.search("en", &word).is_empty(),
            "a scheduled unpublish removes the item from the results"
        );
    });
}

#[test]
fn a_scheduled_date_is_applied_once_so_past_dates_do_not_repeat() {
    run_test(async {
        let app = plugin_app().await;
        // Both dates past: published, then unpublished, then left alone. Were
        // a date kept, every run would publish and unpublish the item again,
        // and items like it would fill every batch ahead of the rest.
        let id = create_page(app, "Both dates past", "Body", 0, LIVE_STAGE_ID).await;
        let past = chrono::Utc::now().timestamp() - 60;
        schedule(&app.db, id, "field_publish_on", past).await;
        schedule(&app.db, id, "field_unpublish_on", past).await;
        cron_until(app, "applied both dates", || async {
            dates(&app.db, id).await == (None, None)
        })
        .await;
        assert_eq!(status(&app.db, id).await, 0, "published, then unpublished");
        let settled = revisions(&app.db, id).await;
        let mut ran = 0;
        for _ in 0..240 {
            if matches!(app.state.cron().run().await, CronResult::Skipped) {
                tokio::time::sleep(Duration::from_millis(500)).await;
            } else {
                ran += 1;
                if ran == 2 {
                    break;
                }
            }
        }
        assert_eq!(ran, 2, "two more cron runs");
        assert_eq!(status(&app.db, id).await, 0);
        assert_eq!(
            revisions(&app.db, id).await,
            settled,
            "later runs change nothing"
        );

        // A date not yet due is kept, and its item left alone.
        let later = create_page(app, "Not yet due", "Body", 0, LIVE_STAGE_ID).await;
        schedule(&app.db, later, "field_publish_on", past + 3_600).await;
        let other = create_page(app, "Due", "Body", 0, LIVE_STAGE_ID).await;
        schedule(&app.db, other, "field_publish_on", past).await;
        cron_until(app, "published the due item", || async {
            status(&app.db, other).await == 1
        })
        .await;
        assert_eq!(status(&app.db, later).await, 0, "not yet due");
        assert!(dates(&app.db, later).await.0.is_some(), "its date is kept");
    });
}

/// The answer `test_search_enqueue` reported for `nonce`, if it has.
async fn enqueue_answer(pool: &PgPool, nonce: &str) -> Option<Value> {
    let stored = SiteConfig::get(pool, "plugin.test_search_enqueue.result")
        .await
        .ok()??;
    let answer: Value = match stored {
        Value::String(text) => serde_json::from_str(&text).ok()?,
        other => other,
    };
    (answer.get("nonce").and_then(Value::as_str) == Some(nonce)).then_some(answer)
}

/// Have the fixture retitle `id` with raw SQL, queueing it or not, and return
/// its answer.
async fn plugin_retitle(app: &TestApp, id: Uuid, title: &str, enqueue: bool) -> Value {
    let nonce = Uuid::now_v7().simple().to_string();
    let step = json!({ "nonce": nonce, "id": id.to_string(), "title": title, "enqueue": enqueue });
    SiteConfig::set(
        &app.db,
        "plugin.test_search_enqueue.step",
        json!(step.to_string()),
    )
    .await
    .expect("set the step");
    cron_until(app, "ran the plugin step", || async {
        enqueue_answer(&app.db, &nonce).await.is_some()
    })
    .await;
    enqueue_answer(&app.db, &nonce).await.unwrap()
}

#[test]
fn a_plugin_raw_write_is_queued_by_the_database_and_search_index_enqueue_names_it() {
    run_test(async {
        let app = plugin_app().await;
        let index = anchored(app).await;
        let before = unique_word("before");
        let raw = unique_word("raw");
        let queued = unique_word("queued");
        let id = create_page(app, &format!("Plugin {before}"), "Body", 1, LIVE_STAGE_ID).await;
        index.track(id);
        index.take(&[id]).await;
        index.update().await;
        assert_eq!(index.search("en", &before), urls([&item_url(id)]));

        // A raw write without the call: the database queued it with the write.
        let answer = plugin_retitle(app, id, &format!("Plugin {raw}"), false).await;
        assert_eq!(answer["updated"], 1, "{answer}");
        assert_queued(&index.take(&[id]).await, id, 1, "item");
        index.update().await;
        assert_eq!(index.search("en", &raw), urls([&item_url(id)]));
        assert!(
            index.search("en", &before).is_empty(),
            "the old title is gone"
        );

        // With `search_index_enqueue`, as for data of the plugin's own that
        // the page shows: queued again, under the plugin's name.
        let answer = plugin_retitle(app, id, &format!("Plugin {queued}"), true).await;
        assert_eq!(answer["updated"], 1, "{answer}");
        assert_eq!(answer["queued"], 1, "{answer}");
        assert_queued(
            &index.take(&[id]).await,
            id,
            1,
            "plugin:test_search_enqueue",
        );
        index.update().await;
        assert_eq!(index.search("en", &queued), urls([&item_url(id)]));
        assert!(index.search("en", &raw).is_empty(), "the old title is gone");
    });
}

// -----------------------------------------------------------------------------
// Recovery: what a cron run does when its inputs are not what it expects.
// -----------------------------------------------------------------------------

/// Overwrite `id`'s cached terms in `index` with bytes that do not decode.
async fn spoil_cached_terms(index: &Index, id: Uuid) {
    let spoiled =
        sqlx::query("UPDATE search_page_terms SET words = '\\xff'::bytea WHERE item_id = $1")
            .bind(id)
            .execute(&index.pool)
            .await
            .expect("spoil the cached terms")
            .rows_affected();
    assert_eq!(spoiled, 1, "the item has cached terms");
}

/// Retitle `id` through the item service.
async fn retitle(app: &TestApp, id: Uuid, title: &str) {
    app.state
        .items()
        .update(id, update(json!({ "title": title })), &admin())
        .await
        .expect("edit the title")
        .expect("the item");
}

#[test]
fn a_cached_term_set_that_does_not_decode_is_rebuilt_not_fatal() {
    run_test(async {
        let app = shared_app().await;
        let index = anchored(app).await;
        let first = unique_word("cachedfirst");
        let second = unique_word("cachedsecond");
        let third = unique_word("cachedthird");
        let id = create_page(app, &format!("Cached {first}"), "Body", 1, LIVE_STAGE_ID).await;
        index.track(id);
        index.take(&[id]).await;
        index.update().await;

        // An edit whose old postings are in the spoiled entry: the update
        // cannot remove them, so the run takes a full build, which drains the
        // queue rather than failing on it at every run.
        spoil_cached_terms(&index, id).await;
        retitle(app, id, &format!("Cached {second}")).await;
        index.take(&[id]).await;
        let report = index.run().await;
        assert_eq!(report.full_builds.len(), 1, "{report:?}");
        assert!(
            report.full_builds[0].0.contains("do not decode"),
            "{report:?}"
        );
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_queue")
            .fetch_one(&index.pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "the queue is drained");
        assert_eq!(index.search("en", &second), urls([&item_url(id)]));
        assert!(
            index.search("en", &first).is_empty(),
            "the old title is gone"
        );

        // A full build over a spoiled entry whose page has not changed reads
        // it as a miss and tokenizes the page again.
        spoil_cached_terms(&index, id).await;
        let report = search_index_run(
            &index.pool,
            &index.out,
            &index,
            &RunOptions {
                full_build: Some("a test".to_string()),
                compaction_percent: 100.0,
                ..RunOptions::default()
            },
        )
        .await
        .expect("a full build over a spoiled entry");
        assert_eq!(report.full_builds.len(), 1, "{report:?}");
        assert!(report.full_builds[0].1.tokenized >= 1, "{report:?}");
        assert_eq!(index.search("en", &second), urls([&item_url(id)]));

        // The entry it rewrote serves the next update.
        retitle(app, id, &format!("Cached {third}")).await;
        index.take(&[id]).await;
        index.update().await;
        assert_eq!(index.search("en", &third), urls([&item_url(id)]));
    });
}

/// A document source whose export fails.
struct FailingSource;

#[async_trait::async_trait]
impl DocumentSource for FailingSource {
    async fn documents(&self, _ids: Option<&[Uuid]>) -> anyhow::Result<Vec<IndexDocument>> {
        anyhow::bail!("the export failed")
    }
}

#[test]
fn a_requested_rebuild_that_fails_stays_requested() {
    run_test(async {
        let app = shared_app().await;
        let index = anchored(app).await;
        // The plugin's status row, in the index's own schema.
        sqlx::query(
            "CREATE TABLE pagefind_index_status (id INTEGER PRIMARY KEY DEFAULT 1, \
             last_indexed_at BIGINT NOT NULL DEFAULT 0, \
             rebuild_requested BOOLEAN NOT NULL DEFAULT false, last_error TEXT)",
        )
        .execute(&index.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO pagefind_index_status (id, rebuild_requested) VALUES (1, true)")
            .execute(&index.pool)
            .await
            .unwrap();
        let state = || async {
            sqlx::query_as::<_, (bool, Option<String>)>(
                "SELECT rebuild_requested, last_error FROM pagefind_index_status",
            )
            .fetch_one(&index.pool)
            .await
            .unwrap()
        };

        // The build fails: the error is recorded, and the request kept.
        let failed = maybe_rebuild_index_with(&index.pool, &index.out, &FailingSource, 100.0).await;
        assert!(failed.is_err(), "the export failed");
        let (requested, error) = state().await;
        assert!(requested, "the request stands for the next run");
        assert!(error.unwrap_or_default().contains("the export failed"));

        // The next run builds, and the request is done.
        let built = maybe_rebuild_index_with(&index.pool, &index.out, &index, 100.0)
            .await
            .expect("the requested build");
        assert!(built);
        assert_eq!(state().await, (false, None));
    });
}
