#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The `CategoryReference` field kind: a field that holds tags of one category.
//!
//! Before it, a site had no way to say "this field holds topics": Ritrovo stored
//! tag uuids in a field declared as something else, nothing checked that a
//! uuid named a tag of the right category, the item page could not turn one
//! into a label or a link, and a field's `cardinality` was enforced by the
//! admin form alone (`G-NO-CATEGORY-REFERENCE-FIELD-KIND`,
//! `G-ITEM-ROUTE-DOES-NOT-RESOLVE-CATEGORY-TAGS`, `G-CARDINALITY-IS-INERT`).
//!
//! These cover the field kind end to end: the admin field screen, the tag
//! picker and its autocomplete endpoint, the one check every write path runs
//! (item service, JSON API, config import), `referenced_tags` on the item page
//! with a fixed number of queries, and gather's `has_tag_or_descendants` over
//! the stored value.
//!
//! The field types are built from JSON, the shape a type's `settings` stores,
//! so this file compiles against a kernel that does not know the kind yet.
//!
//! Requires Postgres + Redis.

mod common;

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, test_ip_for, user_holding};
use trovato_kernel::gather::{
    FilterOperator, FilterValue, QueryContext, QueryDefinition, QueryDisplay, QueryFilter,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateCategory, CreateItem, CreateTag, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PROBE: &str = "b72_tag_probe";
const TOPICS: &str = "b72_topics";
const VENUES: &str = "b72_venues";
const ROUTE_QUERY: &str = "b72.tag_probe_by_topic";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();
static TAGS: tokio::sync::OnceCell<Tags> = tokio::sync::OnceCell::const_new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("tag probe app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    let project_root = common::project_root();
    TestApp::with_config(|config| {
        config.templates_dirs = vec![
            project_root.join("templates"),
            project_root.join("crates/kernel/tests/fixtures/templates"),
        ];
    })
    .await
}

/// The tags every test uses. `Topics`, three levels: Languages > Systems >
/// Rust and Go, Languages > Python, and Web on its own; `Venues`: Hall and
/// Garden, a different category.
#[derive(Debug, Clone, Copy)]
struct Tags {
    languages: Uuid,
    systems: Uuid,
    rust: Uuid,
    go: Uuid,
    python: Uuid,
    web: Uuid,
    hall: Uuid,
    garden: Uuid,
}

async fn ensure_category(app: &TestApp, id: &str, label: &str) {
    let categories = app.state.categories();
    if categories.get_category(id).await.unwrap().is_none() {
        categories
            .create_category(CreateCategory {
                id: id.to_string(),
                label: label.to_string(),
                description: None,
                hierarchy: Some(1),
                weight: None,
            })
            .await
            .expect("create category");
    }
}

async fn ensure_tag(
    app: &TestApp,
    category: &str,
    label: &str,
    slug: &str,
    parent: Option<Uuid>,
) -> Uuid {
    if let Some(tag) = trovato_kernel::models::Tag::find_by_slug(&app.db, category, slug)
        .await
        .unwrap()
    {
        return tag.id;
    }
    app.state
        .categories()
        .create_tag(CreateTag {
            category_id: category.to_string(),
            label: label.to_string(),
            description: None,
            slug: Some(slug.to_string()),
            weight: None,
            parent_ids: parent.map(|p| vec![p]),
        })
        .await
        .expect("create tag")
        .id
}

/// Seed the categories, tags, probe type and the gather route that gives a
/// topic a URL. Once per binary: the tests in it run in parallel.
async fn tags(app: &'static TestApp) -> Tags {
    *TAGS
        .get_or_init(|| async move {
            ensure_category(app, TOPICS, "B72 Topics").await;
            ensure_category(app, VENUES, "B72 Venues").await;
            let languages = ensure_tag(app, TOPICS, "Languages", "b72-languages", None).await;
            let systems =
                ensure_tag(app, TOPICS, "Systems", "b72-systems", Some(languages)).await;
            let rust = ensure_tag(app, TOPICS, "Rust", "b72-rust", Some(systems)).await;
            let go = ensure_tag(app, TOPICS, "Go", "b72-go", Some(systems)).await;
            let python = ensure_tag(app, TOPICS, "Python", "b72-python", Some(languages)).await;
            let web = ensure_tag(app, TOPICS, "Web", "b72-web", None).await;
            let hall = ensure_tag(app, VENUES, "Hall", "b72-hall", None).await;
            let garden = ensure_tag(app, VENUES, "Garden", "b72-garden", None).await;

            let settings = serde_json::json!({ "fields": [
                { "field_name": "field_topics", "field_type": { "CategoryReference": TOPICS },
                  "label": "Topics", "cardinality": 3 },
                { "field_name": "field_venue", "field_type": { "CategoryReference": VENUES },
                  "label": "Venue" },
                { "field_name": "field_tags", "field_type": { "CategoryReference": TOPICS },
                  "label": "Tags", "cardinality": -1 },
                { "field_name": "field_note", "field_type": { "Text": { "max_length": null } },
                  "label": "Note" }
            ]});
            app.state
                .content_types()
                .replace(PROBE, "Tag Probe", None, settings)
                .await
                .expect("seed probe type");

            let query: trovato_kernel::gather::GatherQuery =
                serde_json::from_value(serde_json::json!({
                    "query_id": ROUTE_QUERY,
                    "label": "Tag probe by topic",
                    "description": null,
                    "definition": {
                        "base_table": "item",
                        "item_type": PROBE,
                        "filters": [{
                            "field": "fields.field_topics",
                            "operator": "has_tag_or_descendants",
                            "value": { "url_arg": "topic" },
                            "exposed": false,
                            "exposed_label": null
                        }],
                        "stage_aware": true
                    },
                    "display": {
                        "routes": [{
                            "path": "/b72-topics/{slug}",
                            "params": [{ "segment": "slug", "param": "topic", "tag_category": TOPICS }]
                        }]
                    },
                    "plugin": "core"
                }))
                .expect("gather query json");
            app.state
                .gather()
                .register_query(query)
                .await
                .expect("register route query");

            Tags {
                languages,
                systems,
                rust,
                go,
                python,
                web,
                hall,
                garden,
            }
        })
        .await
}

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn create_input(title: &str, fields: serde_json::Value) -> CreateItem {
    CreateItem {
        item_type: PROBE.to_string(),
        title: title.to_string(),
        author_id: Uuid::nil(),
        status: Some(1),
        promote: Some(0),
        sticky: Some(0),
        fields: Some(fields),
        stage_id: Some(LIVE_STAGE_ID),
        language: Some("en".to_string()),
        log: None,
    }
}

async fn create(app: &TestApp, title: &str, fields: serde_json::Value) -> anyhow::Result<Uuid> {
    app.state
        .items()
        .create(create_input(title, fields), &admin())
        .await
        .map(|item| item.id)
}

async fn count_titled(app: &TestApp, title: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE type = $1 AND title = $2")
        .bind(PROBE)
        .bind(title)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get_as(app: &TestApp, path: &str, cookies: &str, bucket: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

/// Pull a named input's value out of rendered form HTML.
fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The kind is stored as `{"CategoryReference": "<category id>"}` and comes back
/// out of the registry as that kind, not as something else or nothing.
#[test]
fn the_field_kind_round_trips_through_the_registry() {
    run_test(async {
        let app = app();
        tags(app).await;
        let ct = app.state.content_types().get(PROBE).expect("probe type");
        let topics = ct
            .fields
            .iter()
            .find(|f| f.field_name == "field_topics")
            .expect("field_topics is registered");
        assert_eq!(
            serde_json::to_value(&topics.field_type).unwrap(),
            serde_json::json!({ "CategoryReference": TOPICS }),
        );
        assert_eq!(topics.cardinality, 3);
    });
}

/// `GET /api/v1/tags/autocomplete?category=<id>&q=<text>` answers with the
/// tags of that category whose label starts with the text, in the shape the
/// item autocomplete uses.
#[test]
fn tags_autocomplete_answers_with_tags_of_the_category() {
    run_test(async {
        let app = app();
        let t = tags(app).await;

        let (status, body) = get_as(
            app,
            &format!("/api/v1/tags/autocomplete?category={TOPICS}&q=ru"),
            "",
            "b72_tags_ac",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let results: Vec<serde_json::Value> = serde_json::from_str(&body).expect("json array");
        assert_eq!(
            results,
            vec![serde_json::json!({ "id": t.rust, "title": "Rust" })],
            "a case-insensitive label prefix, this category only"
        );

        let (_, body) = get_as(
            app,
            &format!("/api/v1/tags/autocomplete?category={TOPICS}&q=Ha"),
            "",
            "b72_tags_ac",
        )
        .await;
        assert_eq!(body, "[]", "a tag of another category is not offered");

        for path in [
            format!("/api/v1/tags/autocomplete?category={TOPICS}"),
            "/api/v1/tags/autocomplete?q=ru".to_string(),
            "/api/v1/tags/autocomplete?category=b72_no_such_category&q=ru".to_string(),
        ] {
            let (status, body) = get_as(app, &path, "", "b72_tags_ac").await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body, "[]", "{path}");
        }
    });
}

/// The item service refuses more values than a field's cardinality allows and
/// a value that is not a tag of the field's category, on create and on update,
/// and stores nothing. `cardinality: 3` refuses four, `1` refuses two, `-1`
/// takes any number.
#[test]
fn the_item_service_enforces_cardinality_and_category() {
    run_test(async {
        let app = app();
        let t = tags(app).await;

        let four = create(
            app,
            "B72 four topics",
            serde_json::json!({ "field_topics": [t.rust, t.python, t.go, t.web] }),
        )
        .await
        .expect_err("four topics in a field that allows three");
        assert!(
            format!("{four:#}").contains("Topics allows at most 3 values"),
            "{four:#}"
        );
        assert_eq!(count_titled(app, "B72 four topics").await, 0);

        let two_venues = create(
            app,
            "B72 two venues",
            serde_json::json!({ "field_venue": [t.hall, t.garden] }),
        )
        .await
        .expect_err("two venues in a single value field");
        assert!(
            format!("{two_venues:#}").contains("Venue allows at most 1 value"),
            "{two_venues:#}"
        );

        let two_notes = create(
            app,
            "B72 two notes",
            serde_json::json!({ "field_note": ["one", "two"] }),
        )
        .await
        .expect_err("cardinality is checked for every field kind, not only tags");
        assert!(
            format!("{two_notes:#}").contains("Note allows at most 1 value"),
            "{two_notes:#}"
        );

        let wrong = create(
            app,
            "B72 wrong category",
            serde_json::json!({ "field_topics": [t.hall] }),
        )
        .await
        .expect_err("a venue is not a topic");
        assert!(
            format!("{wrong:#}").contains(&t.hall.to_string()),
            "the refusal names the value: {wrong:#}"
        );
        assert_eq!(count_titled(app, "B72 wrong category").await, 0);

        let not_a_tag = Uuid::now_v7();
        let missing = create(
            app,
            "B72 not a tag",
            serde_json::json!({ "field_venue": [not_a_tag] }),
        )
        .await
        .expect_err("a uuid naming no tag");
        assert!(format!("{missing:#}").contains(&not_a_tag.to_string()));

        // The stored shape is an array whatever the cardinality, the shape
        // gather's tag operators read; a bare string is not that shape.
        let bare = create(
            app,
            "B72 bare venue",
            serde_json::json!({ "field_venue": t.hall.to_string() }),
        )
        .await
        .expect_err("a bare string where a list of tags belongs");
        assert!(
            format!("{bare:#}").contains("Venue holds a list of tag ids"),
            "{bare:#}"
        );

        // Within the limits it is stored as written, an array of tag uuids,
        // and `-1` takes all six topics.
        let all: Vec<String> = [t.languages, t.systems, t.rust, t.go, t.python, t.web]
            .iter()
            .map(Uuid::to_string)
            .collect();
        let id = create(
            app,
            "B72 within limits",
            serde_json::json!({
                "field_topics": [t.rust.to_string(), t.python.to_string(), t.web.to_string()],
                "field_venue": [t.hall.to_string()],
                "field_tags": all,
                "field_note": "one",
            }),
        )
        .await
        .expect("three topics, one venue, six tags");
        let item = app.state.items().load(id).await.unwrap().unwrap();
        assert_eq!(
            item.fields["field_topics"],
            serde_json::json!([t.rust.to_string(), t.python.to_string(), t.web.to_string()])
        );
        assert_eq!(item.fields["field_tags"], serde_json::json!(all));

        let refused = app
            .state
            .items()
            .update(
                id,
                UpdateItem {
                    title: None,
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({
                        "field_topics": [t.rust, t.python, t.web, t.go],
                    })),
                    log: None,
                    stage_id: None,
                    author_id: None,
                    created: None,
                },
                &admin(),
            )
            .await
            .expect_err("an update past the limit");
        assert!(format!("{refused:#}").contains("Topics allows at most 3 values"));
        let stored = app.state.items().load(id).await.unwrap().unwrap();
        assert_eq!(
            stored.fields["field_topics"],
            serde_json::json!([t.rust.to_string(), t.python.to_string(), t.web.to_string()]),
            "the refused update wrote nothing"
        );
    });
}

/// The JSON item API: a value the check refuses is a 422 with the reason, not
/// a 500 or a silent store, and an accepted array reads back as written.
#[test]
fn the_json_api_saves_tags_and_answers_422_for_a_refused_value() {
    run_test(async {
        let app = app();
        let t = tags(app).await;
        let (_, cookies) = user_holding(
            app,
            "b72-tagapi",
            &[
                "access content",
                "create b72_tag_probe content",
                "publish content",
            ],
        )
        .await;

        let post = |fields: serde_json::Value, title: String| {
            let cookies = cookies.clone();
            async move {
                let (status, html) =
                    get_as(app, "/item/add/b72_tag_probe", &cookies, "b72-tagapi").await;
                assert_eq!(status, StatusCode::OK, "{html}");
                let csrf = input_value(&html, "_token").expect("csrf token");
                let response = app
                    .request_with_cookies(
                        Request::post("/item/add/b72_tag_probe")
                            .header(header::CONTENT_TYPE, "application/json")
                            .header("X-CSRF-Token", &csrf)
                            .header("x-forwarded-for", test_ip_for("b72-tagapi"))
                            .body(Body::from(
                                serde_json::json!({ "title": title, "status": 1, "fields": fields })
                                    .to_string(),
                            ))
                            .unwrap(),
                        &cookies,
                    )
                    .await;
                let status = response.status();
                (status, body_string(response).await)
            }
        };

        let (status, body) = post(
            serde_json::json!({ "field_topics": [t.rust, t.python, t.go, t.web] }),
            "B72 api four topics".to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("Topics allows at most 3 values"), "{body}");
        assert_eq!(count_titled(app, "B72 api four topics").await, 0);

        let (status, body) = post(
            serde_json::json!({ "field_venue": [t.hall, t.garden] }),
            "B72 api two venues".to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("Venue allows at most 1 value"), "{body}");

        let (status, body) = post(
            serde_json::json!({ "field_topics": [t.garden] }),
            "B72 api wrong category".to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains(&t.garden.to_string()), "{body}");
        assert_eq!(count_titled(app, "B72 api wrong category").await, 0);

        let saved = format!("B72 api saved {}", Uuid::now_v7().simple());
        let (status, body) = post(
            serde_json::json!({ "field_topics": [t.rust.to_string(), t.go.to_string()] }),
            saved.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let id: Uuid = sqlx::query_scalar("SELECT id FROM item WHERE title = $1")
            .bind(&saved)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let (status, body) = get_as(app, &format!("/api/item/{id}"), &cookies, "b72-tagapi").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["fields"]["field_topics"],
            serde_json::json!([t.rust.to_string(), t.go.to_string()]),
            "the API reads back the array it was given"
        );
    });
}

/// The admin content form: four topics are refused with the reason on the
/// form, a venue in the topics field is refused, three topics save.
#[test]
fn the_admin_form_saves_tags_and_refuses_what_the_check_refuses() {
    run_test(async {
        let app = app();
        let t = tags(app).await;
        let cookies = app
            .create_and_login_admin("b72tagform", "b72-tag-form-pass", "b72tagform@test.local")
            .await;

        let submit = |title: String, topics: Vec<Uuid>| {
            let cookies = cookies.clone();
            async move {
                let (status, html) = get_as(
                    app,
                    &format!("/admin/content/add/{PROBE}"),
                    &cookies,
                    "b72tagform",
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{html}");
                let mut body = format!(
                    "_token={}&_form_build_id={}&title={}",
                    urlencoding::encode(&input_value(&html, "_token").unwrap()),
                    urlencoding::encode(&input_value(&html, "_form_build_id").unwrap()),
                    urlencoding::encode(&title),
                );
                for (i, tag) in topics.iter().enumerate() {
                    body.push_str(&format!("&field_topics%5B{i}%5D={tag}"));
                }
                let response = app
                    .request_with_cookies(
                        Request::post(format!("/admin/content/add/{PROBE}"))
                            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                            .header("x-forwarded-for", test_ip_for("b72tagform"))
                            .body(Body::from(body))
                            .unwrap(),
                        &cookies,
                    )
                    .await;
                let status = response.status();
                (status, body_string(response).await)
            }
        };

        let (status, html) = submit(
            "B72 form four".to_string(),
            vec![t.rust, t.go, t.python, t.web],
        )
        .await;
        assert!(!status.is_redirection(), "four topics must not save");
        assert!(html.contains("Topics allows at most 3 values"), "{html}");
        assert_eq!(count_titled(app, "B72 form four").await, 0);

        let (status, html) = submit("B72 form wrong".to_string(), vec![t.hall]).await;
        assert!(!status.is_redirection(), "a venue must not save as a topic");
        assert!(html.contains(&t.hall.to_string()), "{html}");
        assert_eq!(count_titled(app, "B72 form wrong").await, 0);

        let three = format!("B72 form three {}", Uuid::now_v7().simple());
        let (status, html) = submit(three.clone(), vec![t.rust, t.go, t.python]).await;
        assert!(status.is_redirection(), "{status}: {html}");
        let fields: serde_json::Value =
            sqlx::query_scalar("SELECT fields FROM item WHERE title = $1")
                .bind(&three)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(
            fields["field_topics"],
            serde_json::json!([t.rust.to_string(), t.go.to_string(), t.python.to_string()])
        );
    });
}

/// The item page resolves the field into `referenced_tags`: each tag's label,
/// slug, the URL of the gather route that lists its category, and its ancestors
/// root first.
#[test]
fn referenced_tags_resolve_label_slug_url_and_ancestors() {
    run_test(async {
        let app = app();
        let t = tags(app).await;
        let id = create(
            app,
            "B72 resolved tags",
            serde_json::json!({
                "field_topics": [t.rust.to_string(), t.python.to_string()],
                "field_venue": [t.hall.to_string()],
            }),
        )
        .await
        .expect("create");

        let (status, html) = get_as(app, &format!("/item/{id}"), "", "b72_tag_page").await;
        assert_eq!(status, StatusCode::OK, "{html}");
        // Tera's autoescape writes `/` as `&#x2F;` inside attributes.
        let html = html.replace("&#x2F;", "/");

        assert!(
            html.contains(&format!(
                r#"data-field="field_topics" data-id="{}" data-slug="b72-rust" href="/b72-topics/b72-rust">Rust</a>"#,
                t.rust
            )),
            "Rust resolves with its slug and route URL:\n{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<span class="trail" data-for="{}">Languages[/b72-topics/b72-languages] &gt; Systems[/b72-topics/b72-systems] &gt; Rust</span>"#,
                t.rust
            )),
            "Rust's ancestors, root first, each with its URL:\n{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<span class="trail" data-for="{}">Languages[/b72-topics/b72-languages] &gt; Python</span>"#,
                t.python
            )),
            "Python's single ancestor:\n{html}"
        );
        assert!(
            html.contains(&format!(
                r#"data-field="field_venue" data-id="{}" data-slug="b72-hall" href="">Hall</a>"#,
                t.hall
            )),
            "a category no gather route lists resolves without a URL:\n{html}"
        );
    });
}

