#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Languages from the admin, and the default language's prefix (B161).
//!
//! Before B161 the languages were read once at startup from configuration,
//! so a language could not be added from the admin and served without a
//! restart, and `/en/...` (the default language's own prefix, which an old
//! site's links carry) was a 404.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::middleware::language::PREFIX_DEFAULT_KEY;
use trovato_kernel::models::{Language, SiteConfig};

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get_response(app: &TestApp, cookies: &str, uri: &str) -> axum::response::Response {
    app.request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = get_response(app, cookies, uri).await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    body: String,
) -> axum::response::Response {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
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

/// The token and build id of the form whose `action` is `action`.
fn form_pairs(html: &str, action: &str) -> String {
    let at = html
        .find(&format!(r#"action="{action}""#))
        .unwrap_or_else(|| panic!("a form posting to {action}"));
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    let scoped = &scoped[..scoped.find("</form>").unwrap()];
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap()),
        urlencoding::encode(&input_value(scoped, "_form_build_id").unwrap()),
    )
}

fn forget_user(app: &TestApp, name: String) {
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&name)
            .execute(&db)
            .await;
    });
}

async fn admin_cookies(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    forget_user(app, name.clone());
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

fn assert_refused(status: StatusCode, who: &str, what: &str) {
    assert!(
        status == StatusCode::FORBIDDEN
            || status == StatusCode::UNAUTHORIZED
            || status.is_redirection(),
        "{what} must be refused to {who}, got {status}"
    );
}

/// The languages pages refuse an anonymous visitor and a signed-in user
/// without `administer languages`, GET and POST.
#[test]
fn the_languages_pages_refuse_anyone_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b161lplain");
        forget_user(app, name.clone());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let id = common::user_id_of(app, &name).await;
        common::grant_via_role(app, id, &["access administration pages"]).await;
        let plain = app.login(&name, "test-password-123").await;

        for (who, cookies) in [("anonymous", ""), ("a plain user", plain.as_str())] {
            for uri in ["/admin/config/languages", "/admin/config/languages/add"] {
                let (status, _) = get(app, cookies, uri).await;
                assert_refused(status, who, &format!("GET {uri}"));
                let response = post_form(app, cookies, uri, "predefined=fr".into()).await;
                assert_refused(response.status(), who, &format!("POST {uri}"));
            }
        }
    });
}

/// **The finding (K-languages-ui).** An administrator adds a language of the
/// site's own from the admin; its prefix serves the front page at once,
/// without a restart, the list shows it beside English, and deleting it
/// takes the prefix away again.
#[test]
fn a_language_added_in_the_admin_is_served_at_once() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b161lang").await;
        let suffix = &uuid::Uuid::now_v7().simple().to_string()[24..30];
        let code = format!("x-{suffix}");
        let db = app.db.clone();
        let cleanup_code = code.clone();
        let state = app.state.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM language WHERE id = $1")
                .bind(&cleanup_code)
                .execute(&db)
                .await;
            let _ = state.reload_languages().await;
        });

        let (status, _) = get(app, "", &format!("/{code}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "not a language yet");

        let (status, html) = get(app, &cookies, "/admin/config/languages/add").await;
        assert_eq!(status, StatusCode::OK);
        let body = format!(
            "{}&predefined=custom&code={code}&label=Pv2+Test&direction=ltr&weight=5",
            form_pairs(&html, "/admin/config/languages/add")
        );
        let response = post_form(app, &cookies, "/admin/config/languages/add", body).await;
        assert!(
            response.status().is_redirection(),
            "an add redirects to the list, got {}",
            response.status()
        );

        let (status, html) = get(app, &cookies, "/admin/config/languages").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Pv2 Test"), "the new language is listed");
        assert!(html.contains("English"), "beside English");
        assert!(html.contains("has been created"), "the message");

        let (status, _) = get(app, "", &format!("/{code}")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "/{code} serves the front page at once"
        );
        assert!(app.state.known_languages().contains(&code));

        let delete = format!("/admin/config/languages/{code}/delete");
        let (status, html) = get(app, &cookies, &delete).await;
        assert_eq!(status, StatusCode::OK);
        let response = post_form(app, &cookies, &delete, form_pairs(&html, &delete)).await;
        assert!(response.status().is_redirection(), "a delete redirects");
        let (status, _) = get(app, "", &format!("/{code}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "the prefix is gone again");

        // The default language cannot be deleted.
        let response = post_form(
            app,
            &cookies,
            "/admin/config/languages/en/delete",
            "op=Delete".into(),
        )
        .await;
        assert!(!response.status().is_redirection());
        assert!(Language::find_by_id(&app.db, "en").await.unwrap().is_some());
    });
}

/// **The finding (K-language-prefix).** `/en/...` (the default language's own
/// prefix) was a 404. With the setting off it is a permanent redirect to the
/// same address without the prefix; with it on, it is served, and the setting
/// is saved from the languages page.
#[test]
fn the_default_language_prefix_redirects_or_serves() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b161prefix").await;
        let db = app.db.clone();
        let state = app.state.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM site_config WHERE key = $1")
                .bind(PREFIX_DEFAULT_KEY)
                .execute(&db)
                .await;
            let _ = state.reload_languages().await;
        });
        let default = app.state.default_language();

        let response = get_response(app, "", &format!("/{default}/user/login?from=old")).await;
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/user/login?from=old"
        );
        let response = get_response(app, "", &format!("/{default}")).await;
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/");

        // Turn it on from the languages page.
        let (status, html) = get(app, &cookies, "/admin/config/languages").await;
        assert_eq!(status, StatusCode::OK);
        let body = format!(
            "{}&prefix_default=1",
            form_pairs(&html, "/admin/config/languages")
        );
        let response = post_form(app, &cookies, "/admin/config/languages", body).await;
        assert!(response.status().is_redirection(), "saved");
        assert_eq!(
            SiteConfig::get(&app.db, PREFIX_DEFAULT_KEY).await.unwrap(),
            Some(serde_json::json!(true))
        );

        let (status, html) = get(app, "", &format!("/{default}/user/login")).await;
        assert_eq!(status, StatusCode::OK, "/{default}/user/login is served");
        assert!(html.contains("password"), "the login form");
        let (status, _) = get(app, "", &format!("/{default}")).await;
        assert_eq!(status, StatusCode::OK, "/{default} is the front page");
        let (status, _) = get(app, "", "/user/login").await;
        assert_eq!(status, StatusCode::OK, "the unprefixed address still works");
    });
}
