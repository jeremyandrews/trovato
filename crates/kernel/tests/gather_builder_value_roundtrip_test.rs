#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B279: the gather builder offers every operator the engine accepts, reads
//! every `FilterValue` shape (fixed, list, and each contextual spelling) back
//! into its controls, and opening a gather then pressing Save without
//! touching anything leaves its stored definition unchanged.
//!
//! These tests drive the real admin routes end to end: register a query
//! holding one filter per operator and value shape, fetch its edit page, read
//! the exact `definition_json` the page serves (HTML-unescaped, the way a
//! browser would), post it straight back as `gather-builder.js` does when
//! nothing in a filter row was touched, and compare the re-stored definition
//! to the original. The JSON shape fed to the server here is exactly what
//! the builder's JS produces for an untouched row (`static/js/gather-builder.js`,
//! `createFilterRow`'s `readFilter`); the browser side of that mapping is
//! covered by `tests/admin-a11y/specs/gather-builder.spec.mjs`, which a Rust
//! test cannot reach.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{TestApp, defer_cleanup, run_test, shared_app, test_ip_for, user_holding};
use serde_json::{Value, json};
use trovato_kernel::gather::GatherQuery;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::tile::{CreateTile, Tile};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn query_id(marker: &str) -> String {
    format!("b279.{marker}")
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get(app: &TestApp, cookies: &str, bucket: &str, path: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    body_of(response).await
}

async fn post(
    app: &TestApp,
    cookies: &str,
    bucket: &str,
    path: &str,
    body: String,
) -> axum::response::Response {
    app.request_with_cookies(
        Request::post(path)
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header("x-forwarded-for", test_ip_for(bucket))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

fn token_in(html: &str) -> Option<String> {
    let at = html.find(r#"name="_token""#)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The `value="..."` of `<input id="{id}" ... value="...">`, HTML-unescaped
/// the way a browser reads an attribute.
fn input_value(html: &str, id: &str) -> Option<String> {
    let marker = format!(r#"id="{id}""#);
    let at = html.find(&marker)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(html_unescape(&tag[vat..vend]))
}

/// Undo Tera's default attribute escaping: `&amp;`, `&lt;`, `&gt;`,
/// `&quot;`, `&#x27;`/`&#39;`.
fn html_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

async fn forget(app: &TestApp, id: &str) {
    app.state.gather().delete_query(id).await.ok();
}

/// Register `definition` (and a default display) directly, as a config
/// import or a plugin migration would, bypassing the builder entirely: the
/// stored baseline the round trip is measured against.
async fn register(app: &TestApp, id: &str, definition: Value) {
    let query: GatherQuery = serde_json::from_value(json!({
        "query_id": id,
        "label": "B279 round trip",
        "description": null,
        "definition": definition,
        "display": {},
        "plugin": "core",
    }))
    .expect("definition deserializes");
    app.state
        .gather()
        .register_query(query)
        .await
        .expect("register gather");
}

fn stored_definition(app: &TestApp, id: &str) -> Value {
    serde_json::to_value(
        app.state
            .gather()
            .get_query(id)
            .unwrap_or_else(|| panic!("{id} is stored"))
            .definition,
    )
    .expect("definition serializes")
}

/// Fetch the edit page's `definition_json`, post it straight back to save
/// unchanged (the way `gather-builder.js` does when no filter row is
/// touched), and return it (parsed) alongside what is stored afterwards.
async fn round_trip_through_the_builder(
    app: &TestApp,
    cookies: &str,
    bucket: &str,
    id: &str,
) -> (Value, Value) {
    let edit = get(app, cookies, bucket, &format!("/admin/gather/{id}/edit")).await;
    let token = token_in(&edit).expect("the edit form has a token");
    let definition_json = input_value(&edit, "definition_json").expect("definition_json input");
    let display_json = input_value(&edit, "display_json").expect("display_json input");
    let served: Value = serde_json::from_str(&definition_json).expect("served definition parses");

    let body = format!(
        "_token={}&_form_build_id=x&query_id={id}&label=B279%20round%20trip&description=&definition_json={}&display_json={}",
        urlencoding::encode(&token),
        urlencoding::encode(&definition_json),
        urlencoding::encode(&display_json),
    );
    let response = post(
        app,
        cookies,
        bucket,
        &format!("/admin/gather/{id}/save"),
        body,
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SEE_OTHER,
        "an untouched save redirects: {}",
        body_of(response).await
    );

    (served, stored_definition(app, id))
}

/// One filter per `FilterOperator` the engine accepts, and per `FilterValue`
/// shape: fixed scalars of every JSON type, lists, every contextual
/// spelling, and a plugin's own `custom` operator (which the builder cannot
/// enumerate, and must keep verbatim). Also one exposed filter carrying a
/// label and a non-default widget, which the builder has no control for
/// either.
fn every_shape_definition() -> Value {
    json!({
        "base_table": "item",
        "stage_aware": true,
        "filters": [
            {"field": "title", "operator": "equals", "value": "hello"},
            {"field": "title", "operator": "not_equals", "value": 42},
            {"field": "title", "operator": "contains", "value": "foo"},
            {"field": "title", "operator": "starts_with", "value": "bar"},
            {"field": "title", "operator": "ends_with", "value": "baz"},
            {"field": "created", "operator": "greater_than", "value": 1000},
            {"field": "fields.score", "operator": "less_than", "value": 3.5},
            {"field": "fields.published_date", "operator": "greater_or_equal", "value": "2024-01-01"},
            {"field": "fields.active", "operator": "less_or_equal", "value": true},
            {"field": "title", "operator": "in", "value": ["a", "b"]},
            {"field": "created", "operator": "not_in", "value": [1, 2, 3]},
            {"field": "fields.summary", "operator": "is_null", "value": null},
            {"field": "fields.summary", "operator": "is_not_null", "value": null},
            {"field": "fields.tags", "operator": "has_tag", "value": "11111111-1111-1111-1111-111111111111"},
            {"field": "fields.tags", "operator": "has_any_tag", "value": ["11111111-1111-1111-1111-111111111111", "22222222-2222-2222-2222-222222222222"]},
            {"field": "fields.tags", "operator": "has_all_tags", "value": ["11111111-1111-1111-1111-111111111111"]},
            {"field": "fields.tags", "operator": "has_tag_or_descendants", "value": "11111111-1111-1111-1111-111111111111"},
            {"field": "title", "operator": "full_text_search", "value": "search terms"},
            {"field": "title", "operator": "semantic_similarity", "value": "find similar items"},
            {"field": "fields.category", "operator": {"custom": "category_tag"}, "value": "whatever a plugin wants"},
            {"field": "author_id", "operator": "equals", "value": "current_user"},
            {"field": "created", "operator": "greater_than", "value": "current_time"},
            {"field": "fields.published_date", "operator": "greater_or_equal", "value": "current_date"},
            {"field": "fields.category", "operator": "equals", "value": {"url_arg": "cat"}},
            {"field": "id", "operator": "not_equals", "value": "current_item"},
            {"field": "fields.tags", "operator": "has_any_tag", "value": {"current_item_field": "tags"}},
            {
                "field": "fields.region",
                "operator": "equals",
                "value": "1",
                "exposed": true,
                "exposed_label": "Region",
                "widget": {"type": "boolean"},
            },
        ],
    })
}

#[test]
fn an_untouched_save_leaves_every_operator_and_value_shape_unchanged() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b279-rt", &["administer site"]).await;
        let id = query_id(&format!(
            "rt_{}",
            &Uuid::now_v7().simple().to_string()[20..]
        ));

        register(app, &id, every_shape_definition()).await;
        let before = stored_definition(app, &id);

        let (served, after) = round_trip_through_the_builder(app, &cookies, "b279-rt", &id).await;
        forget(app, &id).await;

        assert_eq!(
            served, before,
            "the edit page must serve exactly what is stored"
        );
        assert_eq!(
            after, before,
            "a save that touched nothing must store exactly what was there"
        );
    });
}

/// A second pass over the same filters confirms a save is also idempotent:
/// saving an already-round-tripped definition again changes nothing further.
#[test]
fn a_second_untouched_save_is_still_a_no_op() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b279-rt2", &["administer site"]).await;
        let id = query_id(&format!(
            "rt2_{}",
            &Uuid::now_v7().simple().to_string()[20..]
        ));

        register(app, &id, every_shape_definition()).await;
        let before = stored_definition(app, &id);

        round_trip_through_the_builder(app, &cookies, "b279-rt2", &id).await;
        let (_, after) = round_trip_through_the_builder(app, &cookies, "b279-rt2", &id).await;
        forget(app, &id).await;

        assert_eq!(after, before);
    });
}

