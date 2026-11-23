#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A13: page builder components a plugin ships.
//!
//! `plugins/test_page_builder_components` declares two components in
//! `components/*.component.toml` (`PromoCards`, a repeatable list of cards;
//! `FeatureBanner`, a select variant and an image), renders them with its own
//! `templates/pb/` and styles them from its own `static/`. Nothing about them
//! is in the kernel. These run the real app with the plugin enabled and check
//! the four things a plugin author relies on: the registry lists them, a page
//! using them saves, an invalid one is refused with an error at each field,
//! and the page renders to the committed snapshot with their stylesheets.
//!
//! A20: the same plugin declares page root fields in
//! `components/page.root.toml` (`footer_cta_mode`, a select of default, hide
//! and custom; `footer_cta_content`, rich text) and ships
//! `templates/elements/tile--custom_html--pb_footer_cta.html`, a sitewide
//! footer call to action that follows them. The tests below check that the
//! registry serves the root fields, each mode saves and renders the right
//! footer, an invalid root value is refused at its field, and a page saved
//! before root fields existed validates and renders the same HTML.
//!
//! Requires Postgres, Redis and the `test_page_builder_components` fixture
//! `.wasm` (built by CI before the test job).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use trovato_kernel::content::PageBuilderInvalid;
use trovato_kernel::content::page_builder::ResolvedReferences;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PLUGIN: &str = "test_page_builder_components";
const TYPE: &str = "pb_landing";

// =============================================================================
// App: the shared shape, with the fixture loaded
// =============================================================================

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("page builder components app init thread panicked")
    })
}

/// An app with the fixture loaded at startup, which is then disabled again so
/// no app built later loads it, and whose content type is removed when the
/// binary is done.
async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = PgPool::connect(&database_url).await.expect("connect");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    sqlx::query("UPDATE plugin_status SET status = 1 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&pool)
        .await
        .expect("enable the fixture");
    let app = TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await;
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&pool)
        .await
        .expect("disable the fixture");
    pool.close().await;
    app.cleanup_content_type_on_exit(TYPE);
    app
}

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn fixture_path(name: &str) -> std::path::PathBuf {
    common::project_root()
        .join("crates/kernel/tests/fixtures/page_builder")
        .join(name)
}

/// The page both the save and the snapshot use: one of each plugin component
/// and one standard component, in a zone, so a plugin component and a
/// standard one render side by side.
fn landing_page() -> Value {
    json!({
        "root": { "title": "Landing" },
        "content": [
            { "type": "FeatureBanner", "props": {
                "id": "FeatureBanner-1",
                "tone": "dark",
                "imagePosition": "right",
                "image": { "src": "/files/banner.png", "alt": "The editor with three components", "decorative": false },
                "title": "Compose pages from your own components",
                "body": "Declared in a **file**, rendered by your template.\n\n<script>alert(1)</script>",
                "cta": { "url": "/docs/page-builder", "label": "Read the docs", "new_tab": true },
                "style": "color: red"
            } },
            { "type": "PromoCards", "props": {
                "id": "PromoCards-1",
                "heading": "Why teams choose it",
                "headingLevel": 2,
                "columns": "two",
                "cards": [
                    { "eyebrow": "Fast", "title": "Ships in days", "text": "No developer needed.",
                      "link": { "url": "/fast", "label": "How", "new_tab": false } },
                    { "eyebrow": "Safe", "title": "Checked on save", "text": "Every field, on the server." }
                ]
            } },
            { "type": "Columns", "props": { "id": "Columns-1", "layout": "1/2+1/2", "gapSize": "large" },
              "zones": {
                "zone-0": [ { "type": "TextBlock", "props": { "id": "TextBlock-1", "content": "Standard **text**." } } ],
                "zone-1": []
              } }
        ]
    })
}

async fn create(page: Value) -> anyhow::Result<Uuid> {
    let app = app();
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: TYPE.to_string(),
                title: format!("Landing {}", Uuid::now_v7()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(json!({ "field_layout": page })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("A13 page builder test".to_string()),
            },
            &admin(),
        )
        .await?;
    let pool = app.db.clone();
    let id = item.id;
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    });
    Ok(item.id)
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

// =============================================================================
// Tests
// =============================================================================

