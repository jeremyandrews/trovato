#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Link, List and Telephone fields (B302): storage, the one write check every
//! path reaches, and display.
//!
//! `ItemService::create` is the write path every editor, the JSON API and a
//! plugin's `save-item` share; driving it directly here exercises the same
//! check [`crate::content::field_values::check_field_values`] runs on every
//! one of them, without a CSRF round trip for each case. The item page
//! render is driven through the real router, because escaping and the Live
//! alias lookup only happen there.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::CreateItem;
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A fresh type with one field of each new kind, single valued, plus a multi
/// value List.
async fn fresh_type(app: &TestApp) -> String {
    let name = format!("b302_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let fields = vec![
        FieldDefinition::new("field_link", FieldType::Link).label("Link"),
        FieldDefinition::new("field_choice", FieldType::List)
            .label("Choice")
            .allowed_values(&[("gold", "Gold Medal"), ("silver", "Silver Medal")]),
        FieldDefinition::new("field_choices", FieldType::List)
            .label("Choices")
            .cardinality(-1)
            .allowed_values(&[("gold", "Gold Medal"), ("silver", "Silver Medal")]),
        FieldDefinition::new("field_tel", FieldType::Telephone).label("Telephone"),
    ];
    app.state
        .content_types()
        .create(
            &name,
            "B302 probe",
            Some("link/list/telephone fixture"),
            serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() }),
        )
        .await
        .expect("create the fixture type");

    let pool = app.db.clone();
    let registry = std::sync::Arc::clone(app.state.content_types());
    let machine_name = name.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE type = $1")
            .bind(&machine_name)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(&machine_name)
            .execute(&pool)
            .await;
        registry.invalidate(&machine_name);
    });
    name
}

fn forget_item(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("UPDATE item SET current_revision_id = NULL WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item_revision WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

async fn create(
    app: &TestApp,
    item_type: &str,
    title: &str,
    fields: serde_json::Value,
) -> anyhow::Result<trovato_kernel::models::Item> {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(fields),
                stage_id: None,
                language: None,
                log: None,
            },
            &admin(),
        )
        .await
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn item_page(app: &TestApp, cookies: &str, id: Uuid) -> String {
    let response = app
        .request_with_cookies(
            Request::builder()
                .uri(format!("/item/{id}"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_string(response).await
}

/// Every write path's check accepts a well formed value of each kind, with
/// the e164 filled in on write for a Telephone with a country prefix.
#[test]
fn well_formed_values_are_accepted_and_telephone_gets_its_e164() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let target = create(app, &item_type, "B302 target", serde_json::json!({}))
            .await
            .expect("seed target");
        forget_item(app, target.id);

        let item = create(
            app,
            &item_type,
            "B302 accepted",
            serde_json::json!({
                "field_link": {"uri": format!("/item/{}", target.id), "title": "Target", "new_tab": true},
                "field_choice": "gold",
                "field_choices": ["gold", "silver"],
                "field_tel": "+39 0583 123456",
            }),
        )
        .await
        .expect("well formed values are accepted");
        forget_item(app, item.id);

        assert_eq!(
            item.fields["field_tel"],
            serde_json::json!({"value": "+39 0583 123456", "e164": "+390583123456"}),
            "e164 is filled in on write"
        );
        assert_eq!(item.fields["field_choice"], serde_json::json!("gold"));
    });
}

/// The one write check every path reaches refuses a `javascript:` link, an
/// unknown list key, and a telephone number with letters in it.
#[test]
fn the_write_check_refuses_bad_values_of_each_kind() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;

        let err = create(
            app,
            &item_type,
            "B302 bad link",
            serde_json::json!({"field_link": {"uri": "javascript:alert(1)"}}),
        )
        .await
        .expect_err("a javascript: link is refused");
        assert!(format!("{err:#}").contains("Link"), "{err:#}");

        let err = create(
            app,
            &item_type,
            "B302 bad list",
            serde_json::json!({"field_choice": "platinum"}),
        )
        .await
        .expect_err("an unknown list key is refused");
        assert!(format!("{err:#}").contains("platinum"), "{err:#}");

        let err = create(
            app,
            &item_type,
            "B302 bad tel",
            serde_json::json!({"field_tel": "call me maybe"}),
        )
        .await
        .expect_err("letters are refused in a telephone number");
        assert!(format!("{err:#}").contains("Telephone"), "{err:#}");
    });
}

