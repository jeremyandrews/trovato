#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The stock header's Logout control looks like the header links around it.
//!
//! Logout is a POST form with a CSRF token, so the header in `templates/page.html` draws
//! it as a `<button>` carrying `site-nav__link site-nav__logout`. The reset
//! rule for `.site-nav__logout` sat later in `static/css/theme.css` than
//! `.site-nav__link`, at the same specificity, and declared `font: inherit`,
//! `color: inherit` and `padding: 0`. Those won over the link style, so Logout
//! took the list item's 1rem instead of the links' 0.875rem, lost the links'
//! weight, colour and padding, and stood out in the header (B133).
//!
//! These checks read the files; no database is needed.

use std::path::PathBuf;

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The declarations of the first rule whose selector is exactly `selector`.
fn rule_body<'a>(css: &'a str, selector: &str) -> &'a str {
    let mut from = 0;
    while let Some(found) = css[from..].find(selector) {
        let start = from + found;
        let after = css[start + selector.len()..].trim_start();
        let line_start = css[..start].rfind('\n').map_or(0, |i| i + 1);
        if after.starts_with('{') && css[line_start..start].trim().is_empty() {
            let open = start + css[start..].find('{').unwrap();
            let close = open + css[open..].find('}').unwrap();
            return &css[open + 1..close];
        }
        from = start + selector.len();
    }
    panic!("no `{selector} {{ ... }}` rule in theme.css");
}

fn properties(body: &str) -> Vec<String> {
    body.split(';')
        .filter_map(|decl| decl.split_once(':'))
        .map(|(name, _)| {
            // Drop comments in front of a declaration.
            let name = name.rsplit("*/").next().unwrap_or(name);
            name.trim().to_ascii_lowercase()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// The logout reset sets nothing the header link style sets, so the link
/// style decides the button's size, weight, colour and padding wherever the
/// two rules sit in the sheet.
#[test]
fn logout_reset_leaves_the_link_style_alone() {
    let css = repo_file("static/css/theme.css");
    let logout = properties(rule_body(&css, ".site-nav__logout"));
    for forbidden in [
        "font",
        "font-size",
        "font-weight",
        "color",
        "padding",
        "border-radius",
        "display",
    ] {
        assert!(
            !logout.iter().any(|p| p == forbidden),
            ".site-nav__logout sets `{forbidden}`, which overrides .site-nav__link: {logout:?}"
        );
    }
    // What a browser gives a button and not a link is taken away.
    for needed in [
        "border",
        "background",
        "font-family",
        "line-height",
        "cursor",
    ] {
        assert!(
            logout.iter().any(|p| p == needed),
            ".site-nav__logout no longer resets `{needed}`: {logout:?}"
        );
    }
    // And the link style it relies on is still there.
    let link = properties(rule_body(&css, ".site-nav__link"));
    for needed in ["font-size", "font-weight", "color", "padding"] {
        assert!(link.iter().any(|p| p == needed), "{link:?}");
    }
}

/// Logout stays a button posting a POST form with its CSRF token, styled by
/// class and not by an inline style. The form is built by `FormService`
/// (`site_logout_form`, B83c), which gives it the token; it sits hidden at
/// the foot of the page and the header's button joins it through `form=`.
#[test]
fn logout_stays_a_post_button_with_the_link_class() {
    let page = repo_file("templates/page.html");
    assert!(page.contains(
        r#"<button type="submit" form="site_logout_form" class="site-nav__link site-nav__logout">Logout</button>"#
    ));
    assert!(page.contains("{{ logout_form | safe }}"));
    assert!(!page.contains(r#"action="/user/logout""#));
    let helpers = repo_file("crates/kernel/src/routes/helpers.rs");
    let at = helpers
        .find(r#"Form::new("site_logout_form")"#)
        .expect("the logout form is built in routes/helpers.rs");
    let build = &helpers[at..at + 200];
    assert!(build.contains(r#".action("/user/logout")"#));
    assert!(!build.contains("style="));
}