#[test]
fn the_registry_lists_the_plugin_components() {
    run_test(async {
        let app = app();
        let response = app
            .request(
                Request::get("/api/v1/page-builder/components")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_string(response).await).unwrap();
        let data = body["data"].as_array().unwrap();
        let find = |name: &str| {
            data.iter()
                .find(|c| c["type_name"] == name)
                .unwrap_or_else(|| panic!("{name} is not in the registry: {body}"))
                .clone()
        };

        // The standard set is still there, first, from the kernel.
        assert_eq!(data[0]["type_name"], "Columns");
        assert_eq!(find("Hero")["source"], json!({ "kind": "kernel" }));

        let cards = find("PromoCards");
        assert_eq!(cards["source"], json!({ "kind": "plugin", "name": PLUGIN }));
        assert_eq!(cards["category"], "marketing");
        assert_eq!(cards["template"], "pb/promo-cards.html");
        assert_eq!(
            cards["stylesheets"],
            json!([format!("/static/plugins/{PLUGIN}/promo-cards.css")])
        );
        let list = cards["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "cards")
            .unwrap();
        assert_eq!(list["type"], "list");
        assert_eq!(list["max_items"], 6);
        let sub: Vec<&str> = list["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(sub, vec!["eyebrow", "title", "text", "link"]);

        let banner = find("FeatureBanner");
        let tone = banner["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "tone")
            .unwrap();
        assert_eq!(tone["type"], "select");
        let values: Vec<&str> = tone["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, vec!["light", "dark", "accent"]);
        assert!(
            banner["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["name"] == "image" && f["type"] == "image")
        );
    });
}

#[test]
fn a_page_using_them_saves_and_renders_the_snapshot() {
    run_test(async {
        let app = app();
        let id = create(landing_page()).await.expect("a valid page saves");

        // The rendered components, against the committed snapshot. Regenerate
        // with TROVATO_UPDATE_FIXTURES=1 after a deliberate template change.
        let rendered = app
            .state
            .theme()
            .render_page_builder_content(&landing_page(), &ResolvedReferences::default())
            .expect("render");
        let snapshot = fixture_path("plugin-components.html");
        if std::env::var_os("TROVATO_UPDATE_FIXTURES").is_some() {
            std::fs::write(&snapshot, &rendered.html).unwrap();
        }
        let expected = std::fs::read_to_string(&snapshot).unwrap_or_default();
        assert_eq!(
            rendered.html,
            expected,
            "rendered HTML differs from {}",
            snapshot.display()
        );

        // What the snapshot holds, stated: the plugin templates ran with the
        // declared variant classes, Markdown is sanitized, no prop reached a
        // style attribute, and a link that opens a new tab carries rel.
        assert!(rendered.html.contains(
            r#"class="feature-banner feature-banner--dark feature-banner--image-right""#
        ));
        assert!(
            rendered
                .html
                .contains(r#"class="promo-cards promo-cards--two""#)
        );
        assert!(rendered.html.contains("pb-columns--gap-large"));
        assert!(rendered.html.contains("<strong>file</strong>"));
        assert!(!rendered.html.contains("<script"));
        assert!(!rendered.html.contains("style="));
        assert!(
            rendered
                .html
                .contains(r#"target="_blank" rel="noopener noreferrer""#)
        );
        assert_eq!(
            rendered.stylesheets,
            vec![
                "/static/css/page-builder.css".to_string(),
                format!("/static/plugins/{PLUGIN}/feature-banner.css"),
                format!("/static/plugins/{PLUGIN}/promo-cards.css"),
            ]
        );

        // The item page carries the same HTML and links each stylesheet once.
        let cookies = app
            .create_and_login_admin(
                &common::username("pbc-admin"),
                "password123",
                &format!("{}@example.com", common::username("pbc")),
            )
            .await;
        let response = app
            .request_with_cookies(
                Request::get(format!("/item/{id}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = body_string(response).await;
        assert!(
            page.contains(&rendered.html),
            "the item page renders the snapshot"
        );
        for sheet in &rendered.stylesheets {
            let escaped = sheet.replace('/', "&#x2F;");
            assert_eq!(
                page.matches(&format!(r#"<link rel="stylesheet" href="{escaped}">"#))
                    .count(),
                1,
                "{sheet} linked once in the head"
            );
        }

        // And the plugin's stylesheet is served from its static directory.
        let css = app
            .request(
                Request::get(format!("/static/plugins/{PLUGIN}/promo-cards.css"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(css.status(), StatusCode::OK);
        assert_eq!(css.headers()[header::CONTENT_TYPE], "text/css");
        assert!(body_string(css).await.contains(".promo-cards"));
        let missing = app
            .request(
                Request::get(format!("/static/plugins/{PLUGIN}/../{PLUGIN}.info.toml"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    });
}

#[test]
fn an_invalid_page_is_refused_with_an_error_at_each_field() {
    run_test(async {
        let page = json!({
            "root": {},
            "content": [
                { "type": "PromoCards", "props": {
                    "heading": "x".repeat(81),
                    "cards": [ { "eyebrow": "Too long an eyebrow for its thirty limit", "text": "t" } ]
                } },
                { "type": "FeatureBanner", "props": {
                    "title": "Banner",
                    "tone": "neon",
                    "image": { "src": "/files/banner.png", "alt": "" }
                } }
            ]
        });
        let err = create(page.clone())
            .await
            .expect_err("an invalid page is refused");
        let invalid = err
            .downcast_ref::<PageBuilderInvalid>()
            .unwrap_or_else(|| panic!("not a field level refusal: {err:#}"));
        let found: Vec<(String, String, String, String)> = invalid
            .0
            .iter()
            .map(|(field, e)| {
                (
                    field.clone(),
                    e.location.clone(),
                    e.field.clone(),
                    e.message.clone(),
                )
            })
            .collect();
        let has = |location: &str, field: &str, needle: &str| {
            assert!(
                found
                    .iter()
                    .any(|(item_field, l, f, m)| item_field == "field_layout"
                        && l == location
                        && f == field
                        && m.contains(needle)),
                "no error at {location} {field} containing {needle:?} in {found:#?}"
            );
        };
        has("content[0]", "heading", "at most 80 characters");
        has("content[0]", "cards[0].eyebrow", "at most 30 characters");
        has("content[0]", "cards[0].title", "is required");
        has(
            "content[1]",
            "tone",
            "'neon' is not one of: light, dark, accent",
        );
        has("content[1]", "image.alt", "alt text");
        assert_eq!(found.len(), 5, "{found:#?}");

        // Over HTTP the same refusal is a 422 naming each field.
        let app = app();
        let user = common::username("pbc-json");
        let cookies = app
            .create_and_login_admin(&user, "password123", &format!("{user}@example.com"))
            .await;
        let form = app
            .request_with_cookies(
                Request::get(format!("/item/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(form.status(), StatusCode::OK);
        let html = body_string(form).await;
        let marker = r#"name="_token" value=""#;
        let start = html.find(marker).expect("the form carries a CSRF token") + marker.len();
        let csrf = &html[start..start + html[start..].find('"').unwrap()];
        let response = app
            .request_with_cookies(
                Request::post(format!("/item/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", csrf)
                    .body(Body::from(
                        json!({ "title": "Refused", "fields": { "field_layout": page } })
                            .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = body_string(response).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        for path in [
            "field_layout.content[0].heading",
            "field_layout.content[0].cards[0].title",
            "field_layout.content[1].tone",
            "field_layout.content[1].image.alt",
        ] {
            assert!(body.contains(path), "{path} not named in {body}");
        }
    });
}

/// The editor previews a plugin component through the kernel while the item
/// form is open. CSRF tokens are single use, so a preview that took the
/// page's token would leave the form posting a spent one: previews must not
/// spend it, and the save after them must succeed.
#[test]
fn previewing_does_not_spend_the_forms_csrf_token() {
    run_test(async {
        let app = app();
        let user = common::username("pbc-preview");
        let cookies = app
            .create_and_login_admin(&user, "password123", &format!("{user}@example.com"))
            .await;
        let form = app
            .request_with_cookies(
                Request::get(format!("/item/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(form.status(), StatusCode::OK);
        let html = body_string(form).await;
        let marker = r#"name="_token" value=""#;
        let start = html.find(marker).expect("the form carries a CSRF token") + marker.len();
        let csrf = html[start..start + html[start..].find('"').unwrap()].to_string();

        let page = landing_page();
        for component in page["content"].as_array().unwrap() {
            let response = app
                .request_with_cookies(
                    Request::post("/api/v1/page-builder/preview")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(json!({ "component": component }).to_string()))
                        .unwrap(),
                    &cookies,
                )
                .await;
            let status = response.status();
            let body = body_string(response).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let preview: Value = serde_json::from_str(&body).unwrap();
            assert!(!preview["html"].as_str().unwrap_or("").is_empty(), "{body}");
            assert_eq!(preview["errors"], json!([]), "{body}");
        }

        // A body that is not JSON (what a cross-site form could send) is refused.
        let response = app
            .request_with_cookies(
                Request::post("/api/v1/page-builder/preview")
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(
                        json!({ "component": page["content"][0] }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let response = app
            .request_with_cookies(
                Request::post(format!("/item/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &csrf)
                    .body(Body::from(
                        json!({ "title": "Previewed", "fields": { "field_layout": page } })
                            .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = body_string(response).await;
        assert!(
            status.is_success() || status.is_redirection(),
            "the save after the previews was refused: {status} {body}"
        );
    });
}

// =============================================================================
// Page root fields (A20)
// =============================================================================

/// The sitewide footer call to action the test plugin's tile template renders
/// unless a page says otherwise.
const FOOTER_TILE: &str = "pb_footer_cta";
const FOOTER_DEFAULT: &str = "Talk to our team about your next project.";

fn page_with_root(root: Value) -> Value {
    let mut page = landing_page();
    page["root"] = root;
    page
}

#[test]
fn the_registry_serves_the_page_root_fields() {
    run_test(async {
        let app = app();
        let response = app
            .request(
                Request::get("/api/v1/page-builder/components")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_string(response).await).unwrap();
        let root = body["root_fields"].as_array().unwrap_or_else(|| {
            panic!("no root_fields in {body}");
        });
        let names: Vec<&str> = root.iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["footer_cta_mode", "footer_cta_content"]);

        let mode = &root[0];
        assert_eq!(mode["type"], "select");
        assert_eq!(mode["default"], "default");
        assert_eq!(mode["label"], "Footer call to action");
        assert_eq!(mode["source"], json!({ "kind": "plugin", "name": PLUGIN }));
        let values: Vec<&str> = mode["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, vec!["default", "hide", "custom"]);

        let content = &root[1];
        assert_eq!(content["type"], "rich_text");
        assert_eq!(content["max_length"], 280);
    });
}

#[test]
fn each_footer_mode_saves_and_renders_its_footer() {
    run_test(async {
        let app = app();
        let no_choice = create(page_with_root(json!({ "title": "No choice" })))
            .await
            .expect("a page without root values saves");
        let default = create(page_with_root(
            json!({ "title": "Default", "footer_cta_mode": "default" }),
        ))
        .await
        .expect("default saves");
        let hidden = create(page_with_root(
            json!({ "title": "Hidden", "footer_cta_mode": "hide" }),
        ))
        .await
        .expect("hide saves");
        // Current Puck keeps root values under `root.props`; that shape saves
        // and renders the same.
        let custom = create(page_with_root(json!({ "props": {
            "title": "Custom",
            "footer_cta_mode": "custom",
            "footer_cta_content": "Book a **free** call.\n\n<script>alert(1)</script>"
        } })))
        .await
        .expect("custom saves");

        // The sitewide tile, shown on these pages and one page with no page
        // builder content at all.
        sqlx::query("DELETE FROM tile WHERE machine_name = $1")
            .bind(FOOTER_TILE)
            .execute(&app.db)
            .await
            .expect("clear a tile a failed run left");
        let paths: Vec<String> = [no_choice, default, hidden, custom]
            .iter()
            .map(|id| format!("/item/{id}"))
            .chain(std::iter::once("/user/login".to_string()))
            .collect();
        let tile = app
            .state
            .tiles()
            .create(
                serde_json::from_value(json!({
                    "machine_name": FOOTER_TILE,
                    "label": "Work with us",
                    "region": "footer",
                    "tile_type": "custom_html",
                    "config": { "body": format!("<p>{FOOTER_DEFAULT}</p>"), "format": "filtered_html" },
                    "visibility": { "paths": paths },
                }))
                .unwrap(),
            )
            .await
            .expect("create the footer tile");
        let pool = app.db.clone();
        let tile_id = tile.id;
        common::defer_cleanup(move || async move {
            sqlx::query("DELETE FROM tile WHERE id = $1")
                .bind(tile_id)
                .execute(&pool)
                .await
                .ok();
        });

        let user = common::username("pbc-root");
        let cookies = app
            .create_and_login_admin(&user, "password123", &format!("{user}@example.com"))
            .await;
        let view = |path: String| {
            let cookies = cookies.clone();
            async move {
                let response = app
                    .request_with_cookies(
                        Request::get(&path).body(Body::empty()).unwrap(),
                        &cookies,
                    )
                    .await;
                assert_eq!(response.status(), StatusCode::OK, "{path}");
                body_string(response).await
            }
        };

        for id in [no_choice, default] {
            let page = view(format!("/item/{id}")).await;
            assert!(
                page.contains(&format!(
                    r#"<div class="footer-cta footer-cta--default"><p>{FOOTER_DEFAULT}</p></div>"#
                )),
                "the default footer on {id}: {page}"
            );
            assert!(page.contains("tile--pb_footer_cta"), "{page}");
            assert!(!page.contains("footer-cta--custom"), "{page}");
        }

        let page = view(format!("/item/{hidden}")).await;
        assert!(!page.contains("footer-cta"), "hidden: {page}");
        assert!(
            !page.contains("tile--pb_footer_cta"),
            "a hidden call to action leaves no empty tile: {page}"
        );
        assert!(!page.contains(FOOTER_DEFAULT), "{page}");

        let page = view(format!("/item/{custom}")).await;
        assert!(
            page.contains(
                r#"<div class="footer-cta footer-cta--custom"><p>Book a <strong>free</strong> call.</p>"#
            ),
            "custom: {page}"
        );
        assert!(!page.contains(FOOTER_DEFAULT), "{page}");
        assert!(!page.contains("<script>alert"), "{page}");

        // A page with no page builder field gets the declared default.
        let page = view("/user/login".to_string()).await;
        assert!(page.contains("footer-cta--default"), "login: {page}");
    });
}

#[test]
fn an_invalid_root_value_is_refused_at_its_field() {
    run_test(async {
        for (root, field, needle) in [
            (
                json!({ "title": "x", "footer_cta_mode": "blink" }),
                "footer_cta_mode",
                "'blink' is not one of: default, hide, custom",
            ),
            (
                json!({ "props": { "footer_cta_mode": "custom", "footer_cta_content": "x".repeat(281) } }),
                "footer_cta_content",
                "at most 280 characters",
            ),
            (
                json!({ "title": "x", "footer_colour": "red" }),
                "footer_colour",
                "not a page root field any enabled plugin declares",
            ),
        ] {
            let err = create(page_with_root(root.clone()))
                .await
                .expect_err("an invalid root is refused");
            let invalid = err
                .downcast_ref::<PageBuilderInvalid>()
                .unwrap_or_else(|| panic!("not a field level refusal: {err:#}"));
            assert_eq!(invalid.0.len(), 1, "{root}: {:#?}", invalid.0);
            let (item_field, error) = &invalid.0[0];
            assert_eq!(item_field, "field_layout");
            assert_eq!(error.location, "root");
            assert_eq!(error.field, field);
            assert!(error.message.contains(needle), "{error:?}");
        }

        // Over HTTP the refusal is a 422 at `field_layout.root.<field>`.
        let app = app();
        let user = common::username("pbc-root-json");
        let cookies = app
            .create_and_login_admin(&user, "password123", &format!("{user}@example.com"))
            .await;
        let form = app
            .request_with_cookies(
                Request::get(format!("/item/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = body_string(form).await;
        let marker = r#"name="_token" value=""#;
        let start = html.find(marker).expect("the form carries a CSRF token") + marker.len();
        let csrf = &html[start..start + html[start..].find('"').unwrap()];
        let page = page_with_root(json!({ "footer_cta_mode": "blink" }));
        let response = app
            .request_with_cookies(
                Request::post(format!("/item/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", csrf)
                    .body(Body::from(
                        json!({ "title": "Refused", "fields": { "field_layout": page } })
                            .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = body_string(response).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("field_layout.root.footer_cta_mode"), "{body}");
        assert!(body.contains("blink"), "{body}");
    });
}

/// A page saved before page root fields existed, with the plugin that
/// declares them enabled: it validates, saves, and renders byte for byte the
/// HTML it rendered then, and its templates get the declared defaults.
#[test]
fn a_page_saved_before_root_fields_validates_and_renders_the_same_html() {
    run_test(async {
        let app = app();
        let page: Value = serde_json::from_str(
            &std::fs::read_to_string(fixture_path("standard-page-v0.106.json")).unwrap(),
        )
        .unwrap();
        let errors = app.state.page_builder_components().validate_page(&page);
        assert!(errors.is_empty(), "{errors:?}");
        let rendered = app
            .state
            .theme()
            .render_page_builder_content(&page, &ResolvedReferences::default())
            .expect("render");
        let expected = std::fs::read_to_string(fixture_path("standard-page-v0.106.html")).unwrap();
        assert_eq!(rendered.html, expected);
        assert_eq!(
            Value::Object(rendered.root),
            json!({
                "title": "A page saved by v0.106.0",
                "footer_cta_mode": "default",
                "variant_classes": ""
            })
        );
        create(page).await.expect("the old page saves");
    });
}
