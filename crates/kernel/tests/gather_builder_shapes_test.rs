#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B80.3: the admin gather builder does not drop what it cannot show.
//!
//! The builder rebuilds a definition from its own controls when it saves, and
//! it has no control for filter groups or for the summary clauses (`group_by`,
//! `aggregates`, `having`, `distinct`). Saving such a gather through it would
//! quietly lose them, so it refuses: on create, on a save over a stored gather
//! that has them (whatever the form posts), and in the edit form, which says
//! so and turns saving off.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use serde_json::{Value, json};
use trovato_kernel::gather::{GatherQuery, QueryDefinition, QueryDisplay};
use uuid::Uuid;

const REFUSAL: &str = "which this builder cannot show";

/// The query editor's form id (`routes::gather_admin`).
const QUERY_FORM_ID: &str = "gather-form";

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

/// Post the gather form as the builder would, with `definition` as its JSON.
async fn post_form(
    app: &TestApp,
    cookies: &str,
    bucket: &str,
    form_page: &str,
    action: &str,
    query_id: &str,
    definition: &Value,
) -> String {
    let form = get(app, cookies, bucket, form_page).await;
    let body = format!(
        "{}&query_id={query_id}&label=B80%20shapes&description=&definition_json={}&display_json={}",
        common::form_fields(&form, QUERY_FORM_ID),
        urlencoding::encode(&definition.to_string()),
        urlencoding::encode("{}"),
    );
    let response = app
        .request_with_cookies(
            Request::post(action)
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    body_of(response).await
}

fn grouped() -> Value {
    json!({
        "base_table": "item",
        "filter_groups": [{
            "conjunction": "or",
            "filters": [
                {"field": "type", "operator": "equals", "value": "blog"},
                {"field": "type", "operator": "equals", "value": "news"}
            ]
        }]
    })
}

fn query_id() -> String {
    format!("b80.shapes_{}", &Uuid::now_v7().simple().to_string()[20..])
}

async fn forget(app: &TestApp, id: &str) {
    app.state.gather().delete_query(id).await.ok();
}

#[test]
fn creating_a_grouped_gather_through_the_builder_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b80shape-c", &["administer site"]).await;
        let id = query_id();

        let page = post_form(
            app,
            &cookies,
            "b80shape-c",
            "/admin/gather/create",
            "/admin/gather/create",
            &id,
            &grouped(),
        )
        .await;
        let registered = app.state.gather().get_query(&id).is_some();
        forget(app, &id).await;

        assert!(page.contains(REFUSAL), "the form says why");
        assert!(!registered, "nothing was saved");
    });
}

#[test]
fn saving_over_a_stored_grouped_gather_is_refused_and_keeps_it() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b80shape-s", &["administer site"]).await;
        let id = query_id();
        let definition: QueryDefinition = serde_json::from_value(grouped()).unwrap();
        app.state
            .gather()
            .register_query(GatherQuery {
                query_id: id.clone(),
                label: "B80 shapes".to_string(),
                description: None,
                definition,
                display: QueryDisplay::default(),
                plugin: "core".to_string(),
                created: 1,
                changed: 1,
            })
            .await
            .expect("a grouped gather registers");

        let edit = get(
            app,
            &cookies,
            "b80shape-s",
            &format!("/admin/gather/{id}/edit"),
        )
        .await;

        // What the builder posts: its own controls' definition, the group gone.
        let page = post_form(
            app,
            &cookies,
            "b80shape-s",
            &format!("/admin/gather/{id}/edit"),
            &format!("/admin/gather/{id}/save"),
            &id,
            &json!({"base_table": "item"}),
        )
        .await;
        let kept = app.state.gather().get_query(&id).map(|q| {
            serde_json::to_value(&q.definition).unwrap()["filter_groups"]
                .as_array()
                .map_or(0, Vec::len)
        });
        forget(app, &id).await;

        assert!(
            edit.contains("gather-builder-cannot-show"),
            "the edit form says it cannot show the group"
        );
        let form = common::form_in(&edit, QUERY_FORM_ID).expect("the query editor");
        let submit = &form[form.find(r#"type="submit""#).expect("a submit button")..];
        let submit = &submit[..submit.find('>').unwrap()];
        assert!(
            submit.contains(" disabled"),
            "and turns saving off: {submit}"
        );
        assert!(page.contains(REFUSAL), "the save says why");
        assert_eq!(kept, Some(1), "the stored group survives");
    });
}

