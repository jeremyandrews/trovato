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

fn token_in(html: &str) -> Option<String> {
    let at = html.find(r#"name="_token""#)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
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
    let token =
        token_in(&get(app, cookies, bucket, form_page).await).expect("the form has a token");
    let body = format!(
        "_token={}&_form_build_id=x&query_id={query_id}&label=B80%20shapes&description=&definition_json={}&display_json={}",
        urlencoding::encode(&token),
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
        assert!(
            edit.contains("type=\"submit\" class=\"button button--primary\" disabled"),
            "and turns saving off"
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
