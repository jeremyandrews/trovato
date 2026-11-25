#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Theme switching (B108): the theme registry, `/admin/appearance`, the
//! template loader applying the chosen theme without a restart, and the
//! fallback to the stock theme for a template a theme does not provide.
//!
//! The fixture theme is `tests/fixtures/themes/lakeside`: a `theme.toml`, a
//! `base.html` that adds its stylesheet, and the stylesheet. Every other
//! template comes from the stock theme.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::theme::registry::{self, ADMIN_THEME_KEY, DEFAULT_THEME_KEY};
use trovato_kernel::theme::{ActiveThemes, ThemeEngine, scope_request};

/// The fixture theme's stylesheet link: present exactly when a page is drawn
/// by the Lakeside theme.
const LAKESIDE_CSS: &str = "/static/themes/lakeside/css/lakeside.css";

fn themes_dir() -> std::path::PathBuf {
    common::project_root().join("crates/kernel/tests/fixtures/themes")
}

/// A second theme root holding `cracked`, whose `base.html` does not parse
/// (B138).
fn broken_themes_dir() -> std::path::PathBuf {
    common::project_root().join("crates/kernel/tests/fixtures/broken_themes")
}

/// Theme settings are site-wide, so the tests that change them take turns.
static THEMES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

/// An app whose `THEMES_DIR` holds the fixture theme.
fn themed_app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || {
            handle.block_on(async {
                let root = common::project_root();
                TestApp::with_config(|config| {
                    config.templates_dirs = vec![root.join("templates")];
                    config.themes_dirs = vec![themes_dir(), broken_themes_dir()];
                })
                .await
            })
        })
        .join()
        .expect("themed app init thread panicked")
    })
}

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

/// Put the stock theme back when the test ends.
fn restore_stock_on_exit(app: &'static TestApp) {
    common::defer_cleanup(move || async move {
        for key in [DEFAULT_THEME_KEY, ADMIN_THEME_KEY] {
            let _ = SiteConfig::delete(&app.db, key).await;
        }
        app.state.theme().set_active(ActiveThemes::default());
    });
}

/// The engine, without a server: a page the theme provides a template for
/// is drawn by it, the admin path by the admin theme, a template the theme
/// lacks by the stock theme, and an unknown theme name is the stock theme.
#[test]
fn the_engine_renders_the_active_theme_and_falls_back_to_stock() {
    run_test(async {
        let root = common::project_root();
        let stock_dirs = vec![root.join("templates")];
        let themes = registry::discover(&[themes_dir()]);
        assert_eq!(
            themes
                .iter()
                .map(|t| t.machine_name.as_str())
                .collect::<Vec<_>>(),
            vec!["stock", "lakeside"]
        );
        let lakeside = &themes[1];
        assert_eq!(lakeside.name, "Lakeside");
        let engine = ThemeEngine::new(&stock_dirs, None, Default::default())
            .unwrap()
            .with_themes(themes.clone(), &stock_dirs, None);

        let render = |name: &'static str| {
            let mut context = tera::Context::new();
            context.insert("title", "Probe");
            context.insert("content", "<p>probe</p>");
            engine.tera().render(name, &context)
        };

        // Stock until a theme is chosen.
        let html = scope_request("/", async { render("base.html") })
            .await
            .unwrap();
        assert!(!html.contains(LAKESIDE_CSS));

        engine.set_active(ActiveThemes {
            default: "lakeside".into(),
            admin: "stock".into(),
        });
        let html = scope_request("/", async { render("base.html") })
            .await
            .unwrap();
        assert!(
            html.contains(LAKESIDE_CSS),
            "the default theme draws a front page"
        );
        let html = scope_request("/admin/content", async { render("base.html") })
            .await
            .unwrap();
        assert!(!html.contains(LAKESIDE_CSS), "the admin theme draws /admin");

        // Lakeside has no page.html or maintenance.html: the stock ones are
        // used, and they extend Lakeside's base.html.
        assert!(
            !themes_dir()
                .join("lakeside/templates/maintenance.html")
                .exists()
        );
        let mut context = tera::Context::new();
        context.insert("site_name", "Club");
        context.insert("maintenance_message", "Back soon");
        let html = scope_request("/", async {
            engine.tera().render("maintenance.html", &context)
        })
        .await
        .unwrap();
        assert!(html.contains("Back soon"), "the stock template renders");
        assert!(html.contains(LAKESIDE_CSS), "inside the theme's base");

        // `render_page` (plugin pages, error pages) uses the active theme too.
        let mut context = tera::Context::new();
        let html = scope_request("/blog", async {
            engine.render_page("/blog", "Blog", "<p>posts</p>", &mut context)
        })
        .await
        .unwrap();
        assert!(html.contains("<p>posts</p>"));
        assert!(
            html.contains(LAKESIDE_CSS),
            "render_page draws in the theme"
        );

        // An unknown theme is the stock theme.
        engine.set_active(ActiveThemes {
            default: "no_such_theme".into(),
            admin: "stock".into(),
        });
        assert_eq!(engine.active().default, "stock");
        let html = scope_request("/", async { render("base.html") })
            .await
            .unwrap();
        assert!(!html.contains(LAKESIDE_CSS));
    });
}

