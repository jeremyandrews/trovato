#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A `gather_query` tile renders its gather's rows on the server (BL-99).
//!
//! It used to render `<div class="tile-gather" data-query-id="...">` and
//! nothing ever filled it, so a sidebar tile placed by an administrator was a
//! titled empty box. Each test here places a tile in the sidebar, drives a real
//! page through the router, and reads the tile back off it.
//!
//! Every gather, tile and item a test makes is removed when it ends: the
//! database is shared, and a stray sidebar tile would sit on every other test's
//! pages.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::tile::{CreateTile, Tile};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// A unique marker for this test's titles, machine names and gather ids.
///
/// Prefixed so it never parses as a UUID: a gather filter value is untagged,
/// and a bare 32-digit hex string would be read as a `Uuid` and compared in its
/// hyphenated form, matching no title.
fn stamp() -> String {
    format!("m{}", Uuid::now_v7().simple())
}

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A published conference titled `title`, by `author`, removed when the test ends.
async fn conference(app: &TestApp, title: &str, author: Uuid) -> Uuid {
    app.ensure_conference_type().await;
    published(app, "conference", title, author, serde_json::json!({})).await
}

/// A published item of `item_type` titled `title`, by `author`, carrying
/// `fields`, removed when the test ends.
async fn published(
    app: &TestApp,
    item_type: &str,
    title: &str,
    author: Uuid,
    fields: serde_json::Value,
) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: author,
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather tile test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create conference")
        .id;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

