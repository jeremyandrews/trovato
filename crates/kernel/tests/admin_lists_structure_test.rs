#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The listing pattern on the structure and report listings: gathers, menus
//! and a menu's links, tiles, categories and their tags, URL aliases, the
//! site log and the audit log.
//!
//! Each listing takes a text filter `q` (GET). Where a listing sorts, a
//! `sort` parameter orders it both ways and the sorted header carries
//! `aria-sort`; an unknown sort key falls back to the default. Where a
//! listing pages, a pager link keeps `q`. A filter that matches nothing says
//! so rather than claiming the listing is empty.
//!
//! Every fixture carries a random tag, so a shared database does not
//! interfere, and every fixture is removed when its test ends.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::gather::{GatherQuery, QueryDefinition, QueryDisplay};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateUrlAlias, UrlAlias};
use trovato_kernel::system_log::{self, LogRecord};
use uuid::Uuid;

// =============================================================================
// Helpers
// =============================================================================

/// A random tag: the random tail of a UUIDv7, lowercase hex.
fn tag() -> String {
    Uuid::now_v7().simple().to_string()[20..].to_string()
}

/// Remove the audit rows a fixture's writes left, when the audit plugin is
/// on (its table may not exist, which is fine).
async fn forget_audit(app: &TestApp, entity_id: &str) {
    let _ = sqlx::query("DELETE FROM audit_log WHERE entity_id = $1")
        .bind(entity_id)
        .execute(&app.db)
        .await;
}

/// Sign in a fresh admin, removed when the test ends.
async fn admin(app: &'static TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    let pool = app.db.clone();
    let cleanup = name.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query(
            "DELETE FROM audit_log WHERE entity_id IN (SELECT id::text FROM users WHERE name = $1)",
        )
        .bind(&cleanup)
        .execute(&pool)
        .await;
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&cleanup)
            .execute(&pool)
            .await;
    });
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// GET `uri` and return the page, asserting a 200.
async fn page(app: &TestApp, cookies: &str, uri: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    let html = String::from_utf8_lossy(&bytes).replace("&#x2F;", "/");
    assert_eq!(status, StatusCode::OK, "GET {uri}: {html}");
    html
}

/// The sorted header: its direction and its key.
fn sorted_header(html: &str) -> Option<(String, String)> {
    let at = html.find("aria-sort=\"")?;
    let rest = &html[at + 11..];
    let dir = rest[..rest.find('"')?].to_string();
    let end = rest.find("</th>")?;
    let th = &rest[..end];
    let v = th.find("value=\"")? + 7;
    let value = &th[v..v + th[v..].find('"')?];
    Some((dir, value.trim_start_matches('-').to_string()))
}

/// Assert the page is sorted by `key` in `dir`.
fn assert_sorted(html: &str, key: &str, dir: &str, what: &str) {
    assert_eq!(
        sorted_header(html),
        Some((dir.to_string(), key.to_string())),
        "{what}: the {key} header says it is sorted {dir}"
    );
}

/// Assert `first` appears before `second`.
fn assert_before(html: &str, first: &str, second: &str, what: &str) {
    let a = html
        .find(first)
        .unwrap_or_else(|| panic!("{what}: {first} is listed"));
    let b = html
        .find(second)
        .unwrap_or_else(|| panic!("{what}: {second} is listed"));
    assert!(a < b, "{what}: {first} comes before {second}");
}