/// Every route refuses an anonymous visitor and a signed-in user without
/// `administer themes`, GET and POST.
#[test]
fn the_appearance_screen_refuses_anonymous_and_plain_users() {
    run_test(async {
        let app = shared_app().await;
        let (_, plain) =
            user_holding(app, "b108aplain", &["access content", "administer site"]).await;
        for (who, cookies) in [
            ("an anonymous visitor", ""),
            ("a plain user", plain.as_str()),
        ] {
            for uri in ["/admin/appearance", "/admin/appearance/stock/screenshot"] {
                let (status, _) = get(app, cookies, uri).await;
                assert!(
                    status == StatusCode::FORBIDDEN || status.is_redirection(),
                    "GET {uri} for {who}: {status}"
                );
            }
            let response = post_form(
                app,
                cookies,
                "/admin/appearance",
                "default_theme=stock&admin_theme=stock".into(),
            )
            .await;
            let status = response.status();
            assert!(
                status == StatusCode::FORBIDDEN || status.is_redirection(),
                "POST for {who}: {status}"
            );
        }
        let (status, _) = get(app, &plain, "/admin/appearance").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    });
}

/// **The finding.** A user holding `administer themes` lists the themes,
/// makes Lakeside the default, and the next front page request is drawn by
/// it, with no restart, while `/admin` keeps the stock theme; then makes it
/// the admin theme too, and back.
#[test]
fn switching_the_default_theme_applies_without_a_restart() {
    run_test(async {
        let app = themed_app();
        common::hold_until_cleanup(THEMES.lock().await);
        restore_stock_on_exit(app);
        let (_, cookies) = user_holding(app, "b108atheme", &["administer themes"]).await;

        let (status, html) = get(app, &cookies, "/admin/appearance").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("Lakeside") && html.contains("Trovato"),
            "both themes are listed"
        );
        assert!(
            html.contains("/admin/appearance/lakeside/screenshot"),
            "with its screenshot"
        );

        // Without the token nothing changes.
        let response = post_form(
            app,
            &cookies,
            "/admin/appearance",
            "default_theme=lakeside&admin_theme=stock".into(),
        )
        .await;
        assert!(!response.status().is_redirection());
        assert_eq!(app.state.theme().active().default, "stock");

        // An unknown theme is refused.
        let pairs = form_pairs(&html, "/admin/appearance");
        let response = post_form(
            app,
            &cookies,
            "/admin/appearance",
            format!("{pairs}&default_theme=nope&admin_theme=stock"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            body_string(response)
                .await
                .contains("There is no theme named nope")
        );

        let (_, html) = get(app, &cookies, "/admin/appearance").await;
        let pairs = form_pairs(&html, "/admin/appearance");
        let response = post_form(
            app,
            &cookies,
            "/admin/appearance",
            format!("{pairs}&default_theme=lakeside&admin_theme=stock"),
        )
        .await;
        assert!(
            response.status().is_redirection(),
            "saved: {}",
            response.status()
        );
        let stored = SiteConfig::get(&app.db, DEFAULT_THEME_KEY).await.unwrap();
        assert_eq!(
            stored,
            Some(serde_json::json!("lakeside")),
            "stored in site_config"
        );

        let (status, front) = get(app, "", "/user/login").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            front.contains(LAKESIDE_CSS),
            "a public page is drawn by Lakeside"
        );
        let (_, admin) = get(app, &cookies, "/admin/appearance").await;
        assert!(
            !admin.contains(LAKESIDE_CSS),
            "the admin pages keep the stock theme"
        );
        assert!(admin.contains("Default theme"));

        // The theme's own files are served.
        let (status, css) = get(app, "", LAKESIDE_CSS).await;
        assert_eq!(status, StatusCode::OK);
        assert!(css.contains(".theme-lakeside"));
        let (status, _) = get(app, "", "/static/themes/lakeside/../theme.toml").await;
        assert_ne!(status, StatusCode::OK, "no traversal out of static/");
        let (status, _) = get(app, "", "/static/themes/lakeside/theme.toml").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "only static/ is served");

        // Admin theme too.
        let pairs = form_pairs(&admin, "/admin/appearance");
        let response = post_form(
            app,
            &cookies,
            "/admin/appearance",
            format!("{pairs}&default_theme=lakeside&admin_theme=lakeside"),
        )
        .await;
        assert!(response.status().is_redirection());
        let (_, admin) = get(app, &cookies, "/admin/appearance").await;
        assert!(
            admin.contains(LAKESIDE_CSS),
            "now /admin is drawn by Lakeside"
        );

        // And back to stock.
        let pairs = form_pairs(&admin, "/admin/appearance");
        let response = post_form(
            app,
            &cookies,
            "/admin/appearance",
            format!("{pairs}&default_theme=stock&admin_theme=stock"),
        )
        .await;
        assert!(response.status().is_redirection());
        let (_, front) = get(app, "", "/user/login").await;
        assert!(!front.contains(LAKESIDE_CSS), "stock again");
    });
}