/// How many statements one GET runs, after a warm-up GET has filled the caches:
/// the count the request profiler leaves in the response
/// (`RequestProfileSnapshot`), which is every statement the request ran.
async fn queries_for(app: &TestApp, path: &str) -> u32 {
    let (status, html) = get_as(app, path, "", "b72_tag_count").await;
    assert_eq!(status, StatusCode::OK, "{path}: {html}");
    let response = app
        .request(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for("b72_tag_count"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    response
        .extensions()
        .get::<trovato_kernel::telemetry::RequestProfileSnapshot>()
        .expect("the profile is in the response")
        .queries
}

/// Resolving tags costs the same number of queries for one shallow tag as for
/// two tags with deeper ancestry: no query per tag or per ancestor.
#[test]
fn referenced_tags_costs_a_fixed_number_of_queries() {
    run_test(async {
        let app = app();
        let t = tags(app).await;
        let one = create(
            app,
            "B72 count one",
            serde_json::json!({ "field_topics": [t.python.to_string()] }),
        )
        .await
        .unwrap();
        let more = create(
            app,
            "B72 count more",
            serde_json::json!({
                "field_topics": [t.rust.to_string(), t.systems.to_string()],
                "field_venue": [t.hall.to_string()],
            }),
        )
        .await
        .unwrap();

        let few = queries_for(app, &format!("/item/{one}")).await;
        let many = queries_for(app, &format!("/item/{more}")).await;
        assert_eq!(
            few, many,
            "three tags with deeper ancestry ran {many} queries, one tag {few}"
        );
    });
}

/// The stored value is what gather's `has_tag_or_descendants` reads: an item
/// tagged Rust is listed under Languages, its grandparent.
#[test]
fn gather_finds_an_item_by_an_ancestor_tag() {
    run_test(async {
        let app = app();
        let t = tags(app).await;
        let marker = format!("B72 gather {}", Uuid::now_v7().simple());
        let rust_item = create(
            app,
            &format!("{marker} rust"),
            serde_json::json!({ "field_topics": [t.rust.to_string()] }),
        )
        .await
        .unwrap();
        create(
            app,
            &format!("{marker} untagged"),
            serde_json::json!({ "field_note": "none" }),
        )
        .await
        .unwrap();

        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some(PROBE.to_string()),
            filters: vec![
                QueryFilter {
                    field: "fields.field_topics".to_string(),
                    operator: FilterOperator::HasTagOrDescendants,
                    value: FilterValue::Uuid(t.languages),
                    exposed: false,
                    exposed_label: None,
                    widget: Default::default(),
                },
                QueryFilter {
                    field: "title".to_string(),
                    operator: FilterOperator::Contains,
                    value: FilterValue::String(marker.clone()),
                    exposed: false,
                    exposed_label: None,
                    widget: Default::default(),
                },
            ],
            stage_aware: true,
            ..Default::default()
        };
        let ctx = QueryContext {
            current_user_id: None,
            viewer: Some(admin()),
            url_args: HashMap::new(),
            language: None,
        };
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &QueryDisplay::default(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await
            .expect("gather executes");
        let ids: Vec<String> = result
            .items
            .iter()
            .filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(String::from))
            .collect();
        assert_eq!(ids, vec![rust_item.to_string()]);
    });
}

/// The content form renders the field as the tag picker: the reference widget
/// pointed at the tags endpoint for its category, one slot per stored tag,
/// each showing the tag's label.
#[test]
fn the_content_form_renders_the_tag_picker() {
    run_test(async {
        let app = app();
        let t = tags(app).await;
        let id = create(
            app,
            "B72 picker",
            serde_json::json!({ "field_topics": [t.rust.to_string()] }),
        )
        .await
        .unwrap();
        let cookies = app
            .create_and_login_admin(
                "b72tagpicker",
                "b72-tag-picker-pass",
                "b72tagpicker@test.local",
            )
            .await;

        let (status, html) = get_as(
            app,
            &format!("/admin/content/{id}/edit"),
            &cookies,
            "b72tagpicker",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{html}");
        let html = html.replace("&#x2F;", "/");
        assert!(
            html.contains(r#"data-autocomplete-url="/api/v1/tags/autocomplete?category=""#),
            "the picker asks the tags endpoint:\n{html}"
        );
        assert!(
            html.contains(&format!(r#"data-target-type="{TOPICS}""#)),
            "for the field's category:\n{html}"
        );
        assert!(
            html.contains(&format!(r#"name="field_topics[0]" value="{}""#, t.rust)),
            "the stored tag is the first slot's value:\n{html}"
        );
        assert!(
            html.contains(r#"value="Rust""#),
            "and its label is what the box shows:\n{html}"
        );
        assert!(
            html.contains("/static/js/record-ref.js"),
            "the one autocomplete script is loaded"
        );
    });
}

/// The manage fields screen adds a category reference only with a category,
/// and only one that exists.
#[test]
fn the_field_screen_requires_an_existing_category() {
    run_test(async {
        let app = app();
        tags(app).await;
        const TYPE: &str = "b72_tag_field_admin";
        app.cleanup_content_type_on_exit(TYPE);
        sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(TYPE)
            .execute(&app.db)
            .await
            .unwrap();
        app.state
            .content_types()
            .create(
                TYPE,
                "Tag field admin",
                None,
                serde_json::json!({ "fields": [] }),
            )
            .await
            .unwrap();
        let cookies = app
            .create_and_login_admin(
                "b72tagfield",
                "b72-tag-field-pass",
                "b72tagfield@test.local",
            )
            .await;

        let post = |category: &'static str, name: &'static str| {
            let cookies = cookies.clone();
            async move {
                let (status, html) = get_as(
                    app,
                    &format!("/admin/structure/types/{TYPE}/fields"),
                    &cookies,
                    "b72tagfield",
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                assert!(
                    html.contains(&format!(r#"<option value="{TOPICS}""#)),
                    "the add form offers the categories:\n{html}"
                );
                let csrf = input_value(&html, "_token").unwrap();
                let build = input_value(&html, "_form_build_id").unwrap();
                let response = app
                    .request_with_cookies(
                        Request::post(format!("/admin/structure/types/{TYPE}/fields/add"))
                            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                            .header("x-forwarded-for", test_ip_for("b72tagfield"))
                            .body(Body::from(format!(
                                "_token={csrf}&_form_build_id={build}&label=Topics&name={name}\
                                 &field_type=category_reference&category={category}"
                            )))
                            .unwrap(),
                        &cookies,
                    )
                    .await;
                let status = response.status();
                (status, body_string(response).await)
            }
        };

        let (status, html) = post("", "field_none").await;
        assert!(!status.is_redirection(), "no category is refused");
        assert!(html.contains("Category is required"), "{html}");

        let (status, html) = post("b72_no_such_category", "field_unknown").await;
        assert!(!status.is_redirection(), "an unknown category is refused");
        assert!(html.contains("b72_no_such_category"), "{html}");

        let (status, _) = post(TOPICS, "field_topics").await;
        assert!(status.is_redirection(), "{status}");

        let ct = app.state.content_types().get(TYPE).unwrap();
        let names: Vec<&str> = ct.fields.iter().map(|f| f.field_name.as_str()).collect();
        assert_eq!(names, vec!["field_topics"]);
        assert_eq!(
            serde_json::to_value(&ct.fields[0].field_type).unwrap(),
            serde_json::json!({ "CategoryReference": TOPICS }),
        );

        let (status, html) = get_as(
            app,
            &format!("/admin/structure/types/{TYPE}/fields/field_topics/edit"),
            &cookies,
            "b72tagfield",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!(r#"<option value="{TOPICS}" selected"#)),
            "the edit form shows the field's category:\n{html}"
        );
    });
}
