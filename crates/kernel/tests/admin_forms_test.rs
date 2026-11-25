#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The admin form pattern (B199 Part 2, `docs/admin/design.md` section 8).
//!
//! Every admin form, hand written or built in Rust, is drawn by one set of
//! macros (`templates/admin/macros/form.html`): a label above the field; a
//! hint under it tied with `aria-describedby`; "(required)" in words; related
//! choices in a fieldset with a legend; advanced settings in native
//! `details`. A failed submit leads with an error summary (an alert with one
//! link per problem, each moving to its field), the title starts "Error:",
//! and each failed field has `aria-invalid` and its message tied to it.
//!
//! Before this part, hand written forms showed one unlinked list of errors and
//! no field was marked; builder forms marked required with a CSS asterisk,
//! and their collapsible fieldset opened only from an `onclick` on its
//! legend, which the keyboard cannot reach.
//!
//! The first tests need only the theme engine; the rest use the shared
//! `TestApp` (Postgres + Redis).

mod common;

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::form::{Form, FormElement, ValidationError};
use trovato_kernel::theme::ThemeEngine;

fn root() -> PathBuf {
    common::project_root()
}

fn engine() -> ThemeEngine {
    ThemeEngine::new(
        &[root().join("templates")],
        None,
        std::sync::Arc::new(trovato_kernel::metrics::Metrics::new()),
    )
    .expect("the templates load")
}

/// Whitespace collapsed, and none between tags: markup compared by what it
/// says, not how a template happens to indent it.
fn squash(html: &str) -> String {
    let joined = html.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.replace("> <", "><").replace(" >", ">")
}

/// The opening tag of the element whose `id` is `id`.
fn tag_with_id<'a>(html: &'a str, id: &str) -> &'a str {
    let needle = format!(r#"id="{id}""#);
    let at = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no element with id {id}"));
    let start = html[..at].rfind('<').unwrap();
    let end = at + html[at..].find('>').unwrap();
    &html[start..=end]
}

fn sample_form(action: &str) -> Form {
    Form::new("b199_sample")
        .action(action)
        .element(
            "name",
            FormElement::textfield()
                .title("Name")
                .description("What people call it.")
                .required()
                .weight(0),
        )
        .element(
            "colour",
            FormElement::select(vec![
                ("red".into(), "Red".into()),
                ("blue".into(), "Blue".into()),
            ])
            .title("Colour")
            .default_value("blue")
            .weight(1),
        )
        .element(
            "size",
            FormElement::radio(vec![
                ("s".into(), "Small".into()),
                ("l".into(), "Large".into()),
            ])
            .title("Size")
            .weight(2),
        )
        .element(
            "agree",
            FormElement::checkbox()
                .title("Publish it")
                .description("Shown to everyone once saved.")
                .weight(3),
        )
        .element(
            "advanced",
            FormElement::fieldset_collapsible(true)
                .title("Advanced")
                .child("note", FormElement::textarea(3).title("Note"))
                .weight(4),
        )
        .element("save", FormElement::submit("Save").weight(5))
}

