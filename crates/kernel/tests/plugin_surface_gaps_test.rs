#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Three surfaces a plugin does not have, pinned so the documentation cannot drift
//! from the code.
//!
//! These were found attempting a contact form as a standard plugin, which is where
//! kernel minimality puts it. The attempt stopped; `KNOWN-ISSUES.md` says why, and
//! this file makes each reason checkable rather than a claim in a paragraph.
//!
//! **Every test here fails when the gap is closed**, deliberately, the way
//! `menu_admin_absent_test.rs` did before the menu screen existed. A failure means:
//! the surface now exists, so update KNOWN-ISSUES.md and delete the test.
//!
//! No database or Redis needed: these read the contract and the source.

use std::fs;
use std::path::{Path, PathBuf};

fn project_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    Path::new(&manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = project_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// **Gap 1: there is no mail host interface.**
///
/// The kernel has SMTP, a circuit breaker and a template system in
/// `services/email.rs`, and no seam onto it. A plugin that needs to notify somebody
/// posts to a webhook over `http`, which is what `argus` does, and which is not email.
#[test]
fn the_plugin_contract_exposes_no_mail_interface() {
    let wit = read("crates/wit/kernel.wit");

    let interfaces: Vec<&str> = wit
        .lines()
        .filter_map(|line| line.trim().strip_prefix("interface "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();

    assert!(
        !interfaces.is_empty(),
        "the WIT must declare host interfaces; the parse above is wrong if this fails"
    );

    for interface in &interfaces {
        assert!(
            !interface.contains("mail") && !interface.contains("email"),
            "a mail host interface exists ('{interface}'). A plugin can send email now, \
             so remove this gap from KNOWN-ISSUES.md and delete this test."
        );
    }

    // And the kernel really does have the infrastructure a plugin cannot reach, so
    // the gap is a missing seam rather than a missing capability.
    let email_service = read("crates/kernel/src/services/email.rs");
    assert!(
        email_service.contains("send_templated"),
        "the kernel's email service must still be what a plugin has no access to"
    );
}

/// **Gap 2: a plugin-served form cannot work without JavaScript.**
///
/// A state-changing plugin-served request requires the CSRF token in an
/// `X-CSRF-Token` header. A plain HTML form cannot set a header, so a plugin's own
/// `<form method="post">` is refused unless JavaScript posts it. Every kernel form
/// takes a `_token` field instead.
#[test]
fn a_plugin_served_post_requires_a_csrf_header_rather_than_a_form_field() {
    let helpers = read("crates/kernel/src/routes/helpers.rs");

    // The signature is the fact, not the body: `require_csrf_header` takes a session
    // and a `HeaderMap` and no body, so it *cannot* read a form field however it is
    // written inside. (An earlier version of this test scanned the body for `_token`
    // and matched `verify_csrf_token`, which is a good example of why a substring
    // search is not a check.)
    let signature = helpers
        .split("pub async fn require_csrf_header")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("require_csrf_header must exist");
    assert!(
        signature.contains("HeaderMap"),
        "require_csrf_header must take a HeaderMap: {signature}"
    );
    assert!(
        !signature.contains("body") && !signature.contains("Form"),
        "require_csrf_header now takes a body or a form, so it may accept a `_token`          field and a plugin may be able to serve a no-JS form: check KNOWN-ISSUES.md          and delete this test. Signature: {signature}"
    );

    // And the plugin route gates on it, telling the caller to send a header.
    let plugin_api = read("crates/kernel/src/routes/plugin_api.rs");
    assert!(
        plugin_api.contains("require_csrf_header"),
        "plugin-served requests must still be gated on the header reader"
    );
    assert!(
        plugin_api.contains("Include an X-CSRF-Token header"),
        "and must still tell the caller a header is what it wants, which is the part a          plain <form> cannot do"
    );
}

/// **Gap 3: a plugin cannot render into the site theme.**
///
/// `tap_api` returns a body the kernel serves as-is. The taps that would let a plugin
/// reach the theme are declared and not dispatched.
#[test]
fn the_theme_taps_are_declared_and_not_dispatched() {
    let wit = read("crates/wit/kernel.wit");
    for tap in ["tap-theme", "tap-preprocess-item"] {
        assert!(
            wit.contains(tap),
            "{tap} must still be declared in the contract"
        );
    }

    // Nothing *dispatches* them. Searched for the dispatch call rather than for the
    // name, because `plugin/info_parser.rs` names every tap in `KNOWN_TAPS` — that is
    // the declaration, which is exactly what these taps have and what they lack.
    let dispatched =
        kernel_sources_mentioning(&["dispatch(\"tap_theme\"", "dispatch(\"tap_preprocess_item\""]);
    assert!(
        dispatched.is_empty(),
        "a theme tap is dispatched now ({dispatched:?}), so a plugin may be able to \
         render into the site template: check KNOWN-ISSUES.md and delete this test."
    );
}

/// Every `.rs` file under `crates/kernel/src` that mentions any of `needles`.
fn kernel_sources_mentioning(needles: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let root = project_root().join("crates/kernel/src");
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            if needles.iter().any(|needle| contents.contains(needle)) {
                found.push(
                    path.strip_prefix(&root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                );
            }
        }
    }
    found
}

/// And the documentation says all three, so the gap list and the code agree.
#[test]
fn the_documentation_records_all_three_gaps() {
    let known_issues = read("KNOWN-ISSUES.md");

    assert!(
        known_issues.contains("Three things a plugin cannot do"),
        "KNOWN-ISSUES.md must carry the plugin-surface gap list"
    );
    for phrase in [
        "A plugin cannot send email",
        "cannot serve a form that works without JavaScript",
        "A plugin cannot render into the site theme",
    ] {
        assert!(
            known_issues.contains(phrase),
            "KNOWN-ISSUES.md must record: {phrase}"
        );
    }

    let roadmap = read("ROADMAP.md");
    assert!(
        roadmap.contains("A contact form, and the three plugin surfaces it needs"),
        "ROADMAP.md must say what is blocked on these and in what order to unblock it"
    );
}
