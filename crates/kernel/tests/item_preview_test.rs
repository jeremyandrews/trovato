#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Preview before saving (B158, K-preview).
//!
//! The item forms had Save and nothing else: an editor could not see an item
//! as it would look before it was stored, and a type could not ask for a
//! preview first, as Drupal's types do (optional by default, required on some
//! sites). A type's `preview_mode` now decides: `optional` adds a Preview
//! button, `disabled` none, `required` withholds Save until the submission has
//! been previewed. A preview renders the submission as the item page would,
//! above the form shown again, and saves nothing.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};

const OPTIONAL: &str = "b158_preview_optional";
const DISABLED: &str = "b158_preview_disabled";
const REQUIRED: &str = "b158_preview_required";

async fn ensure_type(app: &TestApp, name: &str, mode: Option<&str>) {
    let mut settings = serde_json::json!({
        "fields": [
            { "field_name": "field_note", "field_type": { "Text": { "max_length": null } }, "label": "Note" }
        ]
    });
    if let Some(mode) = mode {
        settings["preview_mode"] = serde_json::Value::from(mode);
    }
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'b158_test', $2) ON CONFLICT (type) DO NOTHING",
    )
    .bind(name)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(name, name, None, settings)
        .await
        .ok();
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get(app: &TestApp, cookies: &str, uri: &str, ip: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    body: String,
    ip: &str,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", ip)
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

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

/// The CSRF token and build id of the item form on `html`.
fn form_pairs(html: &str) -> String {
    let at = html.find(r#"name="_form_build_id""#).expect("a build id");
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap()),
        urlencoding::encode(&input_value(scoped, "_form_build_id").unwrap()),
    )
}

async fn count_titled(app: &TestApp, title: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM item WHERE title = $1")
        .bind(title)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

fn forget_titled(app: &TestApp, title: String) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE title = $1")
            .bind(title)
            .execute(&pool)
            .await;
    });
}

async fn admin(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// **The finding.** Both add forms offer Preview beside Save; a preview shows
/// the title and the field as the item page renders them, the form again with
/// what was posted, and stores nothing.
#[test]
fn a_preview_renders_the_submission_and_saves_nothing() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app, OPTIONAL, None).await;
        let cookies = admin(app, "b158prevadm").await;
        let ip = test_ip_for("b158prev");

        for path in [
            format!("/item/add/{OPTIONAL}"),
            format!("/admin/content/add/{OPTIONAL}"),
        ] {
            let title = format!("Preview probe {}", uuid::Uuid::now_v7().simple());
            forget_titled(app, title.clone());
            let (status, html) = get(app, &cookies, &path, &ip).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(
                html.contains(r#"name="preview""#),
                "{path}: no Preview button"
            );
            assert!(html.contains(r#"name="op""#), "{path}: no Save button");

            let body = format!(
                "{}&title={}&field_note=Seen+before+saving&preview=1",
                form_pairs(&html),
                urlencoding::encode(&title)
            );
            let (status, page) = post_form(app, &cookies, &path, body, &ip).await;
            assert_eq!(status, StatusCode::OK, "{path}: {page}");
            assert!(page.contains("This is a preview"), "{path}: no preview");
            assert!(page.contains(&title), "{path}: the title is not previewed");
            assert!(
                page.contains("Seen before saving"),
                "{path}: the field is not previewed"
            );
            assert_eq!(
                input_value(&page, "title").as_deref(),
                Some(title.as_str()),
                "{path}: the form is not shown again with what was posted"
            );
            assert_eq!(
                count_titled(app, &title).await,
                0,
                "{path}: a preview saved"
            );

            // Save from the form under the preview.
            let body = format!(
                "{}&title={}&field_note=Seen+before+saving",
                form_pairs(&page),
                urlencoding::encode(&title)
            );
            let (status, _) = post_form(app, &cookies, &path, body, &ip).await;
            assert!(
                status.is_success() || status.is_redirection(),
                "{path}: save refused {status}"
            );
            assert_eq!(count_titled(app, &title).await, 1, "{path}: not saved");
        }
    });
}

