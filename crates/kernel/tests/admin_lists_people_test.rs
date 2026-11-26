#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The listing pattern on the people, comments, files and media listings.
//!
//! Each listing takes a text filter `q` (with its own selects), sorts by its
//! columns from a fixed list, pages with links that keep the filter, says how
//! many rows matched, and tells a filter that matched nothing apart from a
//! listing that is empty. Before, people loaded every account on one page,
//! files stopped at the hundredth, and the comments and media pagers were
//! hand-written links.
//!
//! Every row a test makes carries a fresh tag, so a shared database does not
//! interfere, and is deleted when the test ends. Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, test_ip_for};
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";

/// A fresh tag: lowercase hex, so it is never an ILIKE wildcard.
fn tag() -> String {
    format!("lp{}", Uuid::now_v7().simple())
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b199lists-people"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        String::from_utf8_lossy(&bytes)
            .replace("&#x2F;", "/")
            .replace("&amp;", "&"),
    )
}

/// An administrator's session; the account goes when the test ends. Returns
/// the cookies and the account's id.
async fn admin(app: &TestApp) -> (String, Uuid) {
    let name = common::username("lpadmin");
    let cookies = app
        .create_and_login_admin(&name, PASSWORD, &format!("{name}@test.local"))
        .await;
    let id = common::user_id_of(app, &name).await;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    (cookies, id)
}

/// Where `needle` first appears in `html`, which must contain it.
fn at(html: &str, needle: &str) -> usize {
    html.find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is on the page"))
}

/// The page's table body, so a header's or the filter's words are not taken
/// for a row.
fn tbody(html: &str) -> &str {
    let start = html.find("<tbody>").expect("a table body");
    let end = start + html[start..].find("</tbody>").unwrap();
    &html[start..end]
}

