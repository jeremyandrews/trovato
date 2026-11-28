#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every `<form>` the kernel and the in-tree plugins serve is built by
//! `FormService::build` and drawn by the one form renderer (B83c).
//!
//! The renderer marks each form it draws with `data-form-pipeline`
//! (`templates/form/form.html`, `templates/admin/form/form.html`). This test
//! opens every page it can reach and fails on any `<form>` without the mark:
//! a form written by hand in a template, or built in Rust as HTML, comes from
//! outside the pipeline, where no plugin can alter it, its token and saved
//! state are not checked together, and its errors are shown some other way.
//!
//! The pages: every GET route in `docs/admin/inventory.md` without a
//! parameter, every admin page reached by following links from those (one
//! page per address shape, so `/admin/content/{id}/edit` once), and the
//! front end's pages, signed in and not.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::{BTreeSet, VecDeque};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};

/// GET `uri`: its status, content type and body.
async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b83cmarker"))
                .header("accept", "text/html")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The GET route patterns in docs/admin/inventory.md, kernel and plugin.
fn inventory_get_routes() -> Vec<String> {
    let doc = std::fs::read_to_string(common::project_root().join("docs/admin/inventory.md"))
        .expect("docs/admin/inventory.md");
    doc.lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| {
            let mut cells = l.split('|');
            let path = cells.next()?.trim().trim_end_matches('`').to_string();
            let methods = cells.next()?.trim().to_string();
            methods.contains("GET").then_some(path)
        })
        .collect()
}

/// The route a path is an instance of (`/admin/config/formats/full_html/edit`
/// is `/admin/config/formats/{format}/edit`), so each page type is opened
/// once; a literal segment beats a parameter. A path no listed route matches
/// is its own type, with ids and numbers folded.
fn shape(patterns: &[Vec<String>], path: &str) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let mut best: Option<(usize, String)> = None;
    for pattern in patterns {
        if pattern.len() != segments.len() {
            continue;
        }
        let mut literal = 0;
        let matches = pattern.iter().zip(&segments).all(|(p, s)| {
            if p.starts_with('{') {
                true
            } else {
                literal += 1;
                p == s
            }
        });
        if matches && best.as_ref().is_none_or(|(n, _)| literal > *n) {
            best = Some((literal, pattern.join("/")));
        }
    }
    if let Some((_, pattern)) = best {
        return pattern;
    }
    path.split('/')
        .map(|seg| {
            let uuidish = seg.len() >= 32 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
            let number = !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit());
            if uuidish || number { "{id}" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Remove `<script>` blocks and comments: a `<form` inside them is not a
/// form on the page.
fn drop_blocks(html: &str) -> String {
    let mut out = html.to_string();
    for (open, close) in [("<script", "</script>"), ("<!--", "-->")] {
        while let Some(start) = out.find(open) {
            let Some(len) = out[start..].find(close) else {
                break;
            };
            out.replace_range(start..start + len + close.len(), " ");
        }
    }
    out
}

/// Pages that are not HTML pages, or that act when opened.
fn skip(path: &str) -> bool {
    path.ends_with("/screenshot")
        || path == "/admin/stage/current"
        || path.starts_with("/admin/embed/")
        || path.ends_with(".json")
        || path.ends_with(".xml")
        || path.contains("/export")
        || path.contains("/suggest")
        || path.starts_with("/user/logout")
        || path.starts_with("/static/")
        || path.starts_with("/files/")
}

/// The same-site links on a page, query and fragment dropped. With `admin`,
/// only links into the admin.
fn links(html: &str, admin: bool) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in html.match_indices("href=\"") {
        let from = at + 6;
        let Some(len) = html[from..].find('"') else {
            continue;
        };
        let href = html[from..from + len].replace("&#x2F;", "/");
        let href = href.split(['?', '#']).next().unwrap_or("").to_string();
        if !href.starts_with('/') || href.starts_with("//") {
            continue;
        }
        let is_admin = href == "/admin" || href.starts_with("/admin/");
        if is_admin == admin {
            out.push(href);
        }
    }
    out
}

/// Every `<form …>` opening tag on `html` that the renderer did not mark.
fn unmarked_forms(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<form") {
        let after = &rest[at + 5..];
        // `<formfoo` is not a form; `<form>` and `<form ` are.
        if after.starts_with(['>', ' ', '\n', '\t']) {
            let end = after.find('>').map_or(after.len(), |e| e + 1);
            let tag = &rest[at..at + 5 + end];
            if !tag.contains("data-form-pipeline") {
                out.push(tag.chars().take(200).collect());
            }
        }
        rest = after;
    }
    out
}

