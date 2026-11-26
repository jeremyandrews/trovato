#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every admin page, rendered in marker mode, has no interface text that did
//! not pass through the translation function.
//!
//! Marker mode (`TROVATO_I18N_MARKERS=1`, see `crate::i18n`) wraps each string
//! `t` returns in one pair of private use characters and each value a template
//! prints in another. Whatever letters are left outside both on a rendered
//! page reached it as a literal in a template, or as English built in Rust
//! and printed as trusted HTML: text no translation can reach. This test
//! starts at the Dashboard and the group landing pages, follows every admin
//! link it finds (one page per address shape: `/admin/content/{id}/edit` once,
//! not once per item), and reads every text node and every attribute a person
//! hears or sees (`aria-label`, `title`, `placeholder`, `alt`, a button's
//! `value`).
//!
//! Before B199 the admin had no translation function: every page fails.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::i18n;

/// Marker mode is read once per process, so it is set before the shared app
/// exists. This file is its own test binary: no other test sees it.
fn markers_on() {
    trovato_test_utils::env::set_env_default("TROVATO_I18N_MARKERS", "1");
    trovato_test_utils::env::set_env_default("TROVATO_PSEUDO_LOCALES", "1");
    assert!(
        i18n::markers_enabled(),
        "marker mode must be on before anything renders"
    );
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b199i18n"))
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
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The kernel route patterns in docs/admin/inventory.md, as segment lists.
fn route_patterns() -> Vec<Vec<String>> {
    let doc = std::fs::read_to_string(common::project_root().join("docs/admin/inventory.md"))
        .expect("docs/admin/inventory.md");
    doc.lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split('`').next())
        .filter(|p| p.starts_with("/admin"))
        .map(|p| p.split('/').map(str::to_string).collect())
        .collect()
}

