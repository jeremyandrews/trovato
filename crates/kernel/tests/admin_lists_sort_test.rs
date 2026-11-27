#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Admin listings sort titles in the order a person expects (B246).
//!
//! - SQL (`natural!`, the `trovato_natural` collation) and the in-memory
//!   `admin_list::sort_rows` put the same list in the same order.
//! - Every listing answers every one of its sort keys, both ways: a sort
//!   expression the database refuses would be a 500.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::routes::admin_list::{Sort, sort_rows};

const PARITY: [&str; 7] = [
    "Éclair", "eclair", "Zebra", "Post 02", "Post 2", "Post 10", "post 1",
];

/// The same list, sorted by Postgres under the listings' collation and by
/// `sort_rows`, comes out in the same order, both ways.
#[test]
fn sql_and_sort_rows_agree_on_natural_order() {
    run_test(async {
        let app = shared_app().await;
        let values: Vec<String> = PARITY.iter().map(|s| s.to_string()).collect();
        for desc in [false, true] {
            let sql: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT v FROM unnest($1::text[]) v ORDER BY lower(v) COLLATE trovato_natural {}",
                if desc { "DESC" } else { "ASC" }
            )))
            .bind(&values)
            .fetch_all(&app.db)
            .await
            .unwrap();
            let mut rows = values.clone();
            let sort = Sort {
                key: "v",
                desc,
                sql: "v",
            };
            sort_rows(&mut rows, &sort, &[("v", |r: &String| r.clone())]);
            assert_eq!(rows, sql, "desc = {desc}");
        }
        let ascending: Vec<String> = sqlx::query_scalar(
            "SELECT v FROM unnest($1::text[]) v ORDER BY lower(v) COLLATE trovato_natural",
        )
        .bind(&values)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(
            ascending,
            [
                "eclair", "Éclair", "post 1", "Post 02", "Post 2", "Post 10", "Zebra"
            ]
        );
    });
}

/// Each `*_SORTS` list in the routes, with the listing it sorts.
const LISTINGS: &[(&str, &str)] = &[
    ("FILE_SORTS", "/admin/content/files"),
    ("COMMENT_SORTS", "/admin/content/comments"),
    ("ALIAS_SORTS", "/admin/structure/aliases"),
    ("AUDIT_SORTS", "/admin/reports/audit"),
    ("CONTENT_SORTS", "/admin/content"),
    ("MENU_SORTS", "/admin/structure/menus"),
    ("LOG_SORTS", "/admin/reports/log"),
    ("CATEGORY_SORTS", "/admin/structure/categories"),
    ("USER_SORTS", "/admin/people"),
    ("GATHER_SORTS", "/admin/gather"),
];

/// Every `*_SORTS` list in the route sources and its keys, read from the
/// source, so a new list or key is requested too.
fn sort_lists() -> Vec<(String, Vec<String>)> {
    let dir = common::project_root().join("crates/kernel/src/routes");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (at, _) in text.match_indices("_SORTS: &[(&str, &str)] = &[") {
            let start = text[..at].rfind("const ").unwrap() + "const ".len();
            let name = text[start..at + "_SORTS".len()].to_string();
            let body = &text[at..];
            let body = &body[..body.find("];").unwrap()];
            let keys: Vec<String> = body
                .match_indices("(\"")
                .map(|(i, _)| {
                    let rest = &body[i + 2..];
                    rest[..rest.find('"').unwrap()].to_string()
                })
                .collect();
            found.push((name, keys));
        }
    }
    found.sort();
    found
}

async fn status(app: &TestApp, cookies: &str, uri: &str) -> StatusCode {
    app.request_with_cookies(
        Request::get(uri)
            .header("x-forwarded-for", test_ip_for("b246sort"))
            .body(Body::empty())
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

#[test]
fn every_listing_answers_every_sort_key_both_ways() {
    run_test(async {
        let app = shared_app().await;
        // Three listings belong to plugins: each answers only while its plugin
        // is on, and reads tables that, on a fresh database, exist only once
        // the plugin's migrations have run.
        for plugin in ["trovato_audit_log", "trovato_categories", "trovato_comments"] {
            common::ensure_plugin_tables(app, plugin);
            app.ensure_plugin_enabled(plugin).await;
        }
        let name = common::username("b246sort");
        let pool = app.db.clone();
        let prefix = name.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                .bind(prefix)
                .execute(&pool)
                .await;
        });
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let lists = sort_lists();
        assert_eq!(
            lists.len(),
            LISTINGS.len(),
            "every sort list has its listing: {lists:?}"
        );
        let mut checked = 0;
        for (name, keys) in &lists {
            let path = LISTINGS
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, p)| *p)
                .unwrap_or_else(|| panic!("{name} sorts no listing this test knows"));
            assert!(!keys.is_empty(), "{name}");
            for key in keys {
                for sort in [key.clone(), format!("-{key}")] {
                    let uri = format!("{path}?sort={sort}");
                    assert_eq!(status(app, &cookies, &uri).await, StatusCode::OK, "{uri}");
                    checked += 1;
                }
            }
        }
        assert!(checked >= 2 * LISTINGS.len());
    });
}
