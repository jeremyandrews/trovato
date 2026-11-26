#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every word an admin script puts on the page passed through translation
//! before the script wrote it, not as a literal in the script itself.
//!
//! `admin_i18n_test.rs`'s marker-mode crawl reads only the HTML a response
//! renders: a `<script>` body is markup it drops (`drop_blocks`), because a
//! plain HTTP client never runs it. A phrase an admin script writes into the
//! page at runtime (B253: `static/js/ai-admin.js` and
//! `static/js/gather-builder.js`, fixed by routing every word a script sets
//! through a translated `<script type="application/json">` data block the
//! admin template renders with `t()`, the mechanism
//! `templates/admin/ai-provider-form.html`'s `ai-operations` block already
//! used) never reaches that crawl at all.
//!
//! This test is that crawl's JS-side companion: it reads the guarded
//! scripts' source text directly and fails if either assigns a literal,
//! non-empty string to a property that puts it on the page (`textContent`,
//! `innerHTML`, `innerText`, `placeholder`, `title`, or an `aria-label` /
//! `title` / `placeholder` set through `setAttribute`, or a
//! `document.createTextNode` call). Clearing a node (`x.innerHTML = ''`) is
//! not a word, so an empty literal does not count.
//!
//! It is not a general admin-JS scanner: other admin scripts (`ai-assist.js`,
//! `block-editor.js`, `chat-widget.js`, `file-upload.js`, `media-picker.js`,
//! `page-builder-editor.js`, `scolta.js`) still set literal English this way
//! and are not B253's scope (its problem was named as exactly these two
//! scripts plus `mask_key_ref`); extend [`GUARDED_SCRIPTS`] as each is fixed.
//!
//! Needs no database: it reads files from the checkout.

mod common;

use regex::Regex;
use std::sync::LazyLock;

/// The admin scripts this guard currently covers, repo-relative.
const GUARDED_SCRIPTS: &[&str] = &["static/js/ai-admin.js", "static/js/gather-builder.js"];

/// `.textContent = '…'`, `.innerHTML = "…"`, and the same for `innerText`,
/// `placeholder`, `title`: a non-empty quoted literal assigned to a property
/// that displays it. Two alternatives rather than a backreference on the
/// quote char: this crate's `regex` engine (guaranteed linear time) supports
/// neither backreferences nor lookaround.
static PROPERTY_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\.(textContent|innerHTML|innerText|placeholder|title)\s*=\s*(?:'([^']+)'|"([^"]+)")"#,
    )
    .expect("valid regex")
});

/// `setAttribute('aria-label', '…')` and the same for `title`/`placeholder`.
static SET_ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"setAttribute\(\s*(?:'(aria-label|title|placeholder)'|"(aria-label|title|placeholder)")\s*,\s*(?:'([^']+)'|"([^"]+)")\s*\)"#,
    )
    .expect("valid regex")
});

/// `document.createTextNode('…')`.
static CREATE_TEXT_NODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"createTextNode\(\s*(?:'([^']+)'|"([^"]+)")\s*\)"#).expect("valid regex")
});

/// The first non-empty alternative among `groups`, by index.
fn first_some<'a>(groups: &[Option<regex::Match<'a>>]) -> Option<&'a str> {
    groups.iter().flatten().map(|m| m.as_str()).next()
}

/// Whether a matched literal is a word on the page rather than incidental:
/// not an escape sequence standing in for a symbol (`'✅ '`, a checkmark
/// glyph, not a word), and not an HTML fragment a script assembles by
/// concatenation (`'<select name="op_' + idx + ...`, where the quote the
/// regex stopped at is markup syntax, not the end of a sentence).
fn looks_like_text(value: &str) -> bool {
    !value.starts_with('\\') && !value.contains('<') && !value.contains('>')
}

/// Every literal, display-setting assignment `source` makes, each reported
/// as `property: "value"`. Empty (`''`/`""`) does not count: clearing a node
/// is not a word on the page; see [`looks_like_text`] for what else does not.
fn literal_display_assignments(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for cap in PROPERTY_ASSIGN.captures_iter(source) {
        let value = first_some(&[cap.get(2), cap.get(3)]).unwrap_or_default();
        if looks_like_text(value) {
            found.push(format!("{}: \"{value}\"", &cap[1]));
        }
    }
    for cap in SET_ATTRIBUTE.captures_iter(source) {
        let attr = first_some(&[cap.get(1), cap.get(2)]).unwrap_or_default();
        let value = first_some(&[cap.get(3), cap.get(4)]).unwrap_or_default();
        if looks_like_text(value) {
            found.push(format!("setAttribute({attr}): \"{value}\""));
        }
    }
    for cap in CREATE_TEXT_NODE.captures_iter(source) {
        let value = first_some(&[cap.get(1), cap.get(2)]).unwrap_or_default();
        if looks_like_text(value) {
            found.push(format!("createTextNode: \"{value}\""));
        }
    }
    found
}

#[test]
fn guarded_admin_scripts_set_no_literal_word_into_the_page() {
    let root = common::project_root();
    for script in GUARDED_SCRIPTS {
        let path = root.join(script);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let found = literal_display_assignments(&source);
        assert!(
            found.is_empty(),
            "{script} sets a literal word into the page, bypassing translation:\n{}",
            found.join("\n")
        );
    }
}

/// The guard actually catches a reintroduced literal: proven once here on a
/// string standing in for a script, not on the real files (which, the test
/// above having passed, carry none to reintroduce).
#[test]
fn the_guard_catches_a_literal_and_not_a_translated_reference_or_a_clear() {
    let regression = "msgEl.textContent = 'Testing connection...';";
    let found = literal_display_assignments(regression);
    assert_eq!(
        found,
        vec!["textContent: \"Testing connection...\"".to_string()]
    );

    let fixed = "msgEl.textContent = i18n.testing_connection;";
    assert_eq!(literal_display_assignments(fixed), Vec::<String>::new());

    let clears = "\
        el.innerHTML = '';\n\
        el.textContent = \"\";\n\
        el.setAttribute('aria-label', i18n.remove_filter);\n\
        document.createTextNode(' ' + i18n.singular_label);\n\
    ";
    assert_eq!(literal_display_assignments(clears), Vec::<String>::new());

    // An emoji escape and a concatenated HTML fragment are not words: a
    // naive scanner would stop at the first embedded quote and report
    // markup syntax, not a sentence.
    let incidental = "\
        msgEl.textContent = (ok ? '\\u2705 ' : '\\u274c ') + data.message;\n\
        row.innerHTML = '<select name=\"op_' + idx + '\"></select>';\n\
    ";
    assert_eq!(
        literal_display_assignments(incidental),
        Vec::<String>::new()
    );
}