/// The sort parameter is honoured both ways and marked on its header, and an
/// unknown one falls back to the default without an error.
async fn check_sort(
    app: &TestApp,
    cookies: &str,
    base: &str,
    key: &str,
    first: &str,
    second: &str,
) {
    let (status, html) = get(app, cookies, &format!("{base}&sort={key}")).await;
    assert_eq!(status, StatusCode::OK);
    let body = tbody(&html);
    assert!(at(body, first) < at(body, second), "{base} sorted by {key}");
    assert!(html.contains(r#"aria-sort="ascending""#), "{base}: ascending is marked");

    let (_, html) = get(app, cookies, &format!("{base}&sort=-{key}")).await;
    let body = tbody(&html);
    assert!(at(body, second) < at(body, first), "{base} sorted by -{key}");
    assert!(html.contains(r#"aria-sort="descending""#), "{base}: descending is marked");

    let (status, html) = get(app, cookies, &format!("{base}&sort=id%3B+DROP+TABLE+x")).await;
    assert_eq!(status, StatusCode::OK, "{base}: an unknown sort is not an error");
    let body = tbody(&html);
    assert!(body.contains(first) && body.contains(second));
    assert!(html.contains("aria-sort="), "{base}: the default sort is marked");
}

// =============================================================================
// People
// =============================================================================

#[test]
fn people_filter_sort_page_and_empty_states() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, _) = admin(app).await;
        let tag = tag();
        let pool = app.db.clone();
        let like = format!("{tag}%");
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE name LIKE $1")
                .bind(like)
                .execute(&pool)
                .await;
        });

        let alpha = format!("{tag}-alpha");
        let beta = format!("{tag}-beta");
        for (name, mail) in [(&alpha, "zz"), (&beta, "aa")] {
            sqlx::query(
                "INSERT INTO users (id, name, pass, mail) VALUES ($1, $2, '', $3)",
            )
            .bind(Uuid::now_v7())
            .bind(name)
            .bind(format!("{mail}-{name}@test.local"))
            .execute(&app.db)
            .await
            .unwrap();
        }

        // The text filter narrows to the matching row, by name or by email.
        let (status, html) = get(app, &cookies, &format!("/admin/people?q={alpha}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&alpha));
        assert!(!html.contains(&beta), "the filter leaves out the other row");
        assert!(html.contains("1 result"), "the count says how many matched");
        assert!(html.contains("table--stack"));
        assert!(html.contains(&format!("Delete {alpha}")), "a row action names its object");
        let (_, html) = get(app, &cookies, &format!("/admin/people?q=aa-{tag}")).await;
        assert!(html.contains(&beta) && !html.contains(&alpha), "email matches too");

        // Sorted by name and by email, which order the two the other way.
        let base = format!("/admin/people?q={tag}");
        check_sort(app, &cookies, &base, "name", &alpha, &beta).await;
        check_sort(app, &cookies, &base, "email", &beta, &alpha).await;

        // A filter that matches nothing says so, and offers to clear it.
        let (_, html) = get(app, &cookies, &format!("/admin/people?q=none-{tag}")).await;
        assert!(html.contains("No users match this filter."));
        assert!(html.contains(r#"href="/admin/people""#));

        // A second page, whose links keep the filter.
        sqlx::query(
            "INSERT INTO users (id, name, pass, mail) \
             SELECT gen_random_uuid(), $1 || '-many-' || lpad(g::text, 3, '0'), '', \
                    $1 || '-' || g || '@test.local' \
             FROM generate_series(1, 51) g",
        )
        .bind(&tag)
        .execute(&app.db)
        .await
        .unwrap();
        let (_, html) = get(app, &cookies, &format!("/admin/people?q={tag}&sort=-name")).await;
        assert!(html.contains("53 results, page 1 of 2"), "the count names the pages");
        assert!(
            html.contains(&format!("/admin/people?q={tag}&sort=-name&page=2")),
            "the pager keeps the filter and the sort"
        );
        let (_, html) = get(app, &cookies, &format!("/admin/people?q={tag}&sort=-name&page=2")).await;
        assert!(html.contains("53 results, page 2 of 2"));
        assert!(html.contains(&alpha), "the last name sorts onto the last page");
    });
}

// =============================================================================
// Comments
// =============================================================================

/// A page to comment on; it and its comments go when the test ends.
async fn commented_item(app: &TestApp, author: Uuid, tag: &str) -> Uuid {
    let item_id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, promote, \
         sticky, fields, language) \
         VALUES ($1, 'page', $2, $3, 1, $4, $4, 0, 0, '{}'::jsonb, 'en')",
    )
    .bind(item_id)
    .bind(format!("Commented {tag}"))
    .bind(author)
    .bind(now)
    .execute(&app.db)
    .await
    .unwrap();
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM comment WHERE item_id = $1")
            .bind(item_id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(item_id)
            .execute(&pool)
            .await;
    });
    item_id
}

async fn comment(app: &TestApp, item: Uuid, author: Uuid, subject: &str, status: i16, created: i64) {
    sqlx::query(
        "INSERT INTO comment (id, item_id, author_id, subject, body, body_format, status, \
         created, changed) VALUES ($1, $2, $3, $4, $5, 'plain_text', $6, $7, $7)",
    )
    .bind(Uuid::now_v7())
    .bind(item)
    .bind(author)
    .bind(subject)
    .bind(format!("The body of {subject}"))
    .bind(status)
    .bind(created)
    .execute(&app.db)
    .await
    .unwrap();
}