/// The item page renders a Link as an `<a>` with its title, `target="_blank"
/// rel="noopener noreferrer"` only when `new_tab`, a List's label (not its
/// key), and a Telephone as a `tel:` link.
#[test]
fn the_item_page_renders_each_kind() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;

        let item = create(
            app,
            &item_type,
            "B302 display",
            serde_json::json!({
                "field_link": {"uri": "https://example.org/about", "title": "About us", "new_tab": true},
                "field_choice": "gold",
                "field_choices": ["gold", "silver"],
                "field_tel": "+39 0583 123456",
            }),
        )
        .await
        .expect("create item");
        forget_item(app, item.id);

        let html = item_page(app, "", item.id).await;
        assert!(
            html.contains(r#"<a href="https://example.org/about" target="_blank" rel="noopener noreferrer">About us</a>"#),
            "{html}"
        );
        assert!(html.contains("Gold Medal"), "{html}");
        assert!(html.contains("Silver Medal"), "{html}");
        assert!(
            !html.contains(">gold<"),
            "the key is not shown, its label is: {html}"
        );
        assert!(
            html.contains(r#"<a href="tel:+390583123456">+39 0583 123456</a>"#),
            "{html}"
        );
    });
}

/// A Link without `new_tab` renders with no `target`/`rel`, and an item
/// reference (`/item/<uuid>`) renders at its Live alias when it has one.
#[test]
fn a_link_without_new_tab_has_no_target_and_an_item_reference_shows_its_alias() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let target = create(app, &item_type, "B302 alias target", serde_json::json!({}))
            .await
            .expect("seed target");
        forget_item(app, target.id);

        let alias = format!("/b302-alias-{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO url_alias (id, source, alias, language, stage_id, created, changed) \
             VALUES ($1, $2, $3, 'en', $4, 0, 0)",
        )
        .bind(Uuid::now_v7())
        .bind(format!("/item/{}", target.id))
        .bind(&alias)
        .bind(trovato_kernel::models::stage::LIVE_STAGE_ID)
        .execute(&app.db)
        .await
        .expect("insert alias");

        let item = create(
            app,
            &item_type,
            "B302 no new tab",
            serde_json::json!({
                "field_link": {"uri": format!("/item/{}", target.id), "title": "See target"},
            }),
        )
        .await
        .expect("create item");
        forget_item(app, item.id);

        let html = item_page(app, "", item.id).await;
        assert!(
            html.contains(&format!(r#"<a href="{alias}">See target</a>"#)),
            "{html}"
        );
        assert!(!html.contains("target=\"_blank\""), "{html}");
    });
}

/// A stored Link whose `uri` is `javascript:`, written straight to the
/// database bypassing the write check (as a pre-B302 row, or a bug in some
/// other writer, would be), renders as escaped text: never as an `href`.
#[test]
fn a_stored_javascript_link_never_becomes_an_href() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;

        let item = create(app, &item_type, "B302 unsafe seed", serde_json::json!({}))
            .await
            .expect("create item");
        forget_item(app, item.id);

        sqlx::query("UPDATE item SET fields = $1 WHERE id = $2")
            .bind(serde_json::json!({
                "field_link": {"uri": "javascript:alert(document.cookie)", "title": "Click me"}
            }))
            .bind(item.id)
            .execute(&app.db)
            .await
            .expect("write the unsafe value directly");
        // The write bypassed the check, but not the item's current revision,
        // which the item page edit flow (not the page render this test
        // drives) reads: write it too, so the row is internally consistent.
        sqlx::query("UPDATE item_revision SET fields = $1 WHERE id = $2")
            .bind(serde_json::json!({
                "field_link": {"uri": "javascript:alert(document.cookie)", "title": "Click me"}
            }))
            .bind(item.current_revision_id)
            .execute(&app.db)
            .await
            .ok();

        let html = item_page(app, "", item.id).await;
        assert!(
            !html.contains("href=\"javascript:"),
            "a javascript: uri must never become an href: {html}"
        );
        assert!(
            html.contains("Click me"),
            "the title still renders, as text: {html}"
        );
    });
}