/// The two-row filter checks every listing shares: `q` = `shared` lists
/// both, `q` = `alpha` lists only it, a `q` matching nothing shows the
/// filtered empty state with a link back to the bare listing.
async fn assert_filters(
    app: &TestApp,
    cookies: &str,
    base: &str,
    shared: &str,
    (alpha, omega): (&str, &str),
    no_match: &str,
) {
    let html = page(app, cookies, &format!("{base}?q={shared}")).await;
    assert!(html.contains(alpha), "{base}: q lists {alpha}");
    assert!(html.contains(omega), "{base}: q lists {omega}");
    assert!(html.contains("list-count"), "{base}: the result count");
    assert!(html.contains(r#"name="q""#), "{base}: the text filter");

    let html = page(app, cookies, &format!("{base}?q={alpha}")).await;
    assert!(html.contains(alpha), "{base}: q={alpha} lists it");
    assert!(!html.contains(omega), "{base}: q={alpha} hides {omega}");

    let html = page(app, cookies, &format!("{base}?q=nothing-{shared}-here")).await;
    assert!(
        html.contains(no_match),
        "{base}: the filtered empty state says {no_match:?}"
    );
    assert!(
        html.contains(&format!(r#"href="{base}""#)),
        "{base}: the filtered empty state links back to the bare listing"
    );
}

/// Sort checks: `key` ascending lists `alpha` first, descending `omega`
/// first, and the header says which.
async fn assert_sorts(
    app: &TestApp,
    cookies: &str,
    base: &str,
    query: &str,
    key: &str,
    (alpha, omega): (&str, &str),
) {
    let html = page(app, cookies, &format!("{base}?{query}&sort={key}")).await;
    assert_sorted(&html, key, "ascending", base);
    assert_before(&html, alpha, omega, base);
    let html = page(app, cookies, &format!("{base}?{query}&sort=-{key}")).await;
    assert_sorted(&html, key, "descending", base);
    assert_before(&html, omega, alpha, base);
}

// =============================================================================
// Gathers
// =============================================================================

async fn gather(app: &'static TestApp, id: &str, label: &str) {
    app.state
        .gather()
        .register_query(GatherQuery {
            query_id: id.to_string(),
            label: label.to_string(),
            description: None,
            definition: QueryDefinition::default(),
            display: QueryDisplay::default(),
            plugin: "core".to_string(),
            created: 1,
            changed: 1,
        })
        .await
        .expect("register gather");
    let id = id.to_string();
    common::defer_cleanup(move || async move {
        app.state.gather().delete_query(&id).await.ok();
    });
}

#[test]
fn the_gather_listing_filters_and_sorts() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "lstgather").await;
        let t = tag();
        let alpha = format!("alpha-{t}");
        let omega = format!("omega-{t}");
        gather(app, &format!("lst_{t}_z"), &alpha).await;
        gather(app, &format!("lst_{t}_a"), &omega).await;

        let base = "/admin/gather";
        assert_filters(
            app,
            &cookies,
            base,
            &t,
            (&alpha, &omega),
            "No gather query matches this filter.",
        )
        .await;
        assert_sorts(
            app,
            &cookies,
            base,
            &format!("q={t}"),
            "label",
            (&alpha, &omega),
        )
        .await;
        // By id the other way round: omega's id ends in `_a`.
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=id")).await;
        assert_sorted(&html, "id", "ascending", base);
        assert_before(&html, &omega, &alpha, base);

        // The default sort is by label, and an unknown key falls back to it.
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=bogus%3Bdrop")).await;
        assert_sorted(&html, "label", "ascending", base);
        assert_before(&html, &alpha, &omega, base);
        assert!(html.contains("table--stack"));
    });
}

// =============================================================================
// Menus and menu links
// =============================================================================

async fn menu_link(app: &'static TestApp, menu: &str, title: &str, path: &str) {
    let create = serde_json::from_value(serde_json::json!({
        "menu_name": menu, "path": path, "title": title
    }))
    .unwrap();
    let link = app
        .state
        .menu_links()
        .create(create)
        .await
        .expect("create link");
    common::defer_cleanup(move || async move {
        app.state.menu_links().delete(link.id).await.ok();
        forget_audit(app, &link.id.to_string()).await;
    });
}

#[test]
fn the_menu_listing_filters_and_sorts() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "lstmenu").await;
        let t = tag();
        let alpha = format!("alpha-{t}");
        let omega = format!("omega-{t}");
        menu_link(app, &alpha, &format!("first-{t}"), "/first").await;
        menu_link(app, &alpha, &format!("second-{t}"), "/second").await;
        menu_link(app, &omega, &format!("only-{t}"), "/only").await;

        let base = "/admin/structure/menus";
        assert_filters(
            app,
            &cookies,
            base,
            &t,
            (&alpha, &omega),
            "No menu matches this filter.",
        )
        .await;
        assert_sorts(
            app,
            &cookies,
            base,
            &format!("q={t}"),
            "name",
            (&alpha, &omega),
        )
        .await;
        // By link count: omega has one, alpha two.
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=links")).await;
        assert_sorted(&html, "links", "ascending", base);
        assert_before(&html, &format!(">{omega}<"), &format!(">{alpha}<"), base);

        let html = page(app, &cookies, &format!("{base}?sort=nope")).await;
        assert_sorted(&html, "name", "ascending", base);
        assert!(html.contains("table--stack"));

        // A menu's links: filtered, in their weight order, with no sort.
        let links = format!("/admin/structure/menus/{alpha}");
        let first = format!("first-{t}");
        let second = format!("second-{t}");
        let html = page(app, &cookies, &links).await;
        assert!(html.contains(&first) && html.contains(&second));
        assert!(html.contains("table--stack"));
        assert!(!html.contains("aria-sort"), "the links keep their order");
        let html = page(app, &cookies, &format!("{links}?q={first}")).await;
        assert!(html.contains(&first));
        assert!(!html.contains(&second), "q narrows the links");
        let html = page(app, &cookies, &format!("{links}?q=none-{t}")).await;
        assert!(html.contains("No link in this menu matches this filter."));
        assert!(html.contains(&format!(r#"href="{links}""#)));
    });
}

