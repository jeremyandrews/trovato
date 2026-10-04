#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The two item-form stacks agree (**K1 fix 4**, G-ITEM-FORM-MISMATCH).
//!
//! `GET /item/add/{type}` renders an HTML `<form>`; an HTML form posts
//! `application/x-www-form-urlencoded`; the matching `POST` extracted
//! `Json<CreateItemRequest>`. **Submitting the page the kernel had just
//! rendered was a 415 by construction**, and no JavaScript in `static/js/`
//! intercepted it. The edit pair had the same defect, plus a URL-alias input
//! concatenated after `</form>` where a browser would never submit it. And
//! `FormBuilder` read `{"value": …}` while the working admin stack wrote flat
//! scalars, so even a successful save rendered back empty.
//!
//! These drive the **real** router over HTTP: render the page, scrape its
//! fields, post them back exactly as a browser would, and read the result.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use serde_json::json;
use uuid::Uuid;

const TYPE: &str = "k1_form_probe";
const REF_TYPE: &str = "k1_form_target";

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Register the two probe content types, as `tap_item_info` would.
async fn ensure_types(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    for (name, label, fields) in [
        (
            REF_TYPE,
            "K1 Form Target",
            vec![
                FieldDefinition::new("field_note", FieldType::Text { max_length: None })
                    .label("Note"),
            ],
        ),
        (
            TYPE,
            "K1 Form Probe",
            vec![
                FieldDefinition::new("field_headline", FieldType::Text { max_length: None })
                    .label("Headline"),
                FieldDefinition::new(
                    "field_target",
                    FieldType::RecordReference(REF_TYPE.to_string()),
                )
                .label("Target"),
            ],
        ),
    ] {
        let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $2, '', true, 'Title', 'k1_test', $3) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(name)
        .bind(label)
        .bind(&settings)
        .execute(&app.db)
        .await
        .unwrap();

        app.state
            .content_types()
            .create(name, label, None, settings)
            .await
            .ok();
    }
}

