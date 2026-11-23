#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Category term reference fields (B137).
//!
//! The `RecordReference` widget (`static/js/record-ref.js`) asks
//! `/api/v1/items/autocomplete` for suggestions with the field's target type.
//! For `category_term` that endpoint searched the item table for items of type
//! `category_term`, of which there are none, so a term reference field offered
//! nothing and could not be filled in by hand. These tests drive the real
//! router: the endpoint answers a term target from the terms, limited to what
//! the field's save check accepts and to callers who may edit the field, and
//! the edit form shows the saved term's name.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use trovato_kernel::content::owned_vocabulary::{self, CreateOwned};
use trovato_kernel::models::{CreateItem, CreateTag, Tag};
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";

fn unique(prefix: &str) -> String {
    format!("{prefix}_{}", &Uuid::now_v7().simple().to_string()[20..])
}

fn admin_context() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A site-wide vocabulary, deleted (with its terms) when the test ends.
async fn vocabulary(app: &TestApp, label: &str) -> String {
    let id = unique("b137v");
    sqlx::query("INSERT INTO category (id, label) VALUES ($1, $2)")
        .bind(&id)
        .bind(label)
        .execute(&app.db)
        .await
        .unwrap();
    cleanup_vocabulary(app, &id);
    id
}

/// A vocabulary owned by `club`, deleted when the test ends.
async fn owned_vocabulary(app: &TestApp, club: &str) -> String {
    let id = unique("b137o");
    owned_vocabulary::create(
        &app.db,
        CreateOwned {
            owner_type: "club".to_string(),
            owner_id: club.to_string(),
            id: id.clone(),
            label: format!("{club} boats"),
            description: None,
            hierarchy: None,
            weight: None,
        },
    )
    .await
    .unwrap();
    cleanup_vocabulary(app, &id);
    id
}

fn cleanup_vocabulary(app: &TestApp, id: &str) {
    let pool = app.db.clone();
    let categories = app.state.categories().clone();
    let id = id.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM category_tag WHERE category_id = $1")
            .bind(&id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM category WHERE id = $1")
            .bind(&id)
            .execute(&pool)
            .await;
        categories.clear_cache();
    });
}

async fn term(app: &TestApp, vocabulary: &str, label: &str) -> Tag {
    app.state
        .categories()
        .create_tag(CreateTag {
            category_id: vocabulary.to_string(),
            label: label.to_string(),
            description: None,
            slug: None,
            weight: None,
            parent_ids: None,
        })
        .await
        .unwrap()
}

/// A content type with an open term field, `field_topics`, and a term field
/// limited to the item's club, `field_boats`. Removed when the test ends.
async fn content_type(app: &TestApp) -> String {
    let item_type = unique("b137_post");
    let fields = vec![
        FieldDefinition::new(
            "field_topics",
            FieldType::RecordReference("category_term".into()),
        )
        .label("Topics"),
        FieldDefinition::new("field_club", FieldType::Text { max_length: None }).label("Club"),
        {
            let mut f = FieldDefinition::new(
                "field_boats",
                FieldType::RecordReference("category_term".into()),
            )
            .label("Boats");
            f.settings = serde_json::json!({
                "vocabulary_owner": {"owner_type": "club", "owner_field": "field_club"}
            });
            f
        },
    ];
    let settings = serde_json::json!({ "fields": fields });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'B137 post', 'B137 fixture', true, 'Title', 'core', $2)",
    )
    .bind(&item_type)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.cleanup_content_type_on_exit(&item_type);
    app.state
        .content_types()
        .replace(&item_type, "B137 post", Some("B137 fixture"), settings)
        .await
        .ok();
    item_type
}