// =============================================================================
// Tiles
// =============================================================================

async fn tile(app: &'static TestApp, machine_name: &str, label: &str) {
    let create = serde_json::from_value(serde_json::json!({
        "machine_name": machine_name, "label": label, "region": "sidebar"
    }))
    .unwrap();
    let tile = app.state.tiles().create(create).await.expect("create tile");
    common::defer_cleanup(move || async move {
        app.state.tiles().delete(tile.id).await.ok();
        forget_audit(app, &tile.id.to_string()).await;
    });
}

#[test]
fn the_tile_listing_filters() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "lsttile").await;
        let t = tag();
        let alpha = format!("alpha-{t}");
        let omega = format!("omega-{t}");
        tile(app, &format!("alpha_{t}"), &alpha).await;
        tile(app, &format!("omega_{t}"), &omega).await;

        let base = "/admin/structure/tiles";
        assert_filters(
            app,
            &cookies,
            base,
            &t,
            (&alpha, &omega),
            "No tile matches this filter.",
        )
        .await;
        // Tiles keep their region and weight order: an unknown sort is
        // ignored and nothing claims a sort.
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=label")).await;
        assert!(!html.contains("aria-sort"));
        assert_before(&html, &alpha, &omega, base);
        assert!(html.contains("table--stack"));
    });
}

// =============================================================================
// Categories and tags
// =============================================================================

async fn category(app: &'static TestApp, id: &str, label: &str) {
    let create = serde_json::from_value(serde_json::json!({ "id": id, "label": label })).unwrap();
    app.state
        .categories()
        .create_category(create)
        .await
        .expect("create category");
    let id = id.to_string();
    common::defer_cleanup(move || async move {
        app.state.categories().delete_category(&id).await.ok();
        forget_audit(app, &id).await;
    });
}

#[test]
fn the_category_and_tag_listings_filter_and_sort() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_categories").await;
        let cookies = admin(app, "lstcat").await;
        let t = tag();
        let alpha = format!("alpha-{t}");
        let omega = format!("omega-{t}");
        let alpha_id = format!("lst_{t}_z");
        category(app, &alpha_id, &alpha).await;
        category(app, &format!("lst_{t}_a"), &omega).await;

        let base = "/admin/structure/categories";
        assert_filters(
            app,
            &cookies,
            base,
            &t,
            (&alpha, &omega),
            "No category matches this filter.",
        )
        .await;
        assert_sorts(
            app,
            &cookies,
            base,
            &format!("q={t}"),
            "name",
            (&alpha, &omega),
        )
        .await;
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=id")).await;
        assert_sorted(&html, "id", "ascending", base);
        assert_before(&html, &omega, &alpha, base);
        // The default is the stored order (weight, then name), which no
        // header claims; an unknown key falls back to it.
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=x")).await;
        assert_eq!(sorted_header(&html), None);
        assert_before(&html, &alpha, &omega, base);
        assert!(html.contains("table--stack"));

        // Tags: filtered, kept in their weight order.
        for label in ["first", "second"] {
            let input = serde_json::from_value(serde_json::json!({
                "category_id": alpha_id, "label": format!("{label}-{t}")
            }))
            .unwrap();
            let tag = app.state.categories().create_tag(input).await.unwrap();
            common::defer_cleanup(move || async move {
                forget_audit(app, &tag.id.to_string()).await;
            });
        }
        let tags = format!("/admin/structure/categories/{alpha_id}/tags");
        let html = page(app, &cookies, &tags).await;
        assert!(html.contains(&format!("first-{t}")) && html.contains(&format!("second-{t}")));
        assert!(!html.contains("aria-sort"));
        assert!(html.contains("table--stack"));
        let html = page(app, &cookies, &format!("{tags}?q=first-{t}")).await;
        assert!(html.contains(&format!("first-{t}")));
        assert!(!html.contains(&format!("second-{t}")));
        let html = page(app, &cookies, &format!("{tags}?q=none-{t}")).await;
        assert!(html.contains("No tag matches this filter."));
        assert!(html.contains(&format!(r#"href="{tags}""#)));
    });
}