#[test]
fn comments_filter_sort_page_and_empty_states() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        let (cookies, admin_id) = admin(app).await;
        let tag = tag();
        let item = commented_item(app, admin_id, &tag).await;

        // A second author, to filter by.
        let writer = format!("{tag}-writer");
        let writer_id = Uuid::now_v7();
        sqlx::query("INSERT INTO users (id, name, pass, mail) VALUES ($1, $2, '', $3)")
            .bind(writer_id)
            .bind(&writer)
            .bind(format!("{writer}@test.local"))
            .execute(&app.db)
            .await
            .unwrap();
        let pool = app.db.clone();
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM comment WHERE author_id = $1")
                .bind(writer_id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(writer_id)
                .execute(&pool)
                .await;
        });

        let now = chrono::Utc::now().timestamp();
        let older = format!("{tag}-older");
        let newer = format!("{tag}-newer");
        comment(app, item, admin_id, &older, 1, now - 100).await;
        comment(app, item, writer_id, &newer, 0, now).await;

        // By subject or body, and by author.
        let (status, html) =
            get(app, &cookies, &format!("/admin/content/comments?q={older}")).await;
        assert_eq!(status, StatusCode::OK);
        let body = tbody(&html);
        assert!(body.contains(&older) && !body.contains(&newer));
        assert!(html.contains("1 result"));
        assert!(html.contains("table--stack"));
        assert!(html.contains(&format!("Delete {older}")), "a row action names its object");
        let (_, html) = get(app, &cookies, &format!("/admin/content/comments?q={writer}")).await;
        let body = tbody(&html);
        assert!(body.contains(&newer) && !body.contains(&older), "author matches too");
        // The existing status select still filters, with the text filter.
        let (_, html) =
            get(app, &cookies, &format!("/admin/content/comments?q={tag}&status=0")).await;
        let body = tbody(&html);
        assert!(body.contains(&newer) && !body.contains(&older));

        let base = format!("/admin/content/comments?q={tag}");
        check_sort(app, &cookies, &base, "posted", &older, &newer).await;

        let (_, html) =
            get(app, &cookies, &format!("/admin/content/comments?q=none-{tag}")).await;
        assert!(html.contains("No comments match this filter."));
        assert!(html.contains(r#"href="/admin/content/comments""#));

        sqlx::query(
            "INSERT INTO comment (id, item_id, author_id, subject, body, body_format, status, \
             created, changed) \
             SELECT gen_random_uuid(), $1, $2, $3 || '-many-' || g, 'Many', 'plain_text', 1, \
                    $4 - 1000 - g, $4 - 1000 - g \
             FROM generate_series(1, 51) g",
        )
        .bind(item)
        .bind(admin_id)
        .bind(&tag)
        .bind(now)
        .execute(&app.db)
        .await
        .unwrap();
        let (_, html) =
            get(app, &cookies, &format!("/admin/content/comments?q={tag}&status=1")).await;
        assert!(html.contains("52 results, page 1 of 2"), "the count follows the filter");
        assert!(
            html.contains(&format!("/admin/content/comments?q={tag}&status=1&page=2")),
            "the pager keeps the filter"
        );
        // The old address still works.
        let (status, html) =
            get(app, &cookies, &format!("/admin/content/comments?q={tag}&status=1&page=2")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("52 results, page 2 of 2"));
    });
}

// =============================================================================
// Files and media
// =============================================================================

/// A managed file row, gone when the test ends.
async fn file(app: &TestApp, owner: Uuid, name: &str, mime: &str, size: i64, status: i16, created: i64) {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, \
         created, changed) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(id)
    .bind(owner)
    .bind(name)
    .bind(format!("local://lists-test/{id}"))
    .bind(mime)
    .bind(size)
    .bind(status)
    .bind(created)
    .execute(&app.db)
    .await
    .unwrap();
}

/// Delete every file whose name carries `tag` when the test ends.
fn forget_files(app: &TestApp, tag: &str) {
    let pool = app.db.clone();
    let like = format!("{tag}%");
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM file_managed WHERE filename LIKE $1")
            .bind(like)
            .execute(&pool)
            .await;
    });
}

/// Fifty-one more files whose names carry `tag`.
async fn many_files(app: &TestApp, owner: Uuid, tag: &str, now: i64) {
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, \
         created, changed) \
         SELECT gen_random_uuid(), $1, $2 || '-many-' || g || '.png', \
                'local://lists-test/' || $2 || '/' || g, 'image/png', 10, 1, $3 - 1000 - g, $3 \
         FROM generate_series(1, 51) g",
    )
    .bind(owner)
    .bind(tag)
    .bind(now)
    .execute(&app.db)
    .await
    .unwrap();
}