async fn admin(app: &TestApp) -> String {
    let name = common::username("b137admin");
    app.create_and_login_admin(&name, PASSWORD, &format!("{name}@test.local"))
        .await
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// What the widget asks for, as `cookies`: `query` is the rest of the query
/// string after `type=category_term`.
async fn suggest(app: &TestApp, cookies: &str, query: &str) -> Vec<(String, String)> {
    let response = app
        .request_with_cookies(
            Request::get(format!(
                "/api/v1/items/autocomplete?type=category_term&{query}"
            ))
            .header("x-forwarded-for", common::test_ip_for("b137"))
            .body(Body::empty())
            .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_of(response).await;
    let value: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("not JSON ({e}): {body}"));
    value
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {body}"))
        .iter()
        .map(|v| {
            (
                v["id"].as_str().unwrap_or_default().to_string(),
                v["title"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Typing "sail" into a term field offers the two sail terms, with their term
/// ids and names, and not "Rowing".
#[test]
fn a_term_field_offers_matching_terms() {
    common::run_test(async {
        let app = common::shared_app().await;
        let vocabulary = vocabulary(app, "Water sports").await;
        let sailing = term(app, &vocabulary, "Sailing").await;
        let repair = term(app, &vocabulary, "Sail repair").await;
        let rowing = term(app, &vocabulary, "Rowing").await;
        let item_type = content_type(app).await;
        let cookies = admin(app).await;

        let got = suggest(
            app,
            &cookies,
            &format!("q=sail&limit=50&item_type={item_type}&field=field_topics"),
        )
        .await;
        assert!(
            got.contains(&(sailing.id.to_string(), "Sailing".to_string())),
            "Sailing is offered with its term id: {got:?}"
        );
        assert!(
            got.contains(&(repair.id.to_string(), "Sail repair".to_string())),
            "Sail repair is offered with its term id: {got:?}"
        );
        assert!(
            !got.iter().any(|(id, _)| *id == rowing.id.to_string()),
            "Rowing does not match: {got:?}"
        );
        assert!(
            got.iter()
                .all(|(_, title)| title.to_lowercase().starts_with("sail")),
            "a prefix match on the name: {got:?}"
        );

        // The limit is the limit.
        let one = suggest(
            app,
            &cookies,
            &format!("q=sail&limit=1&item_type={item_type}&field=field_topics"),
        )
        .await;
        assert_eq!(one.len(), 1, "{one:?}");
    });
}

/// A field limited to the item's club offers that club's terms only: not
/// another club's, not a site-wide vocabulary's, and nothing on an item with
/// no club. The club comes from the form's owner field, or from the stored
/// item when the form says nothing.
#[test]
fn an_owner_limited_field_offers_only_the_owners_terms() {
    common::run_test(async {
        let app = common::shared_app().await;
        let club = unique("club");
        let other = unique("club");
        let own_vocabulary = owned_vocabulary(app, &club).await;
        let other_vocabulary = owned_vocabulary(app, &other).await;
        let site_vocabulary = vocabulary(app, "Site boats").await;
        let own = term(app, &own_vocabulary, "Sailboat").await;
        let foreign = term(app, &other_vocabulary, "Sailboard").await;
        let site_wide = term(app, &site_vocabulary, "Sail plan").await;
        let item_type = content_type(app).await;
        let cookies = admin(app).await;

        let ids =
            |got: &[(String, String)]| got.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();

        let got = suggest(
            app,
            &cookies,
            &format!("q=sail&limit=50&item_type={item_type}&field=field_boats&owner={club}"),
        )
        .await;
        assert_eq!(ids(&got), vec![own.id.to_string()], "{got:?}");
        assert!(!ids(&got).contains(&foreign.id.to_string()));
        assert!(!ids(&got).contains(&site_wide.id.to_string()));

        // No club on the form and no item: nothing the save would take.
        let got = suggest(
            app,
            &cookies,
            &format!("q=sail&limit=50&item_type={item_type}&field=field_boats"),
        )
        .await;
        assert!(got.is_empty(), "no owner, no terms: {got:?}");

        // An existing item's club, when the form does not name one.
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: item_type.clone(),
                    title: "A club post".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({ "field_club": club })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin_context(),
            )
            .await
            .unwrap();
        let got = suggest(
            app,
            &cookies,
            &format!(
                "q=sail&limit=50&item_type={item_type}&field=field_boats&item={}",
                item.id
            ),
        )
        .await;
        assert_eq!(ids(&got), vec![own.id.to_string()], "{got:?}");

        // The open field on the same type offers site-wide terms, and no
        // club's: an owned vocabulary is its owner's.
        let got = suggest(
            app,
            &cookies,
            &format!("q=sail&limit=50&item_type={item_type}&field=field_topics"),
        )
        .await;
        assert!(ids(&got).contains(&site_wide.id.to_string()), "{got:?}");
        assert!(!ids(&got).contains(&own.id.to_string()), "{got:?}");
        assert!(!ids(&got).contains(&foreign.id.to_string()), "{got:?}");
    });
}

/// A caller who may not edit the field is offered nothing: an anonymous
/// caller, a signed-in user without the type's create permission, and a
/// request that names no field at all.
#[test]
fn a_caller_who_may_not_edit_the_field_is_offered_nothing() {
    common::run_test(async {
        let app = common::shared_app().await;
        let vocabulary = vocabulary(app, "Water sports").await;
        let sailing = term(app, &vocabulary, "Sailing").await;
        let item_type = content_type(app).await;
        let query = format!("q=sail&limit=50&item_type={item_type}&field=field_topics");

        let got = suggest(app, "", &query).await;
        assert!(got.is_empty(), "anonymous: {got:?}");

        let (_, reader) = common::user_holding(app, "b137reader", &[]).await;
        let got = suggest(app, &reader, &query).await;
        assert!(got.is_empty(), "no create permission: {got:?}");

        let (_, writer) =
            common::user_holding(app, "b137writer", &[&format!("create {item_type} content")])
                .await;
        let got = suggest(app, &writer, &query).await;
        assert!(
            got.iter().any(|(id, _)| *id == sailing.id.to_string()),
            "one who may create the type is offered the term: {got:?}"
        );

        // Without the field it is for, there is nothing to decide with.
        let got = suggest(app, &writer, "q=sail&limit=50").await;
        assert!(got.is_empty(), "no field context: {got:?}");
        // Nor for a field that is not a term reference.
        let got = suggest(
            app,
            &writer,
            &format!("q=sail&limit=50&item_type={item_type}&field=field_club"),
        )
        .await;
        assert!(got.is_empty(), "not a term field: {got:?}");
    });
}

/// Reopening an item shows the chosen term's name in the box, not a blank.
#[test]
fn the_edit_form_shows_the_saved_terms_name() {
    common::run_test(async {
        let app = common::shared_app().await;
        let vocabulary = vocabulary(app, "Water sports").await;
        let sailing = term(app, &vocabulary, "Sailing").await;
        let item_type = content_type(app).await;
        let cookies = admin(app).await;

        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: item_type.clone(),
                    title: "Regatta".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({ "field_topics": sailing.id.to_string() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin_context(),
            )
            .await
            .unwrap();

        let response = app
            .request_with_cookies(
                Request::get(format!("/item/{}/edit", item.id))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_of(response).await;
        let at = html
            .find("id=\"field_topics_autocomplete\"")
            .unwrap_or_else(|| panic!("no topics box in the form"));
        let input = &html[at..at + html[at..].find('>').unwrap()];
        assert!(
            input.contains("value=\"Sailing\""),
            "the box shows the term's name: {input}"
        );
        assert!(
            html.contains(&format!("value=\"{}\"", sailing.id)),
            "the hidden input keeps the term id"
        );
    });
}