/// `/admin/gather/{id}/clone` copies contextual filters unchanged: every
/// value shape in [`every_shape_definition`] survives a clone byte for byte.
#[test]
fn cloning_a_gather_copies_contextual_filters_unchanged() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b279-clone", &["administer site"]).await;
        let id = query_id(&format!(
            "clone_{}",
            &Uuid::now_v7().simple().to_string()[20..]
        ));
        register(app, &id, every_shape_definition()).await;
        let before = stored_definition(app, &id);

        let edit = get(
            app,
            &cookies,
            "b279-clone",
            &format!("/admin/gather/{id}/edit"),
        )
        .await;
        let token = token_in(&edit).expect("a token");
        let response = post(
            app,
            &cookies,
            "b279-clone",
            &format!("/admin/gather/{id}/clone"),
            format!("_token={}", urlencoding::encode(&token)),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);

        let cloned_id = app
            .state
            .gather()
            .list_queries()
            .into_iter()
            .map(|q| q.query_id)
            .find(|qid| qid.starts_with(&format!("{id}-copy-")))
            .expect("the clone is registered");

        let cloned = stored_definition(app, &cloned_id);
        forget(app, &id).await;
        forget(app, &cloned_id).await;

        assert_eq!(cloned, before, "a clone keeps every filter unchanged");
    });
}