/// **The finding, builder half.** An admin form built in Rust is drawn by the
/// admin macros: required in words, the hint tied to the field, choices in a
/// fieldset, a collapsible group as `details`; on failure the error summary
/// comes first and links to the field, which is marked invalid and names its
/// message.
#[test]
fn an_admin_builder_form_follows_the_field_pattern() {
    let engine = engine();
    let errors = vec![ValidationError::field("name", "Name is required.")];
    let html = engine
        .render_form_with_errors(&sample_form("/admin/b199/sample"), &errors)
        .unwrap();

    assert!(html.contains(" novalidate"), "validated on the server: {html}");
    assert!(
        !html.contains("form-item__label--required"),
        "required is not a CSS asterisk"
    );
    let label = squash(&html[html.find(r#"<label class="form-item__label" for="name""#).unwrap()..]);
    assert!(
        label.starts_with(r#"<label class="form-item__label" for="name">Name <span class="form-item__need">(required)</span></label>"#),
        "required in the label's words: {label}"
    );
    assert!(html.contains(r#"<p class="form-item__description" id="name-hint">What people call it.</p>"#));

    let summary = html.find(r#"class="error-summary""#).expect("an error summary");
    assert!(
        summary < html.find(r#"for="name""#).unwrap(),
        "the summary comes before the fields"
    );
    assert!(html[summary..].starts_with(r#"class="error-summary" role="alert" tabindex="-1""#));
    assert!(html.contains(r##"<a href="#name">Name is required.</a>"##));

    let input = tag_with_id(&html, "name");
    assert!(input.contains(r#"aria-invalid="true""#), "{input}");
    assert!(
        input.contains(r#"aria-describedby="name-hint name-error""#),
        "{input}"
    );
    assert!(html.contains(r#"id="name-error""#), "the message has an id");

    // Radios in a fieldset with a legend; the first carries the field's id.
    let size = &html[html.find(r#"<fieldset class="fieldset fieldset--choices""#).unwrap()..];
    assert!(size.contains(r#"<legend class="fieldset__legend">Size</legend>"#));
    assert!(size.contains(r#"<input type="radio" id="size" name="size" value="s""#));

    // The collapsible group is native details, opened by any keyboard.
    assert!(html.contains("<details"), "{html}");
    assert!(html.contains(r#"<summary class="form-details__summary">Advanced</summary>"#));
    assert!(!html.contains("onclick"), "no inline handler");
}

/// **The finding, sameness.** A field written by hand with the macro and the
/// same field built in Rust are the same markup.
#[test]
fn hand_written_and_builder_fields_are_the_same_markup() {
    let mut engine = engine();
    engine
        .tera_mut()
        .add_raw_template(
            "b199-hand.html",
            r#"{% import "admin/macros/form.html" as f %}{{ f::field(name="name", label=t(s="Name"), hint=t(s="What people call it."), required=true) }}|{{ f::select(name="colour", label=t(s="Colour"), options=colours, value="blue", empty=t(s="- Select -")) }}|{{ f::checkbox(name="agree", label=t(s="Publish it"), hint=t(s="Shown to everyone once saved.")) }}"#,
        )
        .unwrap();
    let hand = engine
        .tera()
        .render("b199-hand.html", &{
            let mut context = tera::Context::new();
            context.insert("colours", &[["red", "Red"], ["blue", "Blue"]]);
            context
        })
        .unwrap();
    let hand: Vec<String> = hand.split('|').map(squash).collect();

    let form = sample_form("/admin/b199/sample");
    for (i, name) in ["name", "colour", "agree"].iter().enumerate() {
        let built = engine
            .render_form_element_html(name, &form.elements[*name], true)
            .unwrap();
        assert_eq!(squash(&built), hand[i], "{name} differs");
    }
}

/// A form whose action is not an admin address still renders through the
/// front end's templates, untouched.
#[test]
fn a_front_end_builder_form_keeps_its_own_templates() {
    let html = engine()
        .render_form_with_errors(&sample_form("/item/add/page"), &[])
        .unwrap();
    assert!(html.contains("form-item__label--required"), "{html}");
    assert!(!html.contains("error-summary"));
    assert!(!html.contains(" novalidate"));
}

/// **F4.** The collapsible fieldset the front end's form templates draw is
/// native details too: it opened only from an `onclick` on its legend, which
/// no keyboard reaches.
#[test]
fn the_shared_collapsible_fieldset_opens_from_the_keyboard() {
    let engine = engine();
    let mut context = tera::Context::new();
    context.insert("name", "advanced");
    context.insert("children", "<p>inside</p>");
    context.insert(
        "element",
        &serde_json::json!({"type": "fieldset", "title": "Advanced", "collapsible": true, "collapsed": true}),
    );
    let html = engine.tera().render("form/fieldset.html", &context).unwrap();
    assert!(!html.contains("onclick"), "{html}");
    assert!(html.contains("<details"), "{html}");
    assert!(!html.contains(" open"), "collapsed starts closed: {html}");
    assert!(html.contains(r#"<summary class="fieldset__summary">Advanced</summary>"#));
}

fn admin_templates(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            admin_templates(&path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

/// **The finding, coverage.** No admin template draws a field by hand: each
/// uses the macros, so none is left outside the pattern. A form that posts
/// is validated on the server (`novalidate`), and nothing uses an inline
/// handler.
#[test]
fn no_admin_form_is_left_outside_the_pattern() {
    let mut files = Vec::new();
    admin_templates(&root().join("templates/admin"), &mut files);
    for plugin in std::fs::read_dir(root().join("plugins")).unwrap() {
        let dir = plugin.unwrap().path().join("templates/admin");
        if dir.is_dir() {
            admin_templates(&dir, &mut files);
        }
    }
    let mut problems = Vec::new();
    for path in files {
        let rel = path.strip_prefix(root()).unwrap().display().to_string();
        // The pattern itself.
        if rel.ends_with("admin/macros/form.html")
            || rel.ends_with("admin/macros/builder.html")
            || rel.contains("admin/form/")
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for needle in [
            r#"class="form-item"#,
            "<select",
            "<textarea",
            "<legend",
            "form-item__label",
            "form-item__required",
            "onclick=",
            "onchange=",
            "onsubmit=",
        ] {
            if source.contains(needle) {
                problems.push(format!("{rel}: draws `{needle}` by hand"));
            }
        }
        // A visible label written by hand is a field drawn by hand; a
        // visually hidden one names a control in a grid's cell (the
        // permission grid), which the table's headers already label.
        for (at, _) in source.match_indices("<label") {
            if !source[at..].starts_with(r#"<label class="visually-hidden""#) {
                let line = source[..at].lines().count();
                problems.push(format!("{rel}:{line}: a label written by hand"));
            }
        }
        let mut rest = source.as_str();
        while let Some(at) = rest.find("<form") {
            let tag = &rest[at..at + rest[at..].find('>').unwrap_or(rest.len() - at)];
            if tag.to_ascii_lowercase().contains(r#"method="post""#) && !tag.contains("novalidate")
            {
                problems.push(format!("{rel}: a posting form without novalidate: {tag}"));
            }
            rest = &rest[at + 5..];
        }
    }
    assert!(
        problems.is_empty(),
        "admin forms outside the pattern:\n{}",
        problems.join("\n")
    );
}

/// The unused second set of form macros is gone.
#[test]
fn the_unused_form_macros_are_gone() {
    assert!(!root().join("templates/macros/forms.html").exists());
}

// --- Through the app -------------------------------------------------------

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
                .header("x-forwarded-for", test_ip_for("b199forms"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body(response).await)
}

async fn post(app: &TestApp, cookies: &str, uri: &str, form: String) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", test_ip_for("b199forms"))
                .body(Body::from(form))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body(response).await)
}

fn hidden(html: &str, name: &str) -> String {
    let needle = format!(r#"name="{name}" value=""#);
    let at = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no {name} on the page"))
        + needle.len();
    html[at..at + html[at..].find('"').unwrap()].to_string()
}

fn title(html: &str) -> String {
    let at = html.find("<title>").unwrap() + 7;
    html[at..at + html[at..].find("</title>").unwrap()].trim().to_string()
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

/// **The finding, hand written half.** A failed submit of a hand written
/// admin form leads with an error summary whose links go to the failed
/// fields, the title starts "Error:", and each field is marked invalid with
/// its message tied to it.
#[test]
fn a_failed_hand_written_form_leads_with_a_linked_error_summary() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199formsuser").await;
        let (status, page) = get(app, &cookies, "/admin/people/add").await;
        assert_eq!(status, StatusCode::OK);
        assert!(tag_with_id(&page, "user-form").contains(" novalidate"));
        assert!(
            tag_with_id(&page, "mail").contains(r#"autocomplete="email""#),
            "person fields say what they hold"
        );

        let form = format!(
            "_token={}&_form_build_id={}&name=&mail=&password=short",
            urlencoding::encode(&hidden(&page, "_token")),
            urlencoding::encode(&hidden(&page, "_form_build_id")),
        );
        let (_, page) = post(app, &cookies, "/admin/people/add", form).await;

        assert!(
            title(&page).starts_with("Error: "),
            "the title says it failed: {}",
            title(&page)
        );
        let summary = page.find(r#"class="error-summary""#).expect("an error summary");
        assert!(summary < page.find(r#"for="name""#).unwrap());
        for field in ["name", "mail", "password"] {
            assert!(
                page.contains(&format!(r##"<a href="#{field}">"##)),
                "the summary links to {field}"
            );
            let input = tag_with_id(&page, field);
            assert!(input.contains(r#"aria-invalid="true""#), "{input}");
            assert!(
                input.contains(&format!("{field}-error")),
                "{field}'s message is tied to it: {input}"
            );
        }
        assert!(page.contains(r##"<a href="#name">Username is required.</a>"##));
    });
}

/// The same, for a form built in Rust on an admin page (the item form).
#[test]
fn a_failed_builder_form_on_an_admin_page_leads_with_the_error_summary() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199formsitem").await;
        let (status, page) = get(app, &cookies, "/admin/content/add/page").await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(page.contains(" novalidate"));
        let at = page.find(r#"name="_form_build_id""#).unwrap();
        let scoped = &page[page[..at].rfind("<form").unwrap()..];
        let form = format!(
            "_token={}&_form_build_id={}&title=",
            urlencoding::encode(&hidden(scoped, "_token")),
            urlencoding::encode(&hidden(scoped, "_form_build_id")),
        );
        let (_, page) = post(app, &cookies, "/admin/content/add/page", form).await;
        assert!(title(&page).starts_with("Error: "), "{}", title(&page));
        assert!(page.contains(r#"class="error-summary""#), "{page}");
        assert!(page.contains(r##"<a href="#title">"##), "{page}");
        assert!(tag_with_id(&page, "title").contains(r#"aria-invalid="true""#));
    });
}

/// Each settings form says, above Save, what saving it changes.
#[test]
fn settings_forms_say_what_they_change() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199formseffect").await;
        for path in [
            "/admin/config/site",
            "/admin/config/recovery",
            "/admin/preferences",
        ] {
            let (status, page) = get(app, &cookies, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            let effect = page
                .find(r#"class="form__effect""#)
                .unwrap_or_else(|| panic!("{path}: no line saying what this changes"));
            let save = page[effect..]
                .find(r#"type="submit""#)
                .map(|p| p + effect);
            assert!(save.is_some(), "{path}: the line is above Save");
        }
    });
}

/// The permission grid is a table with row and column headers, and each
/// checkbox's name says both the role and the permission.
#[test]
fn the_permission_grid_names_each_box_by_role_and_permission() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199formsperm").await;
        let (status, page) = get(app, &cookies, "/admin/people/permissions").await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(r#"<th scope="col""#));
        assert!(page.contains(r#"<th scope="row""#));
        let at = page.find(r#"type="checkbox""#).unwrap();
        let cell = &page[at..at + page[at..].find("</td>").unwrap()];
        let id = cell.split(r#"id=""#).nth(1).unwrap().split('"').next().unwrap();
        let label_at = page.find(&format!(r#"for="{id}""#)).expect("a label");
        let label = &page[label_at..label_at + page[label_at..].find("</label>").unwrap()];
        assert!(label.contains(": "), "role: permission, {label}");
    });
}