/// A type whose preview is disabled offers no Preview button.
#[test]
fn a_disabled_preview_offers_no_button() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app, DISABLED, Some("disabled")).await;
        let cookies = admin(app, "b158prevdis").await;
        let (_, html) = get(
            app,
            &cookies,
            &format!("/item/add/{DISABLED}"),
            &test_ip_for("b158prevdis"),
        )
        .await;
        assert!(!html.contains(r#"name="preview""#));
        assert!(html.contains(r#"name="op""#));
    });
}

/// A type whose preview is required offers Save only under a preview, and a
/// save posted without one is refused and stores nothing.
#[test]
fn a_required_preview_withholds_save_until_previewed() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app, REQUIRED, Some("required")).await;
        let cookies = admin(app, "b158prevreq").await;
        let ip = test_ip_for("b158prevreq");
        let path = format!("/item/add/{REQUIRED}");
        let title = format!("Required probe {}", uuid::Uuid::now_v7().simple());
        forget_titled(app, title.clone());

        let (_, html) = get(app, &cookies, &path, &ip).await;
        assert!(html.contains(r#"name="preview""#));
        assert!(
            !html.contains(r#"name="op""#),
            "Save offered before a preview"
        );

        // A save forged without a preview is refused.
        let body = format!(
            "{}&title={}&op=",
            form_pairs(&html),
            urlencoding::encode(&title)
        );
        let (status, page) = post_form(app, &cookies, &path, body, &ip).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(page.contains("Preview this item before saving it."));
        assert_eq!(count_titled(app, &title).await, 0);

        // Previewed (from the form the refusal showed again: a token is
        // spent once), the form offers Save, and the save goes through.
        let body = format!(
            "{}&title={}&preview=1",
            form_pairs(&page),
            urlencoding::encode(&title)
        );
        let (status, page) = post_form(app, &cookies, &path, body, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(r#"name="op""#), "no Save under the preview");
        let body = format!(
            "{}&title={}&op=",
            form_pairs(&page),
            urlencoding::encode(&title)
        );
        let (status, page) = post_form(app, &cookies, &path, body, &ip).await;
        assert!(status.is_success(), "save refused: {status} {page}");
        assert_eq!(count_titled(app, &title).await, 1);
    });
}

/// The content type form stores the mode and shows it again.
#[test]
fn the_type_form_sets_the_preview_mode() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app, DISABLED, Some("disabled")).await;
        let cookies = admin(app, "b158prevtype").await;
        let ip = test_ip_for("b158prevtype");

        // The add form has no stored mode to compare: it shows the default.
        let (status, html) = get(app, &cookies, "/admin/structure/types/add", &ip).await;
        assert_eq!(status, StatusCode::OK, "the add type form failed");
        assert!(
            html.contains(r#"<option value="optional" selected>"#),
            "the add form does not default to optional"
        );

        let path = format!("/admin/structure/types/{DISABLED}/edit");
        let (_, html) = get(app, &cookies, &path, &ip).await;
        assert!(
            html.contains(r#"<option value="disabled" selected>"#),
            "the stored mode is not shown"
        );
        let build = input_value(&html, "_form_build_id").unwrap();
        let token = input_value(&html[html.find("<form").unwrap()..], "_token").unwrap();
        let body = format!(
            "_token={}&_form_build_id={}&label={DISABLED}&machine_name={DISABLED}&title_label=Title&preview_mode=required",
            urlencoding::encode(&token),
            urlencoding::encode(&build)
        );
        let (status, _) = post_form(app, &cookies, &path, body, &ip).await;
        assert!(status.is_redirection(), "type form refused: {status}");
        let mode: Option<String> =
            sqlx::query_scalar("SELECT settings ->> 'preview_mode' FROM item_type WHERE type = $1")
                .bind(DISABLED)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(mode.as_deref(), Some("required"));
        // Back as the other test expects it.
        ensure_type(app, DISABLED, Some("disabled")).await;
    });
}