/// Crawl from `start` as the holder of `cookies`, following `admin` links
/// (every admin page reachable; on the front end, one step from `start`, as
/// its aliased pages have no shape to fold), and return every unmarked form
/// found, by page.
async fn crawl(
    app: &TestApp,
    patterns: &[Vec<String>],
    cookies: &str,
    start: Vec<String>,
    admin: bool,
    seen: &mut BTreeSet<String>,
    pages: &mut usize,
    forms: &mut usize,
) -> Vec<String> {
    let mut queue: VecDeque<(String, usize)> = start.into_iter().map(|p| (p, 0)).collect();
    let mut found = Vec::new();
    while let Some((path, depth)) = queue.pop_front() {
        let bare = path.split('?').next().unwrap_or("").to_string();
        if skip(&bare) || !seen.insert(format!("{cookies}:{}", shape(patterns, &bare))) {
            continue;
        }
        assert!(seen.len() < 1500, "the crawl ran away");
        let (status, content_type, html) = get(app, cookies, &path).await;
        if !content_type.starts_with("text/html") || status != StatusCode::OK {
            continue;
        }
        *pages += 1;
        *forms += html.matches("data-form-pipeline").count();
        for form in unmarked_forms(&drop_blocks(&html)) {
            found.push(format!("{path} ({status}): {form}"));
        }
        if admin || depth == 0 {
            for link in links(&html, admin) {
                queue.push_back((link, depth + 1));
            }
        }
    }
    found
}

#[test]
fn every_form_served_is_built_by_the_form_pipeline() {
    run_test(async {
        let app = shared_app().await;
        for plugin in [
            "trovato_comments",
            "trovato_categories",
            "trovato_content_translation",
            "trovato_media",
            "trovato_audit_log",
            "trovato_redirects",
        ] {
            common::ensure_plugin_tables(app, plugin);
            app.ensure_plugin_enabled(plugin).await;
        }
        app.ensure_conference_items().await;
        let name = common::username("b83cmarker");
        let admin = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;

        let routes = inventory_get_routes();
        assert!(
            routes.len() > 60,
            "docs/admin/inventory.md lists only {} GET routes",
            routes.len()
        );
        let mut admin_start: Vec<String> = routes
            .iter()
            .filter(|p| !p.contains('{') && p.starts_with("/admin"))
            .cloned()
            .collect();
        admin_start.push("/admin/search?q=content".to_string());
        let front_start: Vec<String> = [
            "/",
            "/search",
            "/search?q=conference",
            "/user/login",
            "/user/register",
            "/user/password-reset",
            "/user/recover",
            "/b83c-no-such-page",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain(
            routes
                .iter()
                .filter(|p| !p.contains('{') && !p.starts_with("/admin"))
                .cloned(),
        )
        .collect();

        let patterns: Vec<Vec<String>> = routes
            .iter()
            .map(|p| p.split('/').map(str::to_string).collect())
            .collect();
        let mut seen = BTreeSet::new();
        let mut pages = 0;
        let mut forms = 0;
        let mut found = crawl(
            app,
            &patterns,
            &admin,
            admin_start,
            true,
            &mut seen,
            &mut pages,
            &mut forms,
        )
        .await;
        found.extend(
            crawl(
                app,
                &patterns,
                &admin,
                front_start.clone(),
                false,
                &mut seen,
                &mut pages,
                &mut forms,
            )
            .await,
        );
        found.extend(
            crawl(
                app,
                &patterns,
                "",
                front_start,
                false,
                &mut seen,
                &mut pages,
                &mut forms,
            )
            .await,
        );

        // Signed in as an administrator, the crawl reads about a hundred and ten
        // pages; were the sign-in lost, it would read only the front end's.
        eprintln!("read {pages} pages, met {forms} pipeline forms");
        assert!(pages > 90, "the crawl read only {pages} pages");
        assert!(forms > 200, "the crawl met only {forms} pipeline forms");
        found.sort();
        found.dedup();
        assert!(
            found.is_empty(),
            "{} form(s) were not built by FormService and drawn by the form renderer \
             (no data-form-pipeline):\n{}",
            found.len(),
            found.join("\n")
        );
    });
}

/// The check sees a hand-written form, and passes one the renderer drew.
#[test]
fn the_check_tells_a_hand_written_form_from_a_pipeline_one() {
    assert_eq!(
        unmarked_forms(r#"<form method="post" action="/x"><input></form>"#).len(),
        1
    );
    assert!(
        unmarked_forms(r#"<form id="f" class="form" data-form-pipeline method="get">"#).is_empty()
    );
    assert!(unmarked_forms("<formation>").is_empty());
}