/// A stored List value whose key is no longer one of the field's allowed
/// values (an allowed value removed after items already held it, which the
/// admin field edit form permits) renders as the escaped key, not blank and
/// not a crash.
#[test]
fn a_list_key_no_longer_allowed_renders_as_the_escaped_key() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;

        let item = create(
            app,
            &item_type,
            "B302 stale key seed",
            serde_json::json!({}),
        )
        .await
        .expect("create item");
        forget_item(app, item.id);

        sqlx::query("UPDATE item SET fields = $1 WHERE id = $2")
            .bind(serde_json::json!({ "field_choice": "<bronze>" }))
            .bind(item.id)
            .execute(&app.db)
            .await
            .expect("write a since-removed key directly");
        sqlx::query("UPDATE item_revision SET fields = $1 WHERE id = $2")
            .bind(serde_json::json!({ "field_choice": "<bronze>" }))
            .bind(item.current_revision_id)
            .execute(&app.db)
            .await
            .ok();

        let html = item_page(app, "", item.id).await;
        assert!(
            html.contains("&lt;bronze&gt;"),
            "a key no longer allowed still renders, escaped: {html}"
        );
    });
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

/// Pull a named input's `value` attribute out of rendered form HTML.
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

/// Pull a named `<textarea>`'s body out of rendered form HTML.
fn textarea_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let open_end = at + html[at..].find('>')? + 1;
    let close_at = open_end + html[open_end..].find("</textarea>")?;
    Some(html[open_end..close_at].to_string())
}

/// The admin field edit form shows a List field's allowed values as its
/// stored `key|label` pairs, one per line, and a posted change to that
/// textarea is parsed, validated and saved the same way it was before the
/// B83c form pipeline migration rewrote this route.
#[test]
fn the_admin_field_edit_form_round_trips_a_lists_allowed_values() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let cookies = app
            .create_and_login_admin(
                "b302adminfield",
                "b302-admin-field-pass",
                "b302adminfield@test.local",
            )
            .await;
        let path = format!("/admin/structure/types/{item_type}/fields/field_choice/edit");

        let (status, html) = get_as(app, &path, &cookies, "b302adminfield").await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert_eq!(
            textarea_value(&html, "allowed_values").as_deref(),
            Some("gold|Gold Medal\nsilver|Silver Medal"),
            "the edit form shows the field's allowed values:\n{html}"
        );

        let csrf = input_value(&html, "_token").expect("csrf token");
        let build = input_value(&html, "_form_build_id").expect("form build id");

        let response = app
            .request_with_cookies(
                Request::post(&path)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", test_ip_for("b302adminfield"))
                    .body(Body::from(format!(
                        "_token={csrf}&_form_build_id={build}&label=Choice&required=&\
                         cardinality=1&allowed_values=gold%7CGold+Medal%0Abronze%7CBronze+Medal"
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(
            response.status().is_redirection(),
            "a valid allowed values textarea saves: {}",
            body_string(response).await
        );

        let updated = app
            .state
            .content_types()
            .get(&item_type)
            .expect("type still exists");
        let field = updated
            .fields
            .iter()
            .find(|f| f.field_name == "field_choice")
            .expect("field still exists");
        assert_eq!(
            trovato_kernel::content::field_value::list_options(&field.settings),
            vec![
                ("gold".to_string(), "Gold Medal".to_string()),
                ("bronze".to_string(), "Bronze Medal".to_string()),
            ],
            "the posted allowed values are saved, in the posted order"
        );
    });
}

/// The admin field edit form refuses a cardinality other than 1 for a Link
/// field, with a clear message: a Link ships single valued only (B302).
#[test]
fn the_admin_field_edit_form_refuses_a_multi_value_link() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let cookies = app
            .create_and_login_admin(
                "b302adminlink",
                "b302-admin-link-pass",
                "b302adminlink@test.local",
            )
            .await;
        let path = format!("/admin/structure/types/{item_type}/fields/field_link/edit");

        let (status, html) = get_as(app, &path, &cookies, "b302adminlink").await;
        assert_eq!(status, StatusCode::OK, "{html}");
        let csrf = input_value(&html, "_token").expect("csrf token");
        let build = input_value(&html, "_form_build_id").expect("form build id");

        let response = app
            .request_with_cookies(
                Request::post(&path)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", test_ip_for("b302adminlink"))
                    .body(Body::from(format!(
                        "_token={csrf}&_form_build_id={build}&label=Link&required=&cardinality=-1"
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK, "no redirect: refused");
        let html = body_string(response).await;
        assert!(
            html.contains("A Link field holds a single value"),
            "the refusal names why:\n{html}"
        );

        let unchanged = app
            .state
            .content_types()
            .get(&item_type)
            .expect("type still exists");
        assert_eq!(
            unchanged
                .fields
                .iter()
                .find(|f| f.field_name == "field_link")
                .expect("field still exists")
                .cardinality,
            1,
            "the field's cardinality is unchanged"
        );
    });
}