/// The route a path is an instance of: `/admin/config/formats/full_html/edit`
/// is `/admin/config/formats/{format}/edit`, so each page type is read once.
/// A literal segment beats a parameter, so `/admin/content/add` is not
/// `/admin/content/{id}`. A path no kernel route matches (a plugin's page)
/// is its own type, with ids and numbers folded.
fn shape(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let mut best: Option<(usize, String)> = None;
    for pattern in route_patterns() {
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
    fold(path)
}

/// `/admin/x/0193…/edit` → `/admin/x/{id}/edit`.
fn fold(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let uuidish = seg.len() >= 32 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
            let number = !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit());
            if uuidish || number { "{id}" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Pages that are not HTML pages, or that act when opened.
fn skip(path: &str) -> bool {
    path.ends_with("/screenshot")
        || path == "/admin/stage/current"
        || path.starts_with("/admin/embed/")
        || path.ends_with(".json")
        || path.contains("/export")
}

/// The same-site admin links on a page, markers removed, query and fragment
/// dropped.
fn admin_links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in html.match_indices("href=\"") {
        let from = at + 6;
        let Some(len) = html[from..].find('"') else {
            continue;
        };
        let href = i18n::strip_markers(&html[from..from + len]).replace("&#x2F;", "/");
        let href = href.split(['?', '#']).next().unwrap_or("").to_string();
        if href == "/admin" || href.starts_with("/admin/") {
            out.push(href);
        }
    }
    out
}

/// Remove `<script>`, `<style>`, `<template>` and comments: their contents are
/// not text on the page. (Strings built in a page's script are the scripts'
/// own problem, out of this test's reach.)
fn drop_blocks(html: &str) -> String {
    let mut out = html.to_string();
    for (open, close) in [
        ("<script", "</script>"),
        ("<style", "</style>"),
        ("<!--", "-->"),
    ] {
        while let Some(start) = out.find(open) {
            let Some(len) = out[start..].find(close) else {
                break;
            };
            out.replace_range(start..start + len + close.len(), " ");
        }
    }
    out
}

/// HTML entities name letters (`&nbsp;`, `&middot;`) without being text.
fn drop_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match tail.find(';') {
            Some(end)
                if end <= 10
                    && tail[1..end]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '#') =>
            {
                out.push(' ');
                rest = &tail[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of `name="…"` in a tag's source.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let at = tag.find(&needle)? + needle.len();
    let len = tag[at..].find('"')?;
    Some(&tag[at..at + len])
}

/// Every piece of text on the page that reached it without `t` or a data
/// print: text nodes, and the attributes a person hears or sees.
fn untranslated(html: &str) -> Vec<String> {
    let body = drop_blocks(html);
    let mut found = Vec::new();
    let mut rest = body.as_str();
    while let Some(lt) = rest.find('<') {
        let text = &rest[..lt];
        if let Some(left) = i18n::untranslated_text(&drop_entities(text)) {
            found.push(left);
        }
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        let tag = &rest[lt..lt + gt + 1];
        for name in [
            "aria-label",
            "title",
            "placeholder",
            "alt",
            "aria-description",
        ] {
            if let Some(v) = attr(tag, name)
                && let Some(left) = i18n::untranslated_text(&drop_entities(v))
            {
                found.push(format!("{name}=\"{left}\""));
            }
        }
        let button = tag.starts_with("<input")
            && matches!(attr(tag, "type"), Some("submit" | "button" | "reset"));
        if button
            && let Some(v) = attr(tag, "value")
            && let Some(left) = i18n::untranslated_text(&drop_entities(v))
        {
            found.push(format!("value=\"{left}\""));
        }
        rest = &rest[lt + gt + 1..];
    }
    found
}

/// The `<title>`, for the failure message.
fn title_of(html: &str) -> String {
    html.split("<title>")
        .nth(1)
        .and_then(|t| t.split("</title>").next())
        .map(i18n::strip_markers)
        .unwrap_or_default()
}

/// Delete the users this test made (every name starting `{prefix}-`): a test
/// run leaves nothing behind. Their roles go with the role tracker, their
/// preferences and role assignments with the account.
async fn forget(app: &TestApp, prefix: &str) {
    sqlx::query("DELETE FROM users WHERE name LIKE $1")
        .bind(format!("{prefix}-%"))
        .execute(&app.db)
        .await
        .expect("delete this test's users");
}

#[test]
fn every_admin_page_passes_all_its_interface_text_through_the_translation_function() {
    markers_on();
    run_test(async {
        let app = shared_app().await;
        for plugin in [
            "trovato_comments",
            "trovato_categories",
            "trovato_content_translation",
            "trovato_media",
            "trovato_audit_log",
        ] {
            // The plugin's own tables first: on a fresh database nothing has
            // created them, and the audit log page reads `audit_log`.
            common::ensure_plugin_tables(app, plugin);
            app.ensure_plugin_enabled(plugin).await;
        }
        app.ensure_conference_items().await;
        let name = common::username("b199i18n");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;

        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back("/admin".to_string());
        for slug in [
            "content",
            "structure",
            "appearance",
            "people",
            "extend",
            "config",
            "reports",
        ] {
            queue.push_back(format!("/admin/{slug}/overview"));
        }
        queue.push_back("/admin/preferences".to_string());
        queue.push_back("/admin/search?q=content".to_string());

        let mut seen_shapes = BTreeSet::new();
        let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut errors = Vec::new();
        let mut pages = 0;
        while let Some(path) = queue.pop_front() {
            let bare = path.split('?').next().unwrap_or("").to_string();
            if skip(&bare) || !seen_shapes.insert(shape(&bare)) {
                continue;
            }
            pages += 1;
            assert!(pages < 400, "the crawl ran away: {seen_shapes:?}");
            let (status, content_type, html) = get(app, &cookies, &path).await;
            if status.is_server_error() {
                errors.push(format!("{path}: {status}"));
                continue;
            }
            if status != StatusCode::OK || !content_type.starts_with("text/html") {
                continue;
            }
            for link in admin_links(&html) {
                queue.push_back(link);
            }
            let left = untranslated(&html);
            if !left.is_empty() {
                let mut left = left;
                left.dedup();
                failures.insert(format!("{} ({})", shape(&bare), title_of(&html)), left);
            }
        }

        assert!(
            errors.is_empty(),
            "admin pages answered with a server error:\n{}",
            errors.join("\n")
        );
        assert!(
            pages > 40,
            "the crawl reached only {pages} pages: {seen_shapes:?}"
        );
        let report: Vec<String> = failures
            .iter()
            .map(|(page, texts)| format!("{page}\n    {}", texts.join("\n    ")))
            .collect();
        assert!(
            failures.is_empty(),
            "{} of {pages} admin page types print text that never passed through t():\n{}",
            failures.len(),
            report.join("\n")
        );
        forget(app, "b199i18n").await;
    });
}