// =============================================================================
// URL aliases
// =============================================================================

#[test]
fn the_alias_listing_filters_sorts_and_pages() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "lstalias").await;
        let t = tag();
        let pool = app.db.clone();
        let cleanup = t.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM url_alias WHERE alias LIKE $1")
                .bind(format!("%{cleanup}%"))
                .execute(&pool)
                .await;
        });
        let create = |alias: String, source: String| CreateUrlAlias {
            source,
            alias,
            language: Some("en".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        };
        let alpha = format!("/alpha-{t}-pair");
        let omega = format!("/omega-{t}-pair");
        UrlAlias::create(&app.db, create(alpha.clone(), "/item/zzz".into()))
            .await
            .unwrap();
        UrlAlias::create(&app.db, create(omega.clone(), "/item/aaa".into()))
            .await
            .unwrap();
        for i in 0..51 {
            UrlAlias::create(
                &app.db,
                create(format!("/bulk-{t}-{i:03}"), "/item/b".into()),
            )
            .await
            .unwrap();
        }

        let base = "/admin/structure/aliases";
        let pair = format!("{t}-pair");
        let html = page(app, &cookies, &format!("{base}?q={pair}")).await;
        assert!(html.contains(&alpha) && html.contains(&omega));
        assert!(!html.contains(&format!("/bulk-{t}-")), "q narrows the rows");
        let html = page(app, &cookies, &format!("{base}?q=/alpha-{t}")).await;
        assert!(html.contains(&alpha) && !html.contains(&omega));
        // By source: omega's is `/item/aaa`.
        let html = page(app, &cookies, &format!("{base}?q={pair}&sort=source")).await;
        assert_before(&html, &omega, &alpha, base);
        let html = page(app, &cookies, &format!("{base}?q=nothing-{t}")).await;
        assert!(html.contains("No URL alias matches this filter."));
        assert!(html.contains(&format!(r#"href="{base}""#)));

        assert_sorts(
            app,
            &cookies,
            base,
            &format!("q={pair}"),
            "alias",
            (&alpha, &omega),
        )
        .await;
        // Newest first by default, and for an unknown key.
        let html = page(app, &cookies, &format!("{base}?q={pair}&sort=1%20OR%201")).await;
        assert_sorted(&html, "created", "descending", base);

        // 53 rows: two pages, and the pager keeps the filter and the sort.
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=alias")).await;
        assert!(html.contains("table--stack"));
        assert!(
            html.contains(&format!("{base}?q={t}&amp;sort=alias&amp;page=2")),
            "the pager keeps q and the sort"
        );
        let html = page(app, &cookies, &format!("{base}?q={t}&sort=alias&page=2")).await;
        assert!(html.contains(&omega), "the last row is on page 2");
        assert!(!html.contains(&alpha));
    });
}

// =============================================================================
// The site log
// =============================================================================

#[test]
fn the_log_filters_sorts_and_pages() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "lstlog").await;
        let t = tag();
        let pool = app.db.clone();
        let cleanup = t.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM system_log WHERE message LIKE $1")
                .bind(format!("%{cleanup}%"))
                .execute(&pool)
                .await;
        });
        let now = chrono::Utc::now().timestamp();
        let record = |level: &'static str, message: String| LogRecord {
            created: now,
            level,
            log_type: "lsttest".to_string(),
            message,
            context: Default::default(),
        };
        let alpha = format!("alpha {t}-pair");
        let omega = format!("omega {t}-pair");
        system_log::insert(
            &app.db,
            &[
                record("error", alpha.clone()),
                record("info", omega.clone()),
            ],
        )
        .await
        .unwrap();
        let bulk: Vec<LogRecord> = (0..51)
            .map(|i| record("warn", format!("bulk {t} {i}")))
            .collect();
        system_log::insert(&app.db, &bulk).await.unwrap();

        let base = "/admin/reports/log";
        let pair = format!("{t}-pair");
        let html = page(app, &cookies, &format!("{base}?q={pair}")).await;
        assert!(html.contains(&alpha) && html.contains(&omega));
        assert!(!html.contains(&format!("bulk {t}")));
        // The existing selects still filter, with the text filter.
        let html = page(
            app,
            &cookies,
            &format!("{base}?q={pair}&level=error&type=lsttest"),
        )
        .await;
        assert!(html.contains(&alpha) && !html.contains(&omega));
        let html = page(app, &cookies, &format!("{base}?q=nothing-{t}")).await;
        assert!(html.contains("No log message matches this filter."));
        assert!(html.contains(&format!(r#"href="{base}""#)));

        // Newest first by default; the time sort both ways.
        let html = page(app, &cookies, &format!("{base}?q={pair}")).await;
        assert_sorted(&html, "time", "descending", base);
        assert_before(&html, &omega, &alpha, base);
        assert_sorts(
            app,
            &cookies,
            base,
            &format!("q={pair}"),
            "time",
            (&alpha, &omega),
        )
        .await;
        // By level, the least severe first.
        let html = page(app, &cookies, &format!("{base}?q={pair}&sort=level")).await;
        assert_sorted(&html, "level", "ascending", base);
        assert_before(&html, &omega, &alpha, base);
        let html = page(app, &cookies, &format!("{base}?q={pair}&sort=drop")).await;
        assert_sorted(&html, "time", "descending", base);

        // 53 rows: the pager keeps the filter.
        let html = page(app, &cookies, &format!("{base}?q={t}&type=lsttest")).await;
        assert!(html.contains("table--stack"));
        assert!(
            html.contains(&format!("{base}?q={t}&amp;type=lsttest&amp;page=2")),
            "the pager keeps q and the type"
        );
        // The old page parameter still works.
        let html = page(app, &cookies, &format!("{base}?q={t}&type=lsttest&page=2")).await;
        assert!(html.contains(&format!("{base}?q={t}&amp;type=lsttest&amp;page=1")));
    });
}