/// A stored theme is applied when a server starts, and a stored name that is
/// no longer installed falls back to the stock theme.
#[test]
fn the_stored_theme_is_read_at_start_and_a_missing_one_is_stock() {
    run_test(async {
        let app = themed_app();
        common::hold_until_cleanup(THEMES.lock().await);
        restore_stock_on_exit(app);
        SiteConfig::set(&app.db, DEFAULT_THEME_KEY, serde_json::json!("lakeside"))
            .await
            .unwrap();
        SiteConfig::set(&app.db, ADMIN_THEME_KEY, serde_json::json!("removed_theme"))
            .await
            .unwrap();
        registry::load_active(&app.db, app.state.theme()).await;
        let active = app.state.theme().active();
        assert_eq!(active.default, "lakeside");
        assert_eq!(
            active.admin, "stock",
            "an uninstalled theme is the stock one"
        );
    });
}

/// B138: a theme whose templates do not parse is refused when the server
/// starts. The appearance screen says so and why, choosing it is refused with
/// the reason, and the theme that was active stays active.
#[test]
fn a_theme_that_does_not_parse_cannot_be_activated() {
    run_test(async {
        let app = themed_app();
        common::hold_until_cleanup(THEMES.lock().await);
        restore_stock_on_exit(app);
        let (_, cookies) = user_holding(app, "b138cracked", &["administer themes"]).await;

        // A theme switch made by an earlier test in this binary bumps the
        // themes generation from a background task, and this server's watcher
        // answers it by reloading the stored choice, which cleanup has deleted,
        // so stock. Let that land before setting Lakeside below, or it can
        // arrive in the middle of this test and replace it (seen in CI: "the
        // previous theme stays active", left "stock", right "lakeside").
        common::settle_cache_generations().await;

        // Lakeside is the default before the attempt, so "the previous theme
        // stays active" is a real theme and not just the fallback.
        app.state.theme().set_active(ActiveThemes {
            default: "lakeside".into(),
            admin: "stock".into(),
        });

        let (status, html) = get(app, &cookies, "/admin/appearance").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Cracked"), "the broken theme is listed");
        assert!(
            html.contains("cannot be activated"),
            "and marked as unusable"
        );
        assert!(
            html.contains("templates/base.html"),
            "naming the file that does not parse"
        );

        let pairs = form_pairs(&html, "/admin/appearance");
        let response = post_form(
            app,
            &cookies,
            "/admin/appearance",
            format!("{pairs}&default_theme=cracked&admin_theme=stock"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "refused, not saved");
        let body = body_string(response).await;
        assert!(
            body.contains("The theme Cracked cannot be activated"),
            "the page says it cannot be activated"
        );
        assert!(
            body.contains("templates/base.html"),
            "and why: the file that does not parse"
        );
        assert_eq!(
            app.state.theme().active().default,
            "lakeside",
            "the previous theme stays active"
        );
        assert_eq!(
            SiteConfig::get(&app.db, DEFAULT_THEME_KEY).await.unwrap(),
            None,
            "nothing is stored"
        );

        // The site still renders, on the theme that was active.
        let (status, front) = get(app, "", "/user/login").await;
        assert_eq!(status, StatusCode::OK);
        assert!(front.contains(LAKESIDE_CSS));
        assert!(!front.contains("theme-cracked"));
    });
}
