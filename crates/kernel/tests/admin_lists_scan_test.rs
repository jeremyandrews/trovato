#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Admin listings you can scan (B246).
//!
//! - A row action shows its verb alone and is named, through
//!   `aria-labelledby`, by the verb and the row's name.
//! - The AI providers listing's Test form keeps what its script finds it by.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

async fn body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b246scan"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body(response).await)
}

async fn admin(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    let prefix = prefix.to_string();
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name LIKE $1")
            .bind(format!("{prefix}-%"))
            .execute(&pool)
            .await;
    });
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// A page item titled `title`, deleted when the test ends.
async fn item(app: &TestApp, title: &str) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("listing test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

/// The content listing's row: its name element holds the title alone, and
/// each action shows its verb and is named by itself and that element.
#[test]
fn a_row_action_shows_its_verb_and_is_named_by_the_row() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b246rowact").await;
        let title = format!("Scan me {}", Uuid::new_v4().simple());
        let id = item(app, &title).await;
        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/content?q={}", urlencoding::encode(&title)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!(
                r#"<a id="row-{id}" href="/admin/content/{id}/edit">{title}</a>"#
            )),
            "the row's name element holds the title alone"
        );
        assert!(
            html.contains(&format!(
                r#"<a id="act-edit-{id}" aria-labelledby="act-edit-{id} row-{id}" href="&#x2F;admin&#x2F;content&#x2F;{id}&#x2F;edit">Edit</a>"#
            )),
            "Edit shows its verb and is named by the row"
        );
        assert!(
            html.contains(&format!(
                r#"<a id="act-revisions-{id}" aria-labelledby="act-revisions-{id} row-{id}" href="&#x2F;item&#x2F;{id}&#x2F;revisions">Revisions</a>"#
            )),
            "Revisions shows its verb and is named by the row"
        );
        assert!(
            html.contains(&format!(
                r#"form="delete-{id}" id="act-delete-{id}" aria-labelledby="act-delete-{id} row-{id}""#
            )),
            "Delete is named by the row"
        );
        // Its state is a quiet pill: published needs nobody.
        assert!(
            html.contains(r#"<span class="list-status list-status--done">Published</span>"#),
            "{html}"
        );
        // The title is not repeated as the actions' visible text.
        assert!(!html.contains(&format!(">Edit {title}<")), "{html}");
        assert!(!html.contains(&format!(">Revisions of {title}<")), "{html}");
        assert!(!html.contains(&format!(">Delete {title}<")), "{html}");
        // The confirmation still names the item in full.
        assert!(html.contains(&format!(r#"name="_confirm_label" value="Delete {title}""#)));
    });
}

/// The Test button's form keeps the class `ai-admin.js` finds it by; only
/// the button changed. No test presses it: that would send a request to the
/// provider's address.
#[test]
fn the_ai_provider_test_form_keeps_its_script_hook() {
    run_test(async {
        use trovato_kernel::services::ai_provider::{
            AiOperationType, AiProviderConfig, OperationModel, ProviderProtocol,
        };
        let app = shared_app().await;
        let cookies = admin(app, "b246aitest").await;
        let id = Uuid::new_v4().to_string();
        let providers = app.state.ai_providers();
        providers
            .save_provider(AiProviderConfig {
                id: id.clone(),
                label: "B246 listing provider".to_string(),
                protocol: ProviderProtocol::Anthropic,
                base_url: "http://127.0.0.1:9/never-called".to_string(),
                api_key_env: "TROVATO_B246_TEST_KEY_UNSET".to_string(),
                models: vec![OperationModel {
                    operation: AiOperationType::Chat,
                    model: "none".to_string(),
                }],
                rate_limit_rpm: 0,
                enabled: true,
            })
            .await
            .unwrap();
        let (status, html) = get(app, &cookies, "/admin/system/ai-providers").await;
        providers.delete_provider(&id).await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!(
                r#"action="/admin/system/ai-providers/{id}/test" class="inline-form js-test-connection" data-provider-id="{id}""#
            )),
            "{html}"
        );
        assert!(html.contains(&format!(
            r#"id="act-test-{id}" aria-labelledby="act-test-{id} row-{id}" class="link-button">Test</button>"#
        )));
        assert!(html.contains(&format!(
            r#"<span id="row-{id}">B246 listing provider</span>"#
        )));
    });
}