#[test]
fn files_filter_sort_page_and_empty_states() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, admin_id) = admin(app).await;
        let tag = tag();
        forget_files(app, &tag);
        let now = chrono::Utc::now().timestamp();
        let small = format!("{tag}-a-small.txt");
        let large = format!("{tag}-b-large.txt");
        file(app, admin_id, &small, "text/plain", 100, 0, now - 100).await;
        file(app, admin_id, &large, "text/plain", 900_000, 1, now).await;

        let (status, html) = get(app, &cookies, &format!("/admin/content/files?q={small}")).await;
        assert_eq!(status, StatusCode::OK);
        let body = tbody(&html);
        assert!(body.contains(&small) && !body.contains(&large));
        assert!(html.contains("1 result"));
        assert!(html.contains("table--stack"));
        assert!(html.contains(&format!("Delete {small}")), "a row action names its object");
        let (_, html) =
            get(app, &cookies, &format!("/admin/content/files?q={tag}&status=1")).await;
        let body = tbody(&html);
        assert!(body.contains(&large) && !body.contains(&small), "the status select still filters");

        let base = format!("/admin/content/files?q={tag}");
        check_sort(app, &cookies, &base, "name", &small, &large).await;
        check_sort(app, &cookies, &base, "size", &small, &large).await;
        check_sort(app, &cookies, &base, "uploaded", &small, &large).await;

        let (_, html) = get(app, &cookies, &format!("/admin/content/files?q=none-{tag}")).await;
        assert!(html.contains("No files match this filter."));
        assert!(html.contains(r#"href="/admin/content/files""#));

        many_files(app, admin_id, &tag, now).await;
        let (_, html) = get(app, &cookies, &format!("/admin/content/files?q={tag}")).await;
        assert!(html.contains("53 results, page 1 of 2"));
        assert!(
            html.contains(&format!("/admin/content/files?q={tag}&page=2")),
            "the pager keeps the filter"
        );
        let (_, html) = get(app, &cookies, &format!("/admin/content/files?q={tag}&page=2")).await;
        assert!(html.contains("53 results, page 2 of 2"));
    });
}

#[test]
fn media_filter_count_page_and_empty_states() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, admin_id) = admin(app).await;
        let tag = tag();
        forget_files(app, &tag);
        let now = chrono::Utc::now().timestamp();
        let picture = format!("{tag}-picture.png");
        let paper = format!("{tag}-paper.pdf");
        file(app, admin_id, &picture, "image/png", 10, 1, now).await;
        file(app, admin_id, &paper, "application/pdf", 10, 1, now).await;

        let (status, html) = get(app, &cookies, &format!("/admin/media?q={picture}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&picture) && !html.contains(&paper));
        assert!(html.contains("1 result"));
        let (_, html) = get(app, &cookies, &format!("/admin/media?q={tag}&type=document")).await;
        assert!(html.contains(&paper) && !html.contains(&picture), "the type select still filters");

        let (_, html) = get(app, &cookies, &format!("/admin/media?q=none-{tag}&type=all")).await;
        assert!(html.contains("No media matches this filter."));
        assert!(html.contains(r#"href="/admin/media""#));

        many_files(app, admin_id, &tag, now).await;
        let (_, html) = get(app, &cookies, &format!("/admin/media?q={tag}&type=image")).await;
        assert!(html.contains("52 results, page 1 of 2"));
        assert!(
            html.contains(&format!("/admin/media?q={tag}&type=image&page=2")),
            "the pager keeps the filter"
        );
        let (_, html) = get(app, &cookies, &format!("/admin/media?q={tag}&type=image&page=2")).await;
        assert!(html.contains("52 results, page 2 of 2"));
    });
}