#[test]
fn a_plain_gather_still_saves_through_the_builder() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b80shape-p", &["administer site"]).await;
        let id = query_id();

        let page = post_form(
            app,
            &cookies,
            "b80shape-p",
            "/admin/gather/create",
            "/admin/gather/create",
            &id,
            &json!({"base_table": "item", "item_type": "page"}),
        )
        .await;
        let registered = app.state.gather().get_query(&id).is_some();
        let edit = get(
            app,
            &cookies,
            "b80shape-p",
            &format!("/admin/gather/{id}/edit"),
        )
        .await;
        forget(app, &id).await;

        assert!(!page.contains(REFUSAL), "{page}");
        assert!(registered, "a plain gather is created");
        assert!(!edit.contains("gather-builder-cannot-show"));
    });
}

// -----------------------------------------------------------------------------
// B83c: the gather screens on the form pipeline (`routes::gather_admin`): the
// query editor `gather-form` (create and edit), the listing's one
// `gather_clone_form` behind every row's Clone button, and the delete
// confirmation `gather_delete_form` the listing's Delete links open.
// -----------------------------------------------------------------------------

/// The listing's clone form id (`routes::gather_admin`).
const CLONE_FORM_ID: &str = "gather_clone_form";

/// The delete confirmation's form id (`routes::gather_admin`).
const DELETE_FORM_ID: &str = "gather_delete_form";