/// Register a conference gather over titles containing `marker`, sorted by
/// title, with `extra` merged into its definition. Removed when the test ends.
async fn gather(app: &TestApp, marker: &str, extra: serde_json::Value) -> String {
    let query_id = format!("b74_tile_{marker}");
    let mut definition = serde_json::json!({
        "base_table": "item",
        "item_type": "conference",
        "filters": [{
            "exposed": false,
            "field": "title",
            "operator": "contains",
            "value": marker,
        }],
        "sorts": [{ "field": "title", "direction": "asc", "nulls": null }],
        "stage_aware": true,
    });
    if let (Some(base), Some(more)) = (definition.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            match (key.as_str(), base.get_mut(key)) {
                ("filters", Some(serde_json::Value::Array(filters))) => {
                    filters.extend(value.as_array().cloned().unwrap_or_default());
                }
                _ => {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let query: trovato_kernel::gather::GatherQuery = serde_json::from_value(serde_json::json!({
        "query_id": query_id,
        "label": format!("B74 gather {marker}"),
        "definition": definition,
        "display": {
            "format": "table",
            "items_per_page": 20,
            "canonical_url": format!("/b74-gather-{marker}"),
        },
        "plugin": "core"
    }))
    .expect("gather deserializes");
    app.state
        .gather()
        .register_query(query)
        .await
        .expect("register gather");
    let state = app.state.clone();
    let cleanup_id = query_id.clone();
    defer_cleanup(move || async move {
        let _ = state.gather().delete_query(&cleanup_id).await;
    });
    query_id
}

/// A page that draws the sidebar: an empty gather's own page. The account
/// pages replace the content block the sidebar is drawn in, and have none.
async fn sidebar_page(app: &TestApp) -> String {
    let marker = format!("nosuchtitle{}", stamp());
    let query_id = gather(app, &marker, serde_json::json!({})).await;
    format!("/gather/{query_id}")
}

/// Place a `gather_query` tile in the sidebar and return its id. Removed when
/// the test ends.
async fn tile(app: &TestApp, machine_name: &str, config: serde_json::Value) -> Uuid {
    tile_with_visibility(app, machine_name, config, serde_json::json!({})).await
}

/// Like [`tile`], with a visibility rule other than "always visible".
async fn tile_with_visibility(
    app: &TestApp,
    machine_name: &str,
    config: serde_json::Value,
    visibility: serde_json::Value,
) -> Uuid {
    let tile = Tile::create(
        &app.db,
        CreateTile {
            machine_name: machine_name.to_string(),
            label: format!("Tile {machine_name}"),
            region: Some("sidebar".to_string()),
            tile_type: Some("gather_query".to_string()),
            config: Some(config),
            visibility: Some(visibility),
            weight: Some(0),
            status: Some(1),
            plugin: None,
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create tile");
    let pool = app.db.clone();
    let id = tile.id;
    defer_cleanup(move || async move {
        let _ = Tile::delete(&pool, id).await;
    });
    id
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

/// The markup of the tile `machine_name`: up to the next tile, or the end of
/// the sidebar.
fn tile_html<'a>(html: &'a str, machine_name: &str) -> Option<&'a str> {
    let start = html.find(&format!("id=\"tile-{machine_name}\""))?;
    let rest = &html[start..];
    let next_tile = rest[1..].find("id=\"tile-").map(|i| i + 1);
    let end = next_tile
        .or_else(|| rest.find("</aside>"))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The titles of the links in a tile, in page order.
fn linked_titles(tile: &str, marker: &str) -> Vec<String> {
    tile.split("<a href=\"")
        .skip(1)
        .filter_map(|a| {
            let text = &a[a.find('>')? + 1..];
            let title = &text[..text.find("</a>")?];
            title.contains(marker).then(|| title.trim().to_string())
        })
        .collect()
}

#[test]
fn a_gather_tile_lists_the_first_rows_in_the_gathers_order() {
    run_test(async {
        let app = shared_app().await;
        let page = sidebar_page(app).await;
        let marker = stamp();
        // Created out of order; the gather sorts by title.
        for n in [4, 1, 6, 3, 7, 2, 5] {
            conference(app, &format!("B74 conf {marker} {n}"), Uuid::nil()).await;
        }
        let query_id = gather(app, &marker, serde_json::json!({})).await;
        let three = format!("b74_three_{marker}");
        tile(
            app,
            &three,
            serde_json::json!({ "query_id": query_id, "items": 3 }),
        )
        .await;
        let default = format!("b74_default_{marker}");
        tile(app, &default, serde_json::json!({ "query_id": query_id })).await;

        let html = get(app, &page, "").await;

        let three_html = tile_html(&html, &three).unwrap_or_else(|| panic!("no tile:\n{html}"));
        assert_eq!(
            linked_titles(three_html, &marker),
            [1, 2, 3].map(|n| format!("B74 conf {marker} {n}")),
            "the first three rows, in the gather's order:\n{three_html}"
        );
        let default_html = tile_html(&html, &default).unwrap_or_else(|| panic!("no tile:\n{html}"));
        assert_eq!(
            linked_titles(default_html, &marker),
            [1, 2, 3, 4, 5].map(|n| format!("B74 conf {marker} {n}")),
            "the default is five rows:\n{default_html}"
        );
        // Each row is a link to the item.
        assert!(three_html.contains("<a href=\"/"), "{three_html}");
        assert!(
            three_html.contains(&format!("href=\"/b74-gather-{marker}\"")),
            "a link to the gather's own page:\n{three_html}"
        );
    });
}

#[test]
fn a_gather_the_viewer_may_not_run_renders_no_tile() {
    run_test(async {
        let app = shared_app().await;
        let page = sidebar_page(app).await;
        let marker = stamp();
        conference(app, &format!("B74 secret {marker}"), Uuid::nil()).await;
        let query_id = gather(
            app,
            &marker,
            serde_json::json!({ "access": { "permission": "b74 run secret gathers" } }),
        )
        .await;
        let machine = format!("b74_secret_{marker}");
        tile(app, &machine, serde_json::json!({ "query_id": query_id })).await;

        let html = get(app, &page, "").await;
        assert!(
            tile_html(&html, &machine).is_none(),
            "no tile at all:\n{html}"
        );
        assert!(!html.contains(&format!("B74 secret {marker}")), "{html}");
        assert!(
            !html.contains(&format!("data-query-id=\"{query_id}\"")),
            "{html}"
        );

        let (_, holder) = common::user_holding(app, "b74gt", &["b74 run secret gathers"]).await;
        let html = get(app, &page, &holder).await;
        let tile_markup = tile_html(&html, &machine).unwrap_or_else(|| panic!("{html}"));
        assert!(
            tile_markup.contains(&format!("B74 secret {marker}")),
            "{tile_markup}"
        );
    });
}

#[test]
fn a_gather_on_the_current_user_shows_each_viewer_their_own_rows() {
    run_test(async {
        let app = shared_app().await;
        let page = sidebar_page(app).await;
        let marker = stamp();
        let (alice, alice_cookies) = common::user_holding(app, "b74alice", &[]).await;
        let (bob, bob_cookies) = common::user_holding(app, "b74bob", &[]).await;
        conference(app, &format!("B74 mine {marker} alice"), alice).await;
        conference(app, &format!("B74 mine {marker} bob"), bob).await;
        let query_id = gather(
            app,
            &marker,
            serde_json::json!({ "filters": [{
                "exposed": false,
                "field": "author_id",
                "operator": "equals",
                "value": "current_user",
            }] }),
        )
        .await;
        let machine = format!("b74_mine_{marker}");
        tile(app, &machine, serde_json::json!({ "query_id": query_id })).await;

        let html = get(app, &page, &alice_cookies).await;
        let alice_tile = tile_html(&html, &machine).unwrap_or_else(|| panic!("{html}"));
        assert_eq!(
            linked_titles(alice_tile, &marker),
            [format!("B74 mine {marker} alice")]
        );

        let html = get(app, &page, &bob_cookies).await;
        let bob_tile = tile_html(&html, &machine).unwrap_or_else(|| panic!("{html}"));
        assert_eq!(
            linked_titles(bob_tile, &marker),
            [format!("B74 mine {marker} bob")]
        );
    });
}

// -----------------------------------------------------------------------------
// B186: a gather tile can name the item its page shows.
// -----------------------------------------------------------------------------

/// One tile, placed once, filters out whichever item it is rendered beside: on
/// conference A's page it lists B (and not A); on B's page it lists A (and not
/// B). On a page with no current item — the gather's own listing — it lists
/// no tile at all, never an unfiltered "every conference".
#[test]
fn a_gather_tile_on_current_item_excludes_the_item_it_is_shown_on() {
    run_test(async {
        let app = shared_app().await;
        let marker = stamp();
        let a = conference(app, &format!("B186 conf {marker} A"), Uuid::nil()).await;
        let b = conference(app, &format!("B186 conf {marker} B"), Uuid::nil()).await;
        let query_id = gather(
            app,
            &marker,
            serde_json::json!({ "filters": [{
                "exposed": false,
                "field": "id",
                "operator": "not_equals",
                "value": "current_item",
            }] }),
        )
        .await;
        let machine = format!("b186_current_{marker}");
        tile(app, &machine, serde_json::json!({ "query_id": query_id })).await;

        let html_a = get(app, &format!("/item/{a}"), "").await;
        let tile_a = tile_html(&html_a, &machine).unwrap_or_else(|| panic!("{html_a}"));
        assert_eq!(
            linked_titles(tile_a, &marker),
            [format!("B186 conf {marker} B")],
            "{tile_a}"
        );
        // No "More": the gather's own page shows no item and lists nothing.
        assert!(!tile_a.contains("tile-gather__more"), "{tile_a}");

        let html_b = get(app, &format!("/item/{b}"), "").await;
        let tile_b = tile_html(&html_b, &machine).unwrap_or_else(|| panic!("{html_b}"));
        assert_eq!(
            linked_titles(tile_b, &marker),
            [format!("B186 conf {marker} A")],
            "{tile_b}"
        );

        // The gather's own listing page shows no item: the filter must make
        // the tile render nothing, not every conference.
        let listing = format!("/gather/{query_id}");
        let html_listing = get(app, &listing, "").await;
        assert!(
            tile_html(&html_listing, &machine).is_none(),
            "no tile, let alone every conference:\n{html_listing}"
        );
    });
}

/// A tile with `{"item_types": ["conference"]}` shows on a conference's page
/// and nowhere else: not the gather's own listing, which shows no item.
#[test]
fn a_tile_with_item_types_visibility_shows_only_on_matching_item_pages() {
    run_test(async {
        let app = shared_app().await;
        let marker = stamp();
        let a = conference(app, &format!("B186 types {marker} A"), Uuid::nil()).await;
        let query_id = gather(app, &marker, serde_json::json!({})).await;
        let machine = format!("b186_types_{marker}");
        tile_with_visibility(
            app,
            &machine,
            serde_json::json!({ "query_id": query_id }),
            serde_json::json!({ "item_types": ["conference"] }),
        )
        .await;

        let html_item = get(app, &format!("/item/{a}"), "").await;
        assert!(
            tile_html(&html_item, &machine).is_some(),
            "shows on a conference page:\n{html_item}"
        );

        // Another type's page: not shown.
        let page = published(
            app,
            "page",
            &format!("B186 types {marker} page"),
            Uuid::nil(),
            serde_json::json!({}),
        )
        .await;
        let html_page = get(app, &format!("/item/{page}"), "").await;
        assert!(
            html_page.contains(&format!("B186 types {marker} page")),
            "the page item renders:\n{html_page}"
        );
        assert!(
            tile_html(&html_page, &machine).is_none(),
            "a page item never matches an item_types conference rule:\n{html_page}"
        );

        let listing = format!("/gather/{query_id}");
        let html_listing = get(app, &listing, "").await;
        assert!(
            tile_html(&html_listing, &machine).is_none(),
            "a page with no item never matches an item_types rule:\n{html_listing}"
        );
    });
}

/// "Related by tag": one tile whose filters are `fields.tags has_any_tag
/// "current_item.fields.tags"` and `id not_equals "current_item"` lists, on
/// each item's page, the other items sharing one of its tags. An item with no
/// tags lists nothing, never every item.
#[test]
fn a_gather_tile_on_a_current_item_field_lists_items_sharing_a_tag() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let marker = stamp();
        let shared = Uuid::now_v7().to_string();
        let only_c = Uuid::now_v7().to_string();
        let title = |label: &str| format!("B186 tags {marker} {label}");
        let a = published(
            app,
            "conference",
            &title("A"),
            Uuid::nil(),
            serde_json::json!({ "tags": [shared] }),
        )
        .await;
        let b = published(
            app,
            "conference",
            &title("B"),
            Uuid::nil(),
            serde_json::json!({ "tags": [shared, Uuid::now_v7().to_string()] }),
        )
        .await;
        let c = published(
            app,
            "conference",
            &title("C"),
            Uuid::nil(),
            serde_json::json!({ "tags": [only_c] }),
        )
        .await;
        let untagged = published(
            app,
            "conference",
            &title("D"),
            Uuid::nil(),
            serde_json::json!({}),
        )
        .await;
        let query_id = gather(
            app,
            &marker,
            serde_json::json!({ "filters": [
                {
                    "exposed": false,
                    "field": "fields.tags",
                    "operator": "has_any_tag",
                    "value": "current_item.fields.tags",
                },
                {
                    "exposed": false,
                    "field": "id",
                    "operator": "not_equals",
                    "value": "current_item",
                },
            ] }),
        )
        .await;
        let machine = format!("b186_tags_{marker}");
        tile_with_visibility(
            app,
            &machine,
            serde_json::json!({ "query_id": query_id }),
            serde_json::json!({ "item_types": ["conference"] }),
        )
        .await;

        let expect = [(a, vec![title("B")]), (b, vec![title("A")]), (c, vec![])];
        for (id, expected) in expect {
            let html = get(app, &format!("/item/{id}"), "").await;
            let tile = tile_html(&html, &machine).unwrap_or_else(|| panic!("{html}"));
            assert_eq!(linked_titles(tile, &marker), expected, "on {id}: {tile}");
        }

        // No tags at all: nothing, not every conference but itself.
        let html = get(app, &format!("/item/{untagged}"), "").await;
        let tile = tile_html(&html, &machine).unwrap_or_else(|| panic!("{html}"));
        assert!(linked_titles(tile, &marker).is_empty(), "{tile}");
    });
}

// -----------------------------------------------------------------------------
// Query counts and warnings, read off the tracing events one request emits.
// -----------------------------------------------------------------------------

/// Every event one request emits: `(target, level, text)`.
struct Events(std::sync::Arc<std::sync::Mutex<Vec<(String, tracing::Level, String)>>>);

struct EventText(String);

impl tracing::field::Visit for EventText {
    fn record_debug(&mut self, _field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{value:?} "));
    }

    fn record_str(&mut self, _field: &tracing::field::Field, value: &str) {
        self.0.push_str(value);
        self.0.push(' ');
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Events {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut text = EventText(String::new());
        event.record(&mut text);
        self.0.lock().unwrap().push((
            event.metadata().target().to_string(),
            *event.metadata().level(),
            text.0,
        ));
    }
}

async fn events_for(
    app: &TestApp,
    path: &str,
    cookies: &str,
) -> Vec<(String, tracing::Level, String)> {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    common::scoped_subscribers_see_every_callsite();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(Events(seen.clone()));
    get(app, path, cookies).with_subscriber(subscriber).await;
    seen.lock().unwrap().clone()
}

fn statement_count(events: &[(String, tracing::Level, String)]) -> usize {
    events
        .iter()
        .filter(|(target, _, _)| target == "sqlx::query")
        .count()
}

#[test]
fn a_gather_tile_costs_the_same_queries_for_one_row_as_for_six() {
    run_test(async {
        let app = shared_app().await;
        let page = sidebar_page(app).await;

        let one = stamp();
        conference(app, &format!("B74 count {one} 1"), Uuid::nil()).await;
        let one_query = gather(app, &one, serde_json::json!({})).await;
        let one_tile = format!("b74_count_{one}");
        let one_tile_id = tile(
            app,
            &one_tile,
            serde_json::json!({ "query_id": one_query, "items": 10 }),
        )
        .await;

        // Warm every cache, then count with only the one-row tile placed. The
        // writes above bump cache generations; let those clears land first.
        common::settle_cache_generations().await;
        let _ = get(app, &page, "").await;
        let one_count = statement_count(&events_for(app, &page, "").await);
        Tile::delete(&app.db, one_tile_id).await.unwrap();

        let six = stamp();
        for n in 1..=6 {
            conference(app, &format!("B74 count {six} {n}"), Uuid::nil()).await;
        }
        let six_query = gather(app, &six, serde_json::json!({})).await;
        let six_tile = format!("b74_count_{six}");
        tile(
            app,
            &six_tile,
            serde_json::json!({ "query_id": six_query, "items": 10 }),
        )
        .await;

        common::settle_cache_generations().await;
        let _ = get(app, &page, "").await;
        let html = get(app, &page, "").await;
        let six_html = tile_html(&html, &six_tile).unwrap_or_else(|| panic!("{html}"));
        assert_eq!(linked_titles(six_html, &six).len(), 6, "{six_html}");
        let six_count = statement_count(&events_for(app, &page, "").await);

        assert_eq!(
            one_count, six_count,
            "a gather tile's queries do not grow with its rows: {one_count} for one row, \
             {six_count} for six"
        );
    });
}

#[test]
fn an_unknown_gather_renders_no_tile_and_warns_once() {
    run_test(async {
        let app = shared_app().await;
        let page = sidebar_page(app).await;
        let marker = stamp();
        let query_id = format!("b74_no_such_gather_{marker}");
        let machine = format!("b74_unknown_{marker}");
        tile(app, &machine, serde_json::json!({ "query_id": query_id })).await;

        let events = events_for(app, &page, "").await;
        let warnings: Vec<&String> = events
            .iter()
            .filter(|(_, level, text)| *level == tracing::Level::WARN && text.contains(&query_id))
            .map(|(_, _, text)| text)
            .collect();
        assert_eq!(
            warnings.len(),
            1,
            "one warning naming the gather: {warnings:#?}"
        );

        let html = get(app, &page, "").await;
        assert!(
            tile_html(&html, &machine).is_none(),
            "no tile at all:\n{html}"
        );
    });
}

// -----------------------------------------------------------------------------
// The tile form's item count.
// -----------------------------------------------------------------------------

async fn admin_session(app: &TestApp) -> String {
    let name = format!("b74tileadm_{}", stamp());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

fn csrf_from(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .map(|i| i + needle.len())
        .expect("a CSRF token");
    let end = html[start..].find('"').expect("token terminated");
    html[start..start + end].to_string()
}

/// Submit the add-tile form for a `gather_query` tile, `items` as typed.
async fn submit_tile_form(
    app: &TestApp,
    cookies: &str,
    machine_name: &str,
    items: Option<&str>,
) -> axum::response::Response {
    submit_tile_form_full(app, cookies, machine_name, items, None).await
}

/// Like [`submit_tile_form`], also posting `item_types` when given.
async fn submit_tile_form_full(
    app: &TestApp,
    cookies: &str,
    machine_name: &str,
    items: Option<&str>,
    item_types: Option<&str>,
) -> axum::response::Response {
    let form_page = app
        .request_with_cookies(
            Request::get("/admin/structure/tiles/add")
                .header("x-forwarded-for", common::test_ip_for("b74tileform"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(form_page.status(), StatusCode::OK);
    let token = csrf_from(&body_text(form_page).await);
    let mut fields = vec![
        ("_token", token.as_str()),
        ("machine_name", machine_name),
        ("label", "Gather tile form"),
        ("region", "content_bottom"),
        ("tile_type", "gather_query"),
        ("query_id", "b74_form_gather"),
        ("weight", "0"),
    ];
    if let Some(items) = items {
        fields.push(("items", items));
    }
    if let Some(item_types) = item_types {
        fields.push(("item_types", item_types));
    }
    let body = fields
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    app.request_with_cookies(
        Request::post("/admin/structure/tiles/add")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", common::test_ip_for("b74tileform"))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

async fn stored_config(app: &TestApp, machine_name: &str) -> Option<serde_json::Value> {
    sqlx::query_scalar("SELECT config FROM tile WHERE machine_name = $1")
        .bind(machine_name)
        .fetch_optional(&app.db)
        .await
        .expect("read tile")
}

async fn stored_visibility(app: &TestApp, machine_name: &str) -> Option<serde_json::Value> {
    sqlx::query_scalar("SELECT visibility FROM tile WHERE machine_name = $1")
        .bind(machine_name)
        .fetch_optional(&app.db)
        .await
        .expect("read tile")
}

fn forget_tile(app: &TestApp, machine_name: &str) {
    let pool = app.db.clone();
    let machine_name = machine_name.to_string();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM tile WHERE machine_name = $1")
            .bind(machine_name)
            .execute(&pool)
            .await;
    });
}

#[test]
fn the_tile_form_stores_an_item_count_from_one_to_fifty() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        for (typed, stored) in [("7", 7), ("1", 1), ("50", 50)] {
            let machine = format!("b74_form_{typed}_{}", stamp());
            forget_tile(app, &machine);
            let response = submit_tile_form(app, &cookies, &machine, Some(typed)).await;
            assert_eq!(response.status(), StatusCode::SEE_OTHER, "items={typed}");
            let config = stored_config(app, &machine).await.expect("tile saved");
            assert_eq!(config["items"], serde_json::json!(stored), "{config}");
        }

        // Left empty, the tile gets the default of five.
        let machine = format!("b74_form_default_{}", stamp());
        forget_tile(app, &machine);
        let response = submit_tile_form(app, &cookies, &machine, Some("")).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let config = stored_config(app, &machine).await.expect("tile saved");
        assert_eq!(config["items"], serde_json::json!(5), "{config}");

        // The edit form shows the stored count.
        let id: Uuid = sqlx::query_scalar("SELECT id FROM tile WHERE machine_name = $1")
            .bind(&machine)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let edit = app
            .request_with_cookies(
                Request::get(format!("/admin/structure/tiles/{id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = body_text(edit).await;
        let field = &html[html.find("name=\"items\"").expect("an items field")..];
        let field = &field[..field.find('>').unwrap()];
        assert!(field.contains("value=\"5\""), "{field}");
        assert!(
            field.contains("min=\"1\"") && field.contains("max=\"50\""),
            "{field}"
        );
    });
}

#[test]
fn the_tile_form_refuses_an_item_count_outside_one_to_fifty() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        for typed in ["0", "51", "-3", "five", "2.5"] {
            let machine = format!("b74_form_bad_{}", stamp());
            forget_tile(app, &machine);
            let response = submit_tile_form(app, &cookies, &machine, Some(typed)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "items={typed}");
            let html = body_text(response).await;
            assert!(html.contains("1 and 50"), "the refusal says why: {html}");
            assert!(
                stored_config(app, &machine).await.is_none(),
                "items={typed} saved nothing"
            );
        }
    });
}

#[test]
fn the_tile_form_stores_and_redisplays_item_types() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        let machine = format!("b186_form_{}", stamp());
        forget_tile(app, &machine);
        let response =
            submit_tile_form_full(app, &cookies, &machine, None, Some("article, recipe")).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let visibility = stored_visibility(app, &machine).await.expect("tile saved");
        assert_eq!(
            visibility["item_types"],
            serde_json::json!(["article", "recipe"]),
            "{visibility}"
        );

        // Left blank, no `item_types` key at all: visible on every page.
        let blank_machine = format!("b186_form_blank_{}", stamp());
        forget_tile(app, &blank_machine);
        let response = submit_tile_form_full(app, &cookies, &blank_machine, None, Some("")).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let visibility = stored_visibility(app, &blank_machine)
            .await
            .expect("tile saved");
        assert!(visibility.get("item_types").is_none(), "{visibility}");

        // The edit form redisplays the stored types.
        let id: Uuid = sqlx::query_scalar("SELECT id FROM tile WHERE machine_name = $1")
            .bind(&machine)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let edit = app
            .request_with_cookies(
                Request::get(format!("/admin/structure/tiles/{id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = body_text(edit).await;
        let field = &html[html
            .find("name=\"item_types\"")
            .expect("an item_types field")..];
        let field = &field[..field.find('>').unwrap()];
        assert!(field.contains("value=\"article, recipe\""), "{field}");
    });
}

#[test]
fn the_tile_form_refuses_an_invalid_item_type() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        let machine = format!("b186_form_bad_{}", stamp());
        forget_tile(app, &machine);
        let response =
            submit_tile_form_full(app, &cookies, &machine, None, Some("Not Valid!")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let html = body_text(response).await;
        assert!(
            html.contains("machine names"),
            "the refusal says why: {html}"
        );
        assert!(
            stored_config(app, &machine).await.is_none(),
            "an invalid item type saved nothing"
        );
    });
}