// =============================================================================
// The audit log
// =============================================================================

#[test]
fn the_audit_log_filters_sorts_and_pages() {
    run_test(async {
        let app = shared_app().await;
        common::ensure_plugin_tables(app, "trovato_audit_log");
        app.ensure_plugin_enabled("trovato_audit_log").await;
        let cookies = admin(app, "lstaudit").await;
        let t = tag();
        let pool = app.db.clone();
        let cleanup = t.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM audit_log WHERE entity_id LIKE $1")
                .bind(format!("%{cleanup}%"))
                .execute(&pool)
                .await;
        });
        let now = chrono::Utc::now().timestamp();
        let insert = |action: &str, entity_id: String, created: i64| {
            sqlx::query(
                "INSERT INTO audit_log (action, entity_type, entity_id, created) \
                 VALUES ($1, 'lsttest', $2, $3)",
            )
            .bind(action.to_string())
            .bind(entity_id)
            .bind(created)
            .execute(&app.db)
        };
        let alpha = format!("alpha-{t}-pair");
        let omega = format!("omega-{t}-pair");
        insert("archive", alpha.clone(), now - 10).await.unwrap();
        insert("update", omega.clone(), now - 5).await.unwrap();
        for i in 0..51 {
            insert("create", format!("bulk-{t}-{i}"), now - 20)
                .await
                .unwrap();
        }

        let base = "/admin/reports/audit";
        let pair = format!("{t}-pair");
        let html = page(app, &cookies, &format!("{base}?q={pair}")).await;
        assert!(html.contains(&alpha) && html.contains(&omega));
        assert!(!html.contains(&format!("bulk-{t}")));
        let html = page(app, &cookies, &format!("{base}?q=alpha-{t}")).await;
        assert!(html.contains(&alpha) && !html.contains(&omega));
        let html = page(app, &cookies, &format!("{base}?q=nothing-{t}")).await;
        assert!(html.contains("No audit entry matches this filter."));
        assert!(html.contains(&format!(r#"href="{base}""#)));

        // Newest first by default.
        let html = page(app, &cookies, &format!("{base}?q={pair}")).await;
        assert_sorted(&html, "time", "descending", base);
        assert_before(&html, &omega, &alpha, base);
        assert_sorts(
            app,
            &cookies,
            base,
            &format!("q={pair}"),
            "time",
            (&alpha, &omega),
        )
        .await;
        assert_sorts(
            app,
            &cookies,
            base,
            &format!("q={pair}"),
            "action",
            (&alpha, &omega),
        )
        .await;
        let html = page(app, &cookies, &format!("{base}?q={pair}&sort=a.id")).await;
        assert_sorted(&html, "time", "descending", base);

        let html = page(app, &cookies, &format!("{base}?q={t}")).await;
        assert!(html.contains("table--stack"));
        assert!(
            html.contains(&format!("{base}?q={t}&amp;page=2")),
            "the pager keeps q"
        );
    });
}