/// With image styles off (the shared app does not enable them) an image in
/// the Files listing shows no picture, and no placeholder.
#[test]
fn without_image_styles_an_image_shows_no_thumbnail() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b246nothumb").await;
        // Another test binary may have left the plugin installed in the
        // database this app read: switch it off here, in memory, and back.
        let was_on = app.state.is_plugin_enabled("trovato_image_styles");
        app.state.set_plugin_enabled("trovato_image_styles", false);
        let owner: Uuid =
            sqlx::query_scalar("SELECT id FROM users WHERE name LIKE 'b246nothumb-%' LIMIT 1")
                .fetch_one(&app.db)
                .await
                .unwrap();
        let tag = Uuid::new_v4().simple().to_string();
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) \
             VALUES ($1, $2, $3, $4, 'image/jpeg', 10, 1, 0, 0)",
        )
        .bind(id)
        .bind(owner)
        .bind(format!("nothumb-{tag}.jpg"))
        .bind(format!("local://2026/11/nothumb-{tag}.jpg"))
        .execute(&app.db)
        .await
        .unwrap();
        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/content/files?q=nothumb-{tag}"),
        )
        .await;
        app.state.set_plugin_enabled("trovato_image_styles", was_on);
        sqlx::query("DELETE FROM file_managed WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&format!(r#"id="row-{id}""#)), "{html}");
        assert!(!html.contains("list-thumbnail"), "{html}");
    });
}

/// A row's key is its machine name when that is a plain slug, and `pos.` with
/// its place in the list otherwise, so the two never meet: a menu named `2`
/// and a menu with spaces in its name shown second each keep their own ids.
#[test]
fn a_slug_key_and_a_positional_key_never_share_an_id() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b246menukey").await;
        let spaced = "2 B246 Menu";
        for menu in ["2", spaced] {
            sqlx::query(
                "INSERT INTO menu_link (id, menu_name, path, title, weight, hidden, plugin, \
                 stage_id, created, changed) \
                 VALUES ($1, $2, '/b246-menu-key', 'B246 key', 0, false, 'core', $3, 0, 0)",
            )
            .bind(Uuid::now_v7())
            .bind(menu)
            .bind(LIVE_STAGE_ID)
            .execute(&app.db)
            .await
            .unwrap();
        }
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM menu_link WHERE path = '/b246-menu-key'")
                .execute(&pool)
                .await;
        });

        // Filtered to `2`, the slug `2` is the first row and the spaced
        // name, which needs a positional key, the second.
        let (status, html) = get(app, &cookies, "/admin/structure/menus?q=2").await;
        assert_eq!(status, StatusCode::OK);
        let first = html.find(">2</a>").expect("the menu named 2");
        let second = html
            .find(&format!(">{spaced}</a>"))
            .expect("the spaced menu");
        assert!(first < second, "the slug sorts first: {html}");
        assert_eq!(html.matches(r#"id="row-2""#).count(), 1, "{html}");
        assert!(
            html.contains(&format!(
                r#"<a id="row-pos.2" href="/admin/structure/menus/{spaced}">{spaced}</a>"#
            )),
            "{html}"
        );
        assert_eq!(html.matches(r#"id="act-links-2""#).count(), 1, "{html}");
        assert_eq!(html.matches(r#"id="act-links-pos.2""#).count(), 1, "{html}");
    });
}

/// Every listing that falls back to the loop index for a row's key writes it
/// with the `pos.` prefix, which no slug key can hold.
#[test]
fn every_positional_row_key_is_prefixed() {
    let mut dirs = vec![common::project_root().join("templates/admin")];
    let mut prefixed = 0;
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "html") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                !text.contains("{% set key = loop.index %}"),
                "{}: a bare loop index can equal a slug key",
                path.display()
            );
            prefixed += text
                .matches(r#"{% set key = "pos." ~ loop.index %}"#)
                .count();
        }
    }
    assert!(prefixed >= 10, "{prefixed}");
}