/// Pull a hidden or text input's value out of rendered form HTML.
fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    // The value attribute may sit before or after `name`, so scan the whole tag.
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// **The finding, closed.** Render the add page, post its own fields back the
/// way a browser would, and get an item — not a 415.
#[test]
fn the_rendered_add_form_can_be_submitted() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = app
            .create_and_login_admin(
                "k1formadd",
                "correct-horse-battery-staple",
                "k1formadd@test.local",
            )
            .await;

        // 1. The kernel renders the page.
        let page = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/item/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = body_string(page).await;
        assert!(
            html.contains(r#"method="post""#) && html.contains(r#"name="title""#),
            "expected a submittable form"
        );

        // A form cannot set the X-CSRF-Token header the JSON path reads, so the
        // token has to be in the body or the page is unsubmittable by design.
        let csrf = input_value(&html, "_csrf").expect("the form must carry a CSRF token");
        assert!(!csrf.is_empty());

        // 2. The browser posts it back, urlencoded, exactly as rendered.
        let form = format!(
            "_csrf={}&title={}&field_headline={}&status=1",
            urlencoding::encode(&csrf),
            urlencoding::encode("Posted from the rendered page"),
            urlencoding::encode("A headline & <b>markup</b>")
        );
        let response = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(format!("/item/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap(),
                &cookies,
            )
            .await;

        // Before the fix this was a 415 — the page could be rendered but never
        // successfully submitted.
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "urlencoded submission rejected: {}",
            body_string(response).await
        );

        // 3. And it round-trips: the saved value renders back into the edit
        //    form, which the `{"value": …}`-only reader used to leave empty.
        let id: Uuid = sqlx::query_scalar("SELECT id FROM item WHERE type = $1 AND title = $2")
            .bind(TYPE)
            .bind("Posted from the rendered page")
            .fetch_one(&app.db)
            .await
            .expect("the submitted item exists");

        let edit = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/item/{id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(edit.status(), StatusCode::OK);
        let edit_html = body_string(edit).await;
        assert_eq!(
            input_value(&edit_html, "field_headline").as_deref(),
            Some("A headline &amp; &lt;b&gt;markup&lt;/b&gt;"),
            "the saved value must render back into the form, HTML-escaped"
        );

        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

/// The JSON API is untouched: same body, same CSRF header, same result.
#[test]
fn the_json_api_path_is_unchanged() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = app
            .create_and_login_admin(
                "k1formjson",
                "correct-horse-battery-staple",
                "k1formjson@test.local",
            )
            .await;

        // The CSRF token a JSON client obtains from the rendered page, sent the
        // way a JSON client sends it — in the header, not the body.
        let page = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/item/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let csrf = input_value(&body_string(page).await, "_csrf").unwrap();

        let response = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(format!("/item/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &csrf)
                    .body(Body::from(
                        json!({
                            "title": "Posted as JSON",
                            "status": 1,
                            "fields": {"field_headline": {"value": "wrapped"}}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        // A JSON client's `{"value": …}` wrapper still renders back — the form
        // reads both shapes rather than picking a winner and stranding data.
        let id: Uuid =
            sqlx::query_scalar("SELECT id FROM item WHERE type = $1 AND title = 'Posted as JSON'")
                .bind(TYPE)
                .fetch_one(&app.db)
                .await
                .unwrap();
        let edit = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/item/{id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = body_string(edit).await;
        assert_eq!(
            input_value(&html, "field_headline").as_deref(),
            Some("wrapped"),
            "the wrapped shape must still render"
        );

        // And a JSON POST with no CSRF header is still refused.
        let refused = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(format!("/item/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"title": "No token"}).to_string()))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

/// A form submission with a bad `_csrf` is refused, so widening the accepted
/// encodings did not open a hole.
#[test]
fn a_form_submission_without_a_valid_token_is_refused() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = app
            .create_and_login_admin(
                "k1formcsrf",
                "correct-horse-battery-staple",
                "k1formcsrf@test.local",
            )
            .await;

        for body in [
            "title=Forged&status=1".to_string(),
            "_csrf=not-a-real-token&title=Forged&status=1".to_string(),
        ] {
            let response = app
                .request_with_cookies(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/item/add/{TYPE}"))
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .body(Body::from(body.clone()))
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "a form body without a valid CSRF token must be refused: {body}"
            );
        }
    });
}

/// **The RecordReference round-trip.** The widget's JavaScript writes a bare
/// uuid into the hidden input; the form used to read `value.target_id`, so the
/// reference emptied itself on every edit — which is why Argus M3 modelled a
/// feed's topic as a plain `Text` uuid instead (deviation 3).
#[test]
fn a_record_reference_survives_an_edit() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = app
            .create_and_login_admin(
                "k1formref",
                "correct-horse-battery-staple",
                "k1formref@test.local",
            )
            .await;

        // A target to point at.
        let target = Uuid::now_v7();
        let author: Uuid = sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
            .fetch_one(&app.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO item (id, type, title, fields, status, author_id, created, changed) \
             VALUES ($1, $2, 'The Target Topic', '{}'::jsonb, 1, $3, 0, 0)",
        )
        .bind(target)
        .bind(REF_TYPE)
        .bind(author)
        .execute(&app.db)
        .await
        .unwrap();

        // Create the referring item through the form, as the widget would:
        // the hidden input holds a bare uuid.
        let page = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/item/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let csrf = input_value(&body_string(page).await, "_csrf").unwrap();
        let form = format!(
            "_csrf={}&title=Refers&field_target={target}&status=1",
            urlencoding::encode(&csrf)
        );
        let created = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(format!("/item/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(created.status(), StatusCode::OK);

        let id: Uuid =
            sqlx::query_scalar("SELECT id FROM item WHERE type = $1 AND title = 'Refers'")
                .bind(TYPE)
                .fetch_one(&app.db)
                .await
                .unwrap();

        // Re-render the edit form: the reference must still be there.
        let edit = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/item/{id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = body_string(edit).await;
        assert_eq!(
            input_value(&html, "field_target").as_deref(),
            Some(target.to_string().as_str()),
            "the reference must survive the round trip"
        );
        // And the editor can see *what* it points at, not just that it is set.
        assert!(
            html.contains("The Target Topic"),
            "the target's title must be shown in the autocomplete box"
        );

        // Saving the edit unchanged must not drop it either — the failure mode
        // that made the widget unusable.
        let csrf = input_value(&html, "_csrf").unwrap();
        let target_value = input_value(&html, "field_target").unwrap();
        let form = format!(
            "_csrf={}&title=Refers&field_target={}&status=1",
            urlencoding::encode(&csrf),
            urlencoding::encode(&target_value)
        );
        let saved = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(format!("/item/{id}/edit"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(saved.status(), StatusCode::OK);

        let stored: serde_json::Value = sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(
            stored["field_target"].as_str(),
            Some(target.to_string().as_str()),
            "an unchanged edit must not blank the reference, got {stored}"
        );

        sqlx::query("DELETE FROM item WHERE id = ANY($1)")
            .bind(vec![id, target])
            .execute(&app.db)
            .await
            .unwrap();
    });
}

// ============================================================================
// S4 — the published state is a permission, and the form says so.
//
// `status` came straight off the form and out of the JSON body, with no
// permission behind it: anyone who could create content could put it live. The
// control is now rendered only for a holder of `publish content`, and the
// submission carries `_status_rendered` to say it was rendered — without which
// an absent checkbox would read as "unpublish" on every save by a user who was
// never offered one.
// ============================================================================

use trovato_kernel::models::Role;

const PUBLISH: &str = "publish content";

/// Grant `permissions` through a role, the way a real site does.
async fn grant_via_role(app: &TestApp, user_id: Uuid, permissions: &[&str]) {
    let role = Role::create(&app.db, &format!("k1pub-{}", Uuid::now_v7().simple()))
        .await
        .expect("create role");
    for permission in permissions {
        Role::add_permission(&app.db, role.id, permission)
            .await
            .expect("add permission to role");
    }
    Role::assign_to_user(&app.db, user_id, role.id)
        .await
        .expect("assign role to user");
    app.state.permissions().invalidate_user(user_id);
}

/// A logged-in non-superuser holding exactly `permissions`.
async fn editor_holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> String {
    let name = format!("{prefix}-{}", Uuid::now_v7().simple());
    app.create_test_user(&name, "test-password-123", &format!("{name}@test.local"))
        .await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("the test user exists");
    grant_via_role(app, id, permissions).await;
    app.login(&name, "test-password-123").await
}

/// The permissions a content editor holds, with or without `publish content`.
fn editor_perms(may_publish: bool) -> Vec<&'static str> {
    let mut perms = vec![
        "create k1_form_probe content",
        "edit any content",
        "access content",
    ];
    if may_publish {
        perms.push(PUBLISH);
    }
    perms
}

async fn get_page(app: &TestApp, path: &str, cookies: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder().uri(path).body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(app: &TestApp, path: &str, cookies: &str, body: String) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_json(
    app: &TestApp,
    path: &str,
    cookies: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", csrf)
                .body(Body::from(body.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn status_of(app: &TestApp, id: Uuid) -> i16 {
    sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .expect("read stored status")
}

async fn id_of(app: &TestApp, title: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM item WHERE type = $1 AND title = $2")
        .bind(TYPE)
        .bind(title)
        .fetch_one(&app.db)
        .await
        .expect("the submitted item exists")
}

#[test]
fn the_rendered_add_form_offers_no_published_control_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = editor_holding(app, "k1pubno", &editor_perms(false)).await;

        let (status, html) = get_page(app, &format!("/item/add/{TYPE}"), &cookies).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !html.contains(r#"name="status""#),
            "a user who may not publish is offered no published checkbox"
        );
        assert!(!html.contains("_status_rendered"));

        // And submitting the page as rendered creates an unpublished item,
        // rather than the published one `Item::create`'s default would have
        // made of a submission that said nothing.
        let csrf = input_value(&html, "_csrf").expect("csrf token");
        let title = format!("Draft by an editor {}", Uuid::now_v7().simple());
        let (status, body) = post_form(
            app,
            &format!("/item/add/{TYPE}"),
            &cookies,
            format!(
                "_csrf={}&title={}&field_headline=hi",
                urlencoding::encode(&csrf),
                urlencoding::encode(&title)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the form must still submit: {body}");

        let id = id_of(app, &title).await;
        assert_eq!(
            status_of(app, id).await,
            0,
            "a user who may not publish creates a draft"
        );

        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

#[test]
fn editing_a_published_item_through_the_rendered_form_leaves_it_published() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = editor_holding(app, "k1pubkeep", &editor_perms(false)).await;

        // A published item, as an administrator left it.
        let title = format!("Published already {}", Uuid::now_v7().simple());
        let admin_ctx = trovato_kernel::tap::UserContext::administrator(
            Uuid::nil(),
            vec!["administer site".to_string()],
        );
        let item = app
            .state
            .items()
            .create(
                trovato_kernel::models::CreateItem {
                    item_type: TYPE.to_string(),
                    title: title.clone(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(json!({"field_headline": "live"})),
                    stage_id: None,
                    language: Some("en".to_string()),
                    log: Some("S4 publish test".to_string()),
                },
                &admin_ctx,
            )
            .await
            .expect("seed a published item");

        // The editor saves the form as rendered: no status control, so the
        // submission says nothing about it — and the item stays published.
        // Before `_status_rendered`, this request unpublished the page.
        let (_, html) = get_page(app, &format!("/item/{}/edit", item.id), &cookies).await;
        let csrf = input_value(&html, "_csrf").expect("csrf token");
        let (status, body) = post_form(
            app,
            &format!("/item/{}/edit", item.id),
            &cookies,
            format!(
                "_csrf={}&title={}&field_headline=edited",
                urlencoding::encode(&csrf),
                urlencoding::encode(&title)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the edit must save: {body}");
        assert_eq!(
            status_of(app, item.id).await,
            1,
            "an editor saving a published page must not unpublish it"
        );

        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(item.id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

#[test]
fn a_json_create_asking_to_publish_is_refused_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = editor_holding(app, "k1pubjson", &editor_perms(false)).await;

        let (_, html) = get_page(app, &format!("/item/add/{TYPE}"), &cookies).await;
        let csrf = input_value(&html, "_csrf").expect("csrf token");

        let (status, body) = post_json(
            app,
            &format!("/item/add/{TYPE}"),
            &cookies,
            &csrf,
            json!({"title": "Published by API", "status": 1}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "`status: 1` without `{PUBLISH}` must be 403, got {status}: {body}"
        );

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM item WHERE type = $1 AND title = 'Published by API')",
        )
        .bind(TYPE)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(!exists, "the refused create must not have landed");
    });
}

#[test]
fn a_json_update_changing_the_published_state_is_refused_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = editor_holding(app, "k1pubunpub", &editor_perms(false)).await;

        let title = format!("Live page {}", Uuid::now_v7().simple());
        let admin_ctx = trovato_kernel::tap::UserContext::administrator(
            Uuid::nil(),
            vec!["administer site".to_string()],
        );
        let item = app
            .state
            .items()
            .create(
                trovato_kernel::models::CreateItem {
                    item_type: TYPE.to_string(),
                    title: title.clone(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: None,
                    stage_id: None,
                    language: Some("en".to_string()),
                    log: Some("S4 publish test".to_string()),
                },
                &admin_ctx,
            )
            .await
            .expect("seed a published item");

        let (_, html) = get_page(app, &format!("/item/{}/edit", item.id), &cookies).await;
        let csrf = input_value(&html, "_csrf").expect("csrf token");

        let (status, body) = post_json(
            app,
            &format!("/item/{}/edit", item.id),
            &cookies,
            &csrf,
            json!({"title": title, "status": 0}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "unpublishing without `{PUBLISH}` must be 403, got {status}: {body}"
        );
        assert_eq!(
            status_of(app, item.id).await,
            1,
            "the page must still be published"
        );

        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(item.id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

#[test]
fn the_permission_holder_publishes_through_both_paths() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let cookies = editor_holding(app, "k1pubyes", &editor_perms(true)).await;

        // The form offers the control, and checking it publishes.
        let (status, html) = get_page(app, &format!("/item/add/{TYPE}"), &cookies).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(r#"name="status""#) && html.contains("_status_rendered"),
            "`{PUBLISH}` is offered the control"
        );
        let csrf = input_value(&html, "_csrf").expect("csrf token");
        let title = format!("Published by a publisher {}", Uuid::now_v7().simple());
        let (status, body) = post_form(
            app,
            &format!("/item/add/{TYPE}"),
            &cookies,
            format!(
                "_csrf={}&title={}&field_headline=hi&_status_rendered=1&status=1",
                urlencoding::encode(&csrf),
                urlencoding::encode(&title)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let id = id_of(app, &title).await;
        assert_eq!(status_of(app, id).await, 1, "the publisher published it");

        // And unpublishing over JSON works for them.
        let (_, html) = get_page(app, &format!("/item/{id}/edit"), &cookies).await;
        let csrf = input_value(&html, "_csrf").expect("csrf token");
        let (status, body) = post_json(
            app,
            &format!("/item/{id}/edit"),
            &cookies,
            &csrf,
            json!({"title": title, "status": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(status_of(app, id).await, 0, "and unpublished it again");

        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

// ============================================================================
// Problem 4 — the edit form prefilled a reference title with an unchecked load.
//
// `edit_item_form` resolves each RecordReference target's title so the
// autocomplete box comes back showing what the field points at. It loaded the
// target with `ItemService::load`, which asks nobody anything, so an editor who
// may edit this item learned the title of a target they may not view. The id
// was always theirs — they can read it in the form value — but the title was
// not.
// ============================================================================

#[test]
fn a_reference_title_is_prefilled_only_when_the_editor_may_view_the_target() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;

        // An editor of the referring type, with no view permission over the
        // target type: the published fast path is the only way they see one.
        let (_, cookies) =
            common::user_holding(app, "k1refaccess", &[&format!("edit any {TYPE}")]).await;

        let author: Uuid = sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
            .fetch_one(&app.db)
            .await
            .unwrap();

        // Two targets, one unpublished and one published, each with a referring
        // item of its own. Two pairs rather than one target published half way
        // through: `ItemService::load` caches, so a raw `UPDATE` would not be
        // seen and the second half of the test would be measuring the cache.
        let mut ids = Vec::new();
        for (n, (title, status)) in [("Secret Target Topic", 0i16), ("Open Target Topic", 1i16)]
            .into_iter()
            .enumerate()
        {
            let target = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO item (id, type, title, fields, status, author_id, created, changed) \
                 VALUES ($1, $2, $3, '{}'::jsonb, $4, $5, 0, 0)",
            )
            .bind(target)
            .bind(REF_TYPE)
            .bind(title)
            .bind(status)
            .bind(author)
            .execute(&app.db)
            .await
            .unwrap();

            let item = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO item (id, type, title, fields, status, author_id, created, changed) \
                 VALUES ($1, $2, $3, $4::jsonb, 1, $5, 0, 0)",
            )
            .bind(item)
            .bind(TYPE)
            .bind(format!("Referring Item {n}"))
            .bind(json!({ "field_target": target.to_string() }).to_string())
            .bind(author)
            .execute(&app.db)
            .await
            .unwrap();

            ids.push((item, target, title));
        }

        for (item, target, title) in &ids {
            let html = body_string(
                app.request_with_cookies(
                    Request::builder()
                        .uri(format!("/item/{item}/edit"))
                        .header("x-forwarded-for", common::test_ip_for("k1refaccess"))
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await,
            )
            .await;

            // The editor's own stored value survives either way: the id was
            // always theirs to read, it is in the form they are editing.
            assert_eq!(
                input_value(&html, "field_target").as_deref(),
                Some(target.to_string().as_str()),
                "the stored reference must survive, got {html}"
            );

            if *title == "Secret Target Topic" {
                assert!(
                    !html.contains(title),
                    "the title of a target this editor may not view must not be \
                     prefilled, got {html}"
                );
            } else {
                assert!(
                    html.contains(title),
                    "a viewable target's title must still be prefilled, got {html}"
                );
            }
        }

        let to_delete: Vec<Uuid> = ids
            .iter()
            .flat_map(|(item, target, _)| [*item, *target])
            .collect();
        sqlx::query("DELETE FROM item WHERE id = ANY($1)")
            .bind(to_delete)
            .execute(&app.db)
            .await
            .unwrap();
    });
}