// -----------------------------------------------------------------------------
// B186 shapes ("other articles", "related recipes") built through form
// fields: the value-source JSON the new builder produces for "This item
// (id)" and "A field of this item", exercised on two different item pages.
// -----------------------------------------------------------------------------

async fn ensure_article_type(app: &TestApp) {
    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ('b279_article', 'B279 Article', 'Fixture type', true, 'Title', 'core', $1) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(&settings)
    .execute(&app.db)
    .await
    .expect("seed item type");
    app.state
        .content_types()
        .replace(
            "b279_article",
            "B279 Article",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

async fn article(app: &TestApp, title: &str, fields: Value) -> Uuid {
    ensure_article_type(app).await;
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "b279_article".to_string(),
                title: title.to_string(),
                author_id: trovato_kernel::models::user::ANONYMOUS_USER_ID,
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b279 builder value shapes test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item");
    let (id, pool) = (item.id, app.db.clone());
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

async fn tile(app: &TestApp, machine_name: &str, query_id: &str) -> Uuid {
    let tile = Tile::create(
        &app.db,
        CreateTile {
            machine_name: machine_name.to_string(),
            label: format!("Tile {machine_name}"),
            region: Some("sidebar".to_string()),
            tile_type: Some("gather_query".to_string()),
            config: Some(json!({ "query_id": query_id })),
            visibility: Some(json!({})),
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

fn tile_html<'a>(html: &'a str, machine_name: &str) -> Option<&'a str> {
    let start = html.find(&format!("id=\"tile-{machine_name}\""))?;
    let rest = &html[start..];
    let next_tile = rest[1..].find("id=\"tile-").map(|i| i + 1);
    let end = next_tile
        .or_else(|| rest.find("</aside>"))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

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

/// Create a gather through the admin create form, with a `title contains
/// marker` filter plus `extra_filters` (the JSON the builder's new
/// value-source controls produce), the way a site builder's browser would
/// post it.
async fn create_via_form(
    app: &TestApp,
    cookies: &str,
    bucket: &str,
    id: &str,
    extra_filters: Value,
) {
    let edit = get(app, cookies, bucket, "/admin/gather/create").await;
    let token = token_in(&edit).expect("a token");
    let mut filters = vec![json!({"field": "type", "operator": "equals", "value": "b279_article"})];
    filters.extend(extra_filters.as_array().cloned().unwrap_or_default());
    let definition = json!({ "base_table": "item", "stage_aware": true, "filters": filters });
    let display = json!({ "format": "list", "items_per_page": 20 });
    let body = format!(
        "_token={}&_form_build_id=x&query_id={id}&label=B279%20shape&description=&definition_json={}&display_json={}",
        urlencoding::encode(&token),
        urlencoding::encode(&definition.to_string()),
        urlencoding::encode(&display.to_string()),
    );
    let response = post(app, cookies, bucket, "/admin/gather/create", body).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SEE_OTHER,
        "{}",
        body_of(response).await
    );
}

/// "Other articles": `id not_equals current_item`, built the way the
/// builder's "This item (id)" value source writes it, lists every other
/// article on each article's own page, never itself.
#[test]
fn other_articles_built_through_form_fields_excludes_only_the_item_shown() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b279-other", &["administer site"]).await;
        let marker = format!("b279other{}", Uuid::now_v7().simple());
        let title = |label: &str| format!("{marker} {label}");

        let a = article(app, &title("A"), json!({})).await;
        let b = article(app, &title("B"), json!({})).await;

        let id = query_id(&format!(
            "other_{}",
            &Uuid::now_v7().simple().to_string()[20..]
        ));
        create_via_form(
            app,
            &cookies,
            "b279-other",
            &id,
            json!([
                {"field": "title", "operator": "contains", "value": marker},
                {"field": "id", "operator": "not_equals", "value": "current_item"},
            ]),
        )
        .await;
        let machine = format!("b279_other_{marker}");
        tile(app, &machine, &id).await;

        let html_a = get(app, &cookies, "b279-other", &format!("/item/{a}")).await;
        let tile_a = tile_html(&html_a, &machine).unwrap_or_else(|| panic!("{html_a}"));
        assert_eq!(linked_titles(tile_a, &marker), [title("B")], "{tile_a}");

        let html_b = get(app, &cookies, "b279-other", &format!("/item/{b}")).await;
        let tile_b = tile_html(&html_b, &machine).unwrap_or_else(|| panic!("{html_b}"));
        assert_eq!(linked_titles(tile_b, &marker), [title("A")], "{tile_b}");

        forget(app, &id).await;
    });
}

/// "Related recipes": `fields.tags has_any_tag current_item.fields.tags`,
/// built the way the builder's "A field of this item" value source writes
/// it (`{"current_item_field": "tags"}`), lists, on each item's page, the
/// other items sharing one of its tags.
#[test]
fn related_recipes_built_through_form_fields_finds_items_sharing_a_tag() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b279-related", &["administer site"]).await;
        let marker = format!("b279rel{}", Uuid::now_v7().simple());
        let shared_tag = Uuid::now_v7().to_string();
        let unique_tag = Uuid::now_v7().to_string();
        let title = |label: &str| format!("{marker} {label}");

        let r1 = article(app, &title("R1"), json!({ "tags": [shared_tag] })).await;
        // Unused but keeps r2 from looking dead to a reader scanning the test.
        let _r2 = article(
            app,
            &title("R2"),
            json!({ "tags": [shared_tag, unique_tag] }),
        )
        .await;

        let id = query_id(&format!(
            "related_{}",
            &Uuid::now_v7().simple().to_string()[20..]
        ));
        create_via_form(
            app,
            &cookies,
            "b279-related",
            &id,
            json!([
                {"field": "title", "operator": "contains", "value": marker},
                {"field": "id", "operator": "not_equals", "value": "current_item"},
                {"field": "fields.tags", "operator": "has_any_tag", "value": {"current_item_field": "tags"}},
            ]),
        )
        .await;
        let machine = format!("b279_related_{marker}");
        tile(app, &machine, &id).await;

        let html_r1 = get(app, &cookies, "b279-related", &format!("/item/{r1}")).await;
        let tile_r1 = tile_html(&html_r1, &machine).unwrap_or_else(|| panic!("{html_r1}"));
        assert_eq!(linked_titles(tile_r1, &marker), [title("R2")], "{tile_r1}");

        forget(app, &id).await;
    });
}
