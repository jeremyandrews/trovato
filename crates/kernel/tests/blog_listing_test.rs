#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The blog listing shows a teaser of each post.
//!
//! `templates/gather/query--blog_listing.html` read `row.fields.body`.
//! `trovato_blog` defines the field as `field_body` (`tap_item_info`), so the
//! `is defined` guard around the teaser was never true and every post in the
//! listing rendered as a title, a date and a "Read more" link with no text
//! between them. `body` is the kernel `page` type's field name; the template was
//! written against the wrong content model.
//!
//! The listing is reached at `/gather/blog_listing`, which is what `/blog`
//! aliases to.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use uuid::Uuid;

/// Its own client address, so this file does not spend the shared rate-limit
/// bucket that requests without `X-Forwarded-For` fall into.
const CLIENT_IP: &str = "10.75.0.1";

/// Serializes the seeding below across tests, binaries and shards.
const BLOG_SEED_LOCK: i64 = 0x_B106_0000_0001;

const LIVE_STAGE: &str = "0193a5a0-0000-7000-8000-000000000001";

/// Seed the `blog` item type and `trovato_blog`'s own gather query.
///
/// The gather query lives in the plugin's migration, and the shared app
/// discovers no plugins (its plugin directory is relative to the test's working
/// directory), so neither row can be assumed: on a database where no earlier
/// test booted with the real plugins directory, `/gather/blog_listing` is a 404.
/// Both are seeded here, under one lock, so this file does not depend on shard
/// order. The migration is idempotent.
///
/// The gather service holds the query set in memory from startup, so the caller
/// has to reload it after this: a row written now is otherwise invisible, which
/// is why an app that booted against a database already carrying the row passed
/// while a fresh one answered "query not found".
fn ensure_blog_listing(app: &TestApp) {
    let dir = common::project_root().join("plugins/trovato_blog");
    let info = trovato_kernel::plugin::PluginInfo::parse(&dir.join("trovato_blog.info.toml"))
        .expect("parse the blog plugin manifest");
    let db = app.db.clone();
    let handle = common::shared_runtime_handle();

    std::thread::spawn(move || {
        handle.block_on(async move {
            let mut guard = db.begin().await.expect("begin blog seed");
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(BLOG_SEED_LOCK)
                .execute(&mut *guard)
                .await
                .expect("take the blog seed lock");

            // The content type `trovato_blog` declares, which the shared app
            // never syncs because it loads no plugins.
            sqlx::query(
                "INSERT INTO item_type (type, label, description, has_title, title_label, \
                 plugin, settings) \
                 VALUES ('blog', 'Blog Post', 'A blog entry with body and tags', true, 'Title', \
                 'trovato_blog', '{\"fields\": []}'::jsonb) \
                 ON CONFLICT (type) DO NOTHING",
            )
            .execute(&mut *guard)
            .await
            .expect("seed the blog item type");

            let result = trovato_kernel::plugin::migration::run_plugin_migrations(
                &db,
                "trovato_blog",
                &info,
                &dir,
            )
            .await;
            guard.commit().await.expect("release the blog seed lock");
            result.expect("run the blog plugin migrations");
        });
    })
    .join()
    .expect("blog seed thread panicked");
}

/// One published blog post with a body, returning its id and body text.
async fn seed_post(app: &TestApp, tag: &str) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let body = format!("The teaser text of post {tag}.");
    let author: Uuid = sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
        .fetch_one(&app.db)
        .await
        .expect("a user exists");

    sqlx::query(
        "INSERT INTO item (id, type, title, fields, status, author_id, stage_id, \
                           created, changed, language) \
         VALUES ($1, 'blog', $2, $3, 1, $4, $5, $6, $6, 'en')",
    )
    .bind(id)
    .bind(format!("Blog post {tag}"))
    .bind(serde_json::json!({"field_body": {"value": body}}))
    .bind(author)
    .bind(Uuid::parse_str(LIVE_STAGE).unwrap())
    .bind(chrono::Utc::now().timestamp())
    .execute(&app.db)
    .await
    .expect("seed a blog post");

    (id, body)
}

async fn get(app: &TestApp, path: &str) -> (StatusCode, String) {
    let response = app
        .request(
            Request::get(path)
                .header("x-forwarded-for", CLIENT_IP)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[test]
fn the_blog_listing_shows_each_post_s_teaser_text() {
    run_test(async {
        let app = shared_app().await;
        ensure_blog_listing(app);
        app.state
            .gather()
            .reload_from_db()
            .await
            .expect("reload the gather queries after seeding");
        let tag = Uuid::now_v7().simple().to_string();
        let (id, body) = seed_post(app, &tag).await;

        let (status, html) = get(app, "/gather/blog_listing").await;
        assert_eq!(status, StatusCode::OK, "GET /gather/blog_listing: {html}");

        // The post is in the listing at all, so a missing teaser is a missing
        // teaser rather than a missing row.
        assert!(
            html.contains(&format!("/item/{id}")),
            "the seeded post is listed: {html}"
        );
        assert!(
            html.contains("blog-teaser__summary"),
            "the teaser block is rendered: {html}"
        );
        assert!(
            html.contains(&body),
            "the teaser shows the post's body text: {html}"
        );

        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .expect("clean up the seeded post");
    });
}
