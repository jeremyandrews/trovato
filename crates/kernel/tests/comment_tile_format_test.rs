#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A comment body and a custom HTML tile body are saved through the same
//! text format check an item field already runs (B268): a forbidden format
//! is refused and nothing is written, a permitted one saves, and a format
//! that does not exist at all is told apart from one the writer may not use.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::tile::{CreateTile, Tile, UpdateTile};
use trovato_kernel::models::{CreateComment, CreateItem, UpdateComment};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const ITEM_TYPE: &str = "b268_format_test";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec![])
}

/// A signed-in writer holding exactly `permissions`, with no admin bypass:
/// the shape `permitted_text_formats` reads, built by hand so the test does
/// not depend on which formats a seeded role happens to grant.
fn writer(permissions: &[&str]) -> UserContext {
    UserContext::authenticated(
        Uuid::now_v7(),
        permissions.iter().map(|p| p.to_string()).collect(),
    )
}

async fn ensure_item_type(app: &TestApp) {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'B268 format test', 'fixture type', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .execute(&app.db)
    .await
    .expect("seed item type");
}

async fn create_item(app: &TestApp) -> Uuid {
    // The comment service panics unless its plugin gate is on.
    app.ensure_plugin_enabled("trovato_comments").await;
    ensure_item_type(app).await;
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: format!("B268 {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b268 fixture".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

fn comment_input(item_id: Uuid, format: &str) -> CreateComment {
    CreateComment {
        item_id,
        parent_id: None,
        author_id: Uuid::nil(),
        subject: None,
        body: "<p>hi</p>".to_string(),
        body_format: Some(format.to_string()),
        status: Some(1),
    }
}

async fn comment_count(app: &TestApp, item_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE item_id = $1")
        .bind(item_id)
        .fetch_one(&app.db)
        .await
        .expect("count comments")
}

#[test]
fn a_comment_in_a_forbidden_format_is_refused_and_nothing_is_written() {
    run_test(async {
        let app = shared_app().await;
        let item_id = create_item(app).await;
        // Holds `post comments` but not `use full_html`.
        let user = writer(&["post comments"]);

        let err = app
            .state
            .comments()
            .create(comment_input(item_id, "full_html"), &user)
            .await
            .expect_err("full_html is refused");
        assert_eq!(
            err.to_string(),
            "Comment: you may not use the full_html text format."
        );
        assert_eq!(comment_count(app, item_id).await, 0, "nothing was written");
    });
}

#[test]
fn a_comment_in_a_permitted_format_saves() {
    run_test(async {
        let app = shared_app().await;
        let item_id = create_item(app).await;
        let user = writer(&["post comments", "use filtered_html"]);

        let comment = app
            .state
            .comments()
            .create(comment_input(item_id, "filtered_html"), &user)
            .await
            .expect("filtered_html is permitted");
        assert_eq!(comment.body_format, "filtered_html");
        assert_eq!(comment_count(app, item_id).await, 1);
    });
}

#[test]
fn a_comment_in_a_missing_format_names_it_missing_not_forbidden() {
    run_test(async {
        let app = shared_app().await;
        let item_id = create_item(app).await;
        let user = writer(&["post comments"]);

        let err = app
            .state
            .comments()
            .create(comment_input(item_id, "b268_nope"), &user)
            .await
            .expect_err("b268_nope does not exist");
        assert_eq!(
            err.to_string(),
            "Comment: the b268_nope text format does not exist."
        );
        assert_eq!(comment_count(app, item_id).await, 0, "nothing was written");
    });
}

#[test]
fn editing_a_comment_into_a_forbidden_format_is_refused_leaving_it_unchanged() {
    run_test(async {
        let app = shared_app().await;
        let item_id = create_item(app).await;
        let owner = writer(&["post comments", "use filtered_html"]);
        let comment = app
            .state
            .comments()
            .create(comment_input(item_id, "filtered_html"), &owner)
            .await
            .expect("create the comment");

        let moderator = writer(&["administer comments"]);
        let input = UpdateComment {
            body: Some("<script>no</script>".to_string()),
            body_format: Some("full_html".to_string()),
            status: None,
            subject: None,
        };
        let err = app
            .state
            .comments()
            .update(comment.id, input, &moderator)
            .await
            .expect_err("full_html is refused");
        assert_eq!(
            err.to_string(),
            "Comment: you may not use the full_html text format."
        );

        let stored = app
            .state
            .comments()
            .load(comment.id)
            .await
            .expect("load")
            .expect("still there");
        assert_eq!(stored.body_format, "filtered_html", "left unchanged");
        assert_eq!(stored.body, comment.body, "left unchanged");
    });
}

fn tile_input(machine_name: &str, format: &str) -> CreateTile {
    CreateTile {
        machine_name: machine_name.to_string(),
        label: "B268 tile".to_string(),
        region: Some("sidebar".to_string()),
        tile_type: Some("custom_html".to_string()),
        config: Some(serde_json::json!({ "body": "<p>hi</p>", "format": format })),
        visibility: None,
        weight: None,
        status: None,
        plugin: None,
        stage_id: None,
    }
}

fn unique_machine_name() -> String {
    format!("b268_{}", Uuid::now_v7().simple())
}

#[test]
fn a_tile_in_a_forbidden_format_is_refused_and_nothing_is_written() {
    run_test(async {
        let app = shared_app().await;
        // Holds `administer site` (the tile admin gate) but not `use full_html`.
        let user = writer(&["administer site"]);
        let machine = unique_machine_name();

        let err = app
            .state
            .tiles()
            .create(tile_input(&machine, "full_html"), &user)
            .await
            .expect_err("full_html is refused");
        assert_eq!(
            err.to_string(),
            "Custom HTML tile: you may not use the full_html text format."
        );
        let found = Tile::find_by_machine_name(&app.db, &machine)
            .await
            .expect("query");
        assert!(found.is_none(), "nothing was written");
    });
}

#[test]
fn a_tile_in_a_permitted_format_saves() {
    run_test(async {
        let app = shared_app().await;
        let user = writer(&["administer site", "use filtered_html"]);
        let machine = unique_machine_name();

        let tile = app
            .state
            .tiles()
            .create(tile_input(&machine, "filtered_html"), &user)
            .await
            .expect("filtered_html is permitted");
        assert_eq!(tile.config["format"], "filtered_html");
        app.state.tiles().delete(tile.id).await.ok();
    });
}

#[test]
fn a_tile_in_a_missing_format_names_it_missing_not_forbidden() {
    run_test(async {
        let app = shared_app().await;
        let user = writer(&["administer site"]);
        let machine = unique_machine_name();

        let err = app
            .state
            .tiles()
            .create(tile_input(&machine, "b268_nope"), &user)
            .await
            .expect_err("b268_nope does not exist");
        assert_eq!(
            err.to_string(),
            "Custom HTML tile: the b268_nope text format does not exist."
        );
        let found = Tile::find_by_machine_name(&app.db, &machine)
            .await
            .expect("query");
        assert!(found.is_none(), "nothing was written");
    });
}

#[test]
fn editing_a_tile_into_a_forbidden_format_is_refused_leaving_it_unchanged() {
    run_test(async {
        let app = shared_app().await;
        let owner = writer(&["administer site", "use filtered_html"]);
        let machine = unique_machine_name();
        let tile = app
            .state
            .tiles()
            .create(tile_input(&machine, "filtered_html"), &owner)
            .await
            .expect("create the tile");

        let other = writer(&["administer site"]);
        let update = UpdateTile {
            label: None,
            region: None,
            tile_type: None,
            config: Some(
                serde_json::json!({ "body": "<script>no</script>", "format": "full_html" }),
            ),
            visibility: None,
            weight: None,
            status: None,
            plugin: None,
            stage_id: None,
        };
        let err = app
            .state
            .tiles()
            .update(tile.id, update, &other)
            .await
            .expect_err("full_html is refused");
        assert_eq!(
            err.to_string(),
            "Custom HTML tile: you may not use the full_html text format."
        );

        let stored = Tile::find_by_machine_name(&app.db, &machine)
            .await
            .expect("query")
            .expect("still there");
        assert_eq!(stored.config["format"], "filtered_html", "left unchanged");
        app.state.tiles().delete(tile.id).await.ok();
    });
}
