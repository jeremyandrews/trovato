#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Text formats as stored configuration (B108): the migration's formats
//! render exactly as the code did, `/admin/config/formats` edits them, the
//! registry follows a save, item forms offer only what an editor's roles may
//! use. The config export and import round trip is in
//! `text_format_config_test`.
//!
//! Tests here never change a built-in format: every edit is to a format the
//! test creates and deletes again, so the rendering test cannot race them.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use trovato_kernel::content::FilterPipeline;
use trovato_kernel::content::text_format::{self, FilterKind, TextFormat};
use uuid::Uuid;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
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
    let mut pairs = format!(
        "_token={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap())
    );
    if let Some(build) = input_value(scoped, "_form_build_id") {
        pairs.push_str(&format!("&_form_build_id={}", urlencoding::encode(&build)));
    }
    pairs
}

async fn admin_cookies(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// Inputs every built-in format must render exactly as before the migration.
const CORPUS: &[&str] = &[
    "plain words",
    "line one\nline two",
    "<p>Hi</p><script>alert(1)</script>",
    r#"<a href="https://example.com" onclick="x()" rel="me">link</a>"#,
    r#"<img src="/a.png" alt="A" style="x" width="3">"#,
    "Visit https://example.com/path?q=1 today",
    r#"<table><tr><td colspan="2" class="c">x</td></tr></table>"#,
    "<b>bold</b> & 'quotes' \"double\"",
    r#"<a href="javascript:alert(1)">bad</a>"#,
    "<h2>Title</h2><iframe src=x></iframe><u>u</u>",
];

/// A format the test owns, deleted (with its grants) when the test ends.
fn delete_format_on_exit(app: &TestApp, format: &str) {
    let db = app.db.clone();
    let format = format.to_string();
    common::defer_cleanup(move || async move {
        let _ = text_format::delete(&db, &format).await;
        let _ = text_format::reload(&db).await;
    });
}

fn machine(prefix: &str) -> String {
    format!("{prefix}_{}", &Uuid::now_v7().simple().to_string()[20..])
}

/// **The migration changes nothing.** The three seeded rows, loaded from the
/// database, render every input exactly as the code-defined pipelines did,
/// both as stored and through the view-time `for_format_safe` path.
#[test]
fn the_seeded_formats_render_exactly_as_the_code_defined_ones() {
    run_test(async {
        let app = shared_app().await;
        let stored = text_format::list(&app.db).await.expect("list formats");
        let by = |name: &str| -> TextFormat {
            stored
                .iter()
                .find(|f| f.format == name)
                .unwrap_or_else(|| panic!("{name} is seeded"))
                .clone()
        };
        assert_eq!(by("plain_text").filter, FilterKind::Escape);
        assert_eq!(by("filtered_html").filter, FilterKind::Html);
        assert_eq!(by("full_html").filter, FilterKind::None);
        assert_eq!(
            by("filtered_html"),
            text_format::built_in()
                .into_iter()
                .find(|f| f.format == "filtered_html")
                .unwrap(),
            "the seeded filtered_html row is the code definition"
        );

        text_format::reload(&app.db).await.unwrap();
        for input in CORPUS {
            // Before: the pipelines as the code defined them.
            let before_plain = FilterPipeline::plain_text().process(input);
            let before_filtered = FilterPipeline::filtered_html().process(input);
            let before_full = FilterPipeline::full_html().process(input);
            // After: the stored rows.
            assert_eq!(
                by("plain_text").pipeline().process(input),
                before_plain,
                "{input}"
            );
            assert_eq!(
                by("filtered_html").pipeline().process(input),
                before_filtered,
                "{input}"
            );
            assert_eq!(
                by("full_html").pipeline().process(input),
                before_full,
                "{input}"
            );
            // And the view path: full_html still renders as plain text.
            assert_eq!(
                FilterPipeline::for_format_safe("plain_text").process(input),
                before_plain
            );
            assert_eq!(
                FilterPipeline::for_format_safe("filtered_html").process(input),
                before_filtered
            );
            assert_eq!(
                FilterPipeline::for_format_safe("full_html").process(input),
                before_plain
            );
            assert_eq!(
                FilterPipeline::for_format_safe("no_such_format").process(input),
                before_plain
            );
        }
    });
}

/// Every route refuses an anonymous visitor and a signed-in user without
/// `administer text formats`, GET and POST.
#[test]
fn the_format_screens_refuse_anonymous_and_plain_users() {
    run_test(async {
        let app = shared_app().await;
        let (_, plain) =
            user_holding(app, "b108fplain", &["access content", "administer site"]).await;
        let gets = [
            "/admin/config/formats",
            "/admin/config/formats/add",
            "/admin/config/formats/filtered_html/edit",
        ];
        let posts = [
            "/admin/config/formats/add",
            "/admin/config/formats/filtered_html/edit",
            "/admin/config/formats/filtered_html/delete",
        ];
        for (who, cookies) in [
            ("an anonymous visitor", ""),
            ("a plain user", plain.as_str()),
        ] {
            for uri in gets {
                let (status, _) = get(app, cookies, uri).await;
                assert!(
                    status == StatusCode::FORBIDDEN || status.is_redirection(),
                    "GET {uri} for {who}: {status}"
                );
            }
            for uri in posts {
                let response = post_form(
                    app,
                    cookies,
                    uri,
                    "name=Hacked&allowed_tags=%3Cscript%3E".into(),
                )
                .await;
                let status = response.status();
                assert!(
                    status == StatusCode::FORBIDDEN || status.is_redirection(),
                    "POST {uri} for {who}: {status}"
                );
            }
        }
        let filtered = text_format::find(&app.db, "filtered_html")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(filtered.name, "Filtered HTML", "nothing changed");
        let (status, _) = get(app, &plain, "/admin/config/formats").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    });
}

/// A user holding `administer text formats` (and the permissions they grant)
/// adds a format with a role, edits its allowed tags, and deletes it; the
/// filter follows each save without a restart.
#[test]
fn a_format_is_added_edited_and_deleted_through_the_forms() {
    run_test(async {
        let app = shared_app().await;
        let name = machine("b108fmt");
        let (_, cookies) = user_holding(app, "b108fman", &["administer text formats"]).await;
        let role_id =
            common::create_test_role(app, &format!("b108fmtrole-{}", Uuid::now_v7().simple()))
                .await;
        delete_format_on_exit(app, &name);

        let (status, html) = get(app, &cookies, "/admin/config/formats").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("Filtered HTML") && html.contains("Plain Text"),
            "the list"
        );
        assert!(
            !html.contains("/admin/config/formats/plain_text/delete"),
            "built-ins have no delete"
        );

        // Without the token nothing is saved.
        let response = post_form(
            app,
            &cookies,
            "/admin/config/formats/add",
            format!("name=Basic&format={name}&filter=html&allowed_tags=%3Cp%3E"),
        )
        .await;
        assert!(!response.status().is_redirection());
        assert!(text_format::find(&app.db, &name).await.unwrap().is_none());

        // <script> is refused and the form shown again.
        let (_, html) = get(app, &cookies, "/admin/config/formats/add").await;
        let pairs = form_pairs(&html, "/admin/config/formats/add");
        let response = post_form(
            app,
            &cookies,
            "/admin/config/formats/add",
            format!("{pairs}&name=Basic&format={name}&filter=html&allowed_tags=%3Cp%3E+%3Cscript%3E&weight=5"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_string(response).await;
        assert!(html.contains("can never be allowed"), "the error is shown");

        // Saved. The role posted is not granted: a non-superuser may grant only
        // a permission they hold, and this one does not hold `use <name>`.
        let pairs = form_pairs(&html, "/admin/config/formats/add");
        let response = post_form(
            app,
            &cookies,
            "/admin/config/formats/add",
            format!(
                "{pairs}&name=Basic&format={name}&filter=html&allowed_tags=%3Cp%3E+%3Cem%3E&allowed_attributes=&convert_urls=1&weight=5&roles%5B%5D={role_id}"
            ),
        )
        .await;
        assert!(
            response.status().is_redirection(),
            "saved: {}",
            response.status()
        );
        let stored = text_format::find(&app.db, &name)
            .await
            .unwrap()
            .expect("stored");
        assert_eq!(stored.allowed_tags, vec!["p", "em"]);
        assert!(stored.convert_urls);
        let granted: Vec<String> =
            sqlx::query_scalar("SELECT permission FROM role_permissions WHERE role_id = $1")
                .bind(role_id)
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert!(
            granted.is_empty(),
            "not granted beyond the editor's own authority"
        );
        assert_eq!(
            FilterPipeline::for_format(&name).process("<p><em>a</em><b>b</b></p>"),
            "<p><em>a</em>b</p>",
            "the registry has the new format"
        );

        // Edit: allow <b> too; the next render keeps it.
        let (status, html) =
            get(app, &cookies, &format!("/admin/config/formats/{name}/edit")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("&lt;p&gt; &lt;em&gt;"),
            "the form shows the tags"
        );
        let pairs = form_pairs(&html, &format!("/admin/config/formats/{name}/edit"));
        let response = post_form(
            app,
            &cookies,
            &format!("/admin/config/formats/{name}/edit"),
            format!(
                "{pairs}&name=Basic+HTML&filter=html&allowed_tags=%3Cp%3E+%3Cem%3E+%3Cb%3E&weight=5"
            ),
        )
        .await;
        assert!(response.status().is_redirection());
        assert_eq!(
            FilterPipeline::for_format(&name).process("<p><em>a</em><b>b</b></p>"),
            "<p><em>a</em><b>b</b></p>",
            "the save invalidated the cached format"
        );

        // Delete through the confirmation.
        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/config/formats/{name}/delete"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let pairs = form_pairs(&html, &format!("/admin/config/formats/{name}/delete"));
        let response = post_form(
            app,
            &cookies,
            &format!("/admin/config/formats/{name}/delete"),
            pairs,
        )
        .await;
        assert!(response.status().is_redirection());
        assert!(text_format::find(&app.db, &name).await.unwrap().is_none());
        assert!(text_format::get(&name).is_none(), "gone from the registry");
    });
}

/// The three built-in formats cannot be deleted, and their filter cannot be
/// changed; their tags can.
#[test]
fn built_in_formats_cannot_be_deleted() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b108fbuilt").await;
        for format in ["plain_text", "filtered_html", "full_html"] {
            let (status, _) = get(
                app,
                &cookies,
                &format!("/admin/config/formats/{format}/delete"),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "GET delete {format}");
            let response = post_form(
                app,
                &cookies,
                &format!("/admin/config/formats/{format}/delete"),
                String::new(),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "POST delete {format}"
            );
            assert!(text_format::find(&app.db, format).await.unwrap().is_some());
        }
        assert!(
            text_format::delete(&app.db, "filtered_html").await.is_err(),
            "the service refuses too"
        );
        // The edit form of a built-in format has no filter choice.
        let (_, html) = get(app, &cookies, "/admin/config/formats/full_html/edit").await;
        assert!(!html.contains(r#"name="filter""#), "the filter is fixed");
    });
}

/// A superuser grants a role a new format from its form; an editor in that
/// role is offered it on the item form and may save with it, and an editor
/// without it is not offered it and is refused when posting it anyway.
#[test]
fn fields_offer_only_the_formats_an_editors_roles_may_use() {
    run_test(async {
        let app = shared_app().await;
        let admin = admin_cookies(app, "b108fsu").await;
        let name = machine("b108field");
        delete_format_on_exit(app, &name);
        let role_name = format!("b108fieldrole-{}", Uuid::now_v7().simple());
        let role_id = common::create_test_role(app, &role_name).await;

        let (_, html) = get(app, &admin, "/admin/config/formats/add").await;
        let pairs = form_pairs(&html, "/admin/config/formats/add");
        let response = post_form(
            app,
            &admin,
            "/admin/config/formats/add",
            format!("{pairs}&name=Club+HTML&format={name}&filter=html&allowed_tags=%3Cp%3E&weight=9&roles%5B%5D={role_id}"),
        )
        .await;
        assert!(
            response.status().is_redirection(),
            "saved: {}",
            response.status()
        );
        let granted: Vec<String> =
            sqlx::query_scalar("SELECT permission FROM role_permissions WHERE role_id = $1")
                .bind(role_id)
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert!(
            granted.contains(&format!("use {name}")),
            "the role holds the format"
        );

        // An editor in the role.
        let (with_id, with) =
            user_holding(app, "b108fwith", &["create page content", "access content"]).await;
        trovato_kernel::models::Role::assign_to_user(&app.db, with_id, role_id)
            .await
            .unwrap();
        app.state.permissions().invalidate_user(with_id);
        let (status, html) = get(app, &with, "/item/add/page").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!(r#"value="{name}""#)),
            "offered to the role"
        );

        // An editor without it.
        let (_, without) = user_holding(
            app,
            "b108fwithout",
            &["create page content", "access content"],
        )
        .await;
        let (status, html) = get(app, &without, "/item/add/page").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!html.contains(&format!(r#"value="{name}""#)), "not offered");
        let pairs = form_pairs(&html, "/item/add/page");
        let response = post_form(
            app,
            &without,
            "/item/add/page",
            format!("{pairs}&title=B108+format+probe&body=%3Cp%3Ex%3C%2Fp%3E&body_format={name}&status=1"),
        )
        .await;
        assert!(
            !response.status().is_redirection(),
            "refused: {}",
            response.status()
        );
        let html = body_string(response).await;
        assert!(
            html.contains(&format!("you may not use the {name} text format")),
            "the refusal is shown"
        );
    });
}