/// Post `body` to `action`; the status and the page.
async fn post_body(
    app: &TestApp,
    cookies: &str,
    bucket: &str,
    action: &str,
    body: String,
) -> (axum::http::StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::post(action)
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

/// A plain gather `id` labelled `label`, as a config import would register it.
async fn plain_gather(app: &TestApp, id: &str, label: &str) {
    let definition: QueryDefinition =
        serde_json::from_value(json!({"base_table": "item", "item_type": "page"})).unwrap();
    app.state
        .gather()
        .register_query(GatherQuery {
            query_id: id.to_string(),
            label: label.to_string(),
            description: None,
            definition,
            display: QueryDisplay::default(),
            plugin: "core".to_string(),
            created: 1,
            changed: 1,
        })
        .await
        .expect("a plain gather registers");
}

#[test]
fn b83c_the_query_editor_is_a_pipeline_form_and_refuses_a_post_without_its_build_id() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b83cgather-e", &["administer site"]).await;
        let id = query_id();

        let page = get(app, &cookies, "b83cgather-e", "/admin/gather/create").await;
        let form = common::form_in(&page, QUERY_FORM_ID).expect("the query editor");
        // What gather-builder.js finds by id, all inside the one form.
        for needle in [
            r#"id="definition_json" name="definition_json""#,
            r#"id="display_json" name="display_json""#,
            r#"id="base_table""#,
            r#"id="add-filter""#,
            r#"id="display_format""#,
            r#"id="feed-settings""#,
            r#"id="preview-btn""#,
        ] {
            assert!(form.contains(needle), "{needle}: {form}");
        }
        assert!(page.contains(r#"<script id="gather-i18n" type="application/json">"#));
        assert!(page.contains(r#"<script src="/static/js/gather-builder.js">"#));

        let token = common::input_value_in(form, "_token").expect("a token");
        let body = format!(
            "_token={}&query_id={id}&label=B83c&description=&definition_json={}&display_json={}",
            urlencoding::encode(&token),
            urlencoding::encode(r#"{"base_table":"item"}"#),
            urlencoding::encode("{}"),
        );
        let (status, text) =
            post_body(app, &cookies, "b83cgather-e", "/admin/gather/create", body).await;
        let registered = app.state.gather().get_query(&id).is_some();
        forget(app, &id).await;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{text}");
        assert!(text.contains("The form has expired"), "{text}");
        assert!(!registered, "an expired form saves nothing");
    });
}

#[test]
fn b83c_the_query_editor_shows_a_missing_label_on_its_field() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b83cgather-v", &["administer site"]).await;
        let id = query_id();

        let page = get(app, &cookies, "b83cgather-v", "/admin/gather/create").await;
        let body = format!(
            "{}&query_id={id}&label=&description=&definition_json={}&display_json={}",
            common::form_fields(&page, QUERY_FORM_ID),
            urlencoding::encode(r#"{"base_table":"item"}"#),
            urlencoding::encode("{}"),
        );
        let (status, html) =
            post_body(app, &cookies, "b83cgather-v", "/admin/gather/create", body).await;
        let registered = app.state.gather().get_query(&id).is_some();
        forget(app, &id).await;

        assert_eq!(status, axum::http::StatusCode::OK, "the form again: {html}");
        let form = common::form_in(&html, QUERY_FORM_ID).expect("the query editor again");
        let field = &form[form.find(r#"id="label""#).expect("the label field")..];
        let field = &field[..field.find('>').unwrap()];
        assert!(field.contains(r#"aria-invalid="true""#), "{field}");
        assert!(form.contains("Label is required."), "{form}");
        assert!(
            form.contains(&format!(r#"value="{id}""#)),
            "the id typed is kept: {form}"
        );
        assert!(!registered, "nothing was saved");
    });
}

#[test]
fn b83c_a_gather_is_cloned_and_deleted_through_pipeline_forms() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b83cgather-d", &["administer site"]).await;
        let id = query_id();
        plain_gather(app, &id, "B83c clone").await;

        let list = get(
            app,
            &cookies,
            "b83cgather-d",
            &format!("/admin/gather?q={id}"),
        )
        .await;
        assert!(
            common::form_in(&list, CLONE_FORM_ID).is_some(),
            "the clone form: {list}"
        );
        assert!(
            list.contains(&format!(
                r#"form="{CLONE_FORM_ID}" formaction="/admin/gather/{id}/clone""#
            )),
            "the row's Clone button belongs to it: {list}"
        );
        let delete = format!("/admin/gather/{id}/delete");
        assert!(
            list.contains(&format!(
                r#"<a class="link-button link-button--danger" href="{delete}">"#
            )),
            "the row links to the delete confirmation: {list}"
        );

        let (status, _) = post_body(
            app,
            &cookies,
            "b83cgather-d",
            &format!("/admin/gather/{id}/clone"),
            common::form_fields(&list, CLONE_FORM_ID),
        )
        .await;
        let copies: Vec<String> = app
            .state
            .gather()
            .list_queries()
            .into_iter()
            .map(|q| q.query_id)
            .filter(|q| q.starts_with(&format!("{id}-copy-")))
            .collect();
        for copy in &copies {
            forget(app, copy).await;
        }
        assert_eq!(status, axum::http::StatusCode::SEE_OTHER);
        assert_eq!(copies.len(), 1, "one copy: {copies:?}");

        let page = get(app, &cookies, "b83cgather-d", &delete).await;
        assert!(page.contains("Delete the B83c clone query?"), "{page}");
        let form = common::form_in(&page, DELETE_FORM_ID).expect("the delete form");
        assert!(form.contains("button--danger"), "{form}");
        let token = common::input_value_in(form, "_token").expect("a token");
        let (refused, _) = post_body(
            app,
            &cookies,
            "b83cgather-d",
            &delete,
            format!("_token={}", urlencoding::encode(&token)),
        )
        .await;
        let kept = app.state.gather().get_query(&id).is_some();

        let page = get(app, &cookies, "b83cgather-d", &delete).await;
        let (status, _) = post_body(
            app,
            &cookies,
            "b83cgather-d",
            &delete,
            common::form_fields(&page, DELETE_FORM_ID),
        )
        .await;
        let gone = app.state.gather().get_query(&id).is_none();
        forget(app, &id).await;

        assert_eq!(refused, axum::http::StatusCode::BAD_REQUEST);
        assert!(kept, "a post without the build id deletes nothing");
        assert_eq!(status, axum::http::StatusCode::SEE_OTHER);
        assert!(gone, "the confirmed delete removes the query");
    });
}
