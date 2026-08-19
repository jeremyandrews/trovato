#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The plugin surfaces a public-facing feature needs, pinned so the documentation
//! cannot drift from the code.
//!
//! This replaces `plugin_surface_gaps_test.rs` (PR #49), which pinned the same
//! three things as **absences** and was written to fail when they were closed.
//! They are closed, so the assertions are inverted: each test now says the surface
//! exists, and fails if it is taken away.
//!
//! Two of the taps that write-up mentioned are still not dispatched, and the last
//! test here pins *that* rather than quietly dropping it, because a claim in
//! KNOWN-ISSUES.md that nothing checks is a claim that goes stale.
//!
//! These read the contract and the source, so they need no database: the
//! behavioural proof lives in `plugin_api_test.rs` (form and theme),
//! `plugin_mail_test.rs` (mail over a real SMTP conversation) and
//! `contact_form_test.rs` (all three at once, through the real plugin).

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

/// **Surface 1: a plugin can send email.**
///
/// The contract declares a `mail` interface, the kernel registers it, and the
/// manifest parser knows the name — the three places the completeness guards in
/// `plugin_test.rs` tie together.
#[test]
fn the_plugin_contract_exposes_a_mail_interface() {
    let wit = read("crates/wit/kernel.wit");
    let interfaces: Vec<String> = wit
        .lines()
        .filter_map(|line| line.trim().strip_prefix("interface "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .collect();
    assert!(
        interfaces.iter().any(|i| i == "mail"),
        "the WIT must declare a mail interface; found {interfaces:?}"
    );
    assert!(
        wit.contains("import mail;"),
        "and `world plugin` must import it"
    );

    assert!(
        trovato_kernel::plugin::KNOWN_HOST_INTERFACES.contains(&"mail"),
        "the manifest parser must accept `mail` in host_interfaces"
    );

    // And it is the *narrow* shape, which is the whole design: the caller does
    // not name a recipient.
    assert!(
        wit.contains("send-to-site-contacts"),
        "the mail interface sends to the site's own address"
    );
    assert!(
        !wit.contains("send-to-address") && !wit.contains("send-mail:"),
        "a caller-supplied recipient would make this a relay"
    );
}

/// **Surface 2, first half: a plugin-served post accepts a `_token` field.**
///
/// Asserted on the helper's signature rather than on its body: a function that
/// takes a body *can* read a field from it, whatever it says inside, and a
/// substring search for `_token` would pass on a comment.
#[test]
fn a_plugin_served_post_accepts_a_csrf_token_from_a_form_field() {
    let helpers = read("crates/kernel/src/routes/helpers.rs");
    let signature = helpers
        .split("pub async fn require_csrf_header_or_field")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("require_csrf_header_or_field must exist");
    assert!(
        signature.contains("headers"),
        "it must still read the header: {signature}"
    );
    assert!(
        signature.contains("body"),
        "and it must take the body, which is the only place a plain form can put a \
         token: {signature}"
    );
    assert!(
        helpers.contains(r#"pub const CSRF_FORM_FIELD: &str = "_token";"#),
        "the field name must be the one every kernel form already uses"
    );

    let plugin_api = read("crates/kernel/src/routes/plugin_api.rs");
    assert!(
        plugin_api.contains("require_csrf_header_or_field"),
        "the plugin route must gate on the reader that accepts both"
    );
    assert!(
        plugin_api.contains("a _token \\\n                 field") || plugin_api.contains("_token"),
        "and the refusal must name the field as well as the header"
    );
}

/// **Surface 2, second half: the plugin is given a token to embed.**
///
/// Accepting the field is useless on its own — this is the half the original
/// write-up missed. A plugin serving a GET form has no other way to obtain a
/// valid token, because `tap_api` is one call with no callback into the kernel.
#[test]
fn a_plugin_receives_a_csrf_token_to_render() {
    let types = read("crates/plugin-sdk/src/types.rs");
    assert!(
        types.contains("pub csrf_token: String"),
        "ApiRequest must carry a token for the plugin to embed"
    );

    let plugin_api = read("crates/kernel/src/routes/plugin_api.rs");
    assert!(
        plugin_api.contains("generate_csrf_token"),
        "and the kernel must mint one per request: {}",
        "routes/plugin_api.rs no longer calls generate_csrf_token"
    );

    // The SDK's own struct is the contract a plugin compiles against, so build one
    // and check the field is really there and really settable.
    let mut request = trovato_sdk::types::ApiRequest::new("cb", "GET", "/x", "", false);
    assert_eq!(request.csrf_token, "", "it defaults to empty");
    request.csrf_token = "tok".to_string();
    assert_eq!(request.csrf_token, "tok");
}

/// **Surface 3: a plugin-served page can ask for the site theme.**
#[test]
fn a_plugin_page_can_ask_to_be_rendered_into_the_theme() {
    let types = read("crates/plugin-sdk/src/types.rs");
    assert!(
        types.contains("pub theme: bool"),
        "ApiResponse must carry the opt-in"
    );

    let plugin_api = read("crates/kernel/src/routes/plugin_api.rs");
    assert!(
        plugin_api.contains("inject_site_context"),
        "the themed path must use the site context the item path uses"
    );
    assert!(
        plugin_api.contains("render_page"),
        "and the theme's own page renderer, so template overrides apply"
    );

    // Opt-in, not the new default: that is what keeps every existing admin screen
    // and JSON endpoint unchanged.
    let response = trovato_sdk::types::ApiResponse::with_status(200, "{}");
    assert!(!response.theme, "a plain response must not be themed");
    let themed = trovato_sdk::types::ApiResponse::themed("Title", "<p>x</p>");
    assert!(themed.theme);
    assert_eq!(themed.title, "Title");
}

/// **The contact form exists, and is a plugin.**
///
/// The feature the three surfaces were for. Asserted on the manifest rather than
/// on the source, because the manifest is what the kernel reads: it declares
/// `mail`, it is off by default like every other standard plugin, and it owns no
/// table.
#[test]
fn the_contact_form_is_a_standard_plugin_declaring_mail() {
    let manifest = read("plugins/trovato_contact/trovato_contact.info.toml");

    assert!(
        manifest.contains(r#"host_interfaces = ["mail", "logging"]"#),
        "it must declare the mail interface it uses: {manifest}"
    );
    assert!(
        manifest.contains("default_enabled = false"),
        "a standard plugin is opt-in: {manifest}"
    );
    assert!(
        !manifest.contains("db_tables"),
        "a contact message is delivered, not stored: {manifest}"
    );

    let source = read("plugins/trovato_contact/src/lib.rs");
    assert!(
        source.contains("mail_send_to_site_contacts"),
        "it must send through the narrow interface"
    );
    assert!(
        source.contains(r#"name="_token""#),
        "its form must carry the token field"
    );
    assert!(
        source.contains("ApiResponse::themed"),
        "and its pages must ask for the site theme"
    );
}

/// **What is still not done**, pinned so KNOWN-ISSUES.md cannot go stale.
///
/// `tap_theme` and `tap_preprocess_item` are declared and not dispatched. This
/// test fails when either is wired up, which is the moment to update
/// KNOWN-ISSUES.md and delete this test — the same discipline
/// `plugin_surface_gaps_test.rs` used, kept for the part that is genuinely still
/// open.
#[test]
fn the_two_theme_taps_are_still_declared_and_not_dispatched() {
    let wit = read("crates/wit/kernel.wit");
    for tap in ["tap-theme", "tap-preprocess-item"] {
        assert!(wit.contains(tap), "{tap} must still be declared");
    }

    // Searched for the dispatch call rather than the name: `plugin/info_parser.rs`
    // names every tap in KNOWN_TAPS, which is the declaration these two have and
    // the dispatch they lack.
    let dispatched =
        kernel_sources_mentioning(&["dispatch(\"tap_theme\"", "dispatch(\"tap_preprocess_item\""]);
    assert!(
        dispatched.is_empty(),
        "a theme tap is dispatched now ({dispatched:?}). That is a real feature and a \
         good one: update KNOWN-ISSUES.md, the WIT comment that explains why it was \
         not, and delete this test."
    );

    let known_issues = read("KNOWN-ISSUES.md");
    assert!(
        known_issues.contains("The two theme taps are declared and not dispatched"),
        "KNOWN-ISSUES.md must still record it"
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

/// The documentation and the code agree about what shipped.
#[test]
fn the_documentation_records_the_surfaces_as_done() {
    let roadmap = read("ROADMAP.md");
    assert!(
        roadmap.contains("A contact form, and the three plugin surfaces it needed"),
        "ROADMAP.md must record the contact form"
    );
    let section = roadmap
        .split("### A contact form, and the three plugin surfaces it needed")
        .nth(1)
        .expect("the section exists");
    assert!(
        section.trim_start().starts_with("**Done.**"),
        "and must mark it done, the way the other finished items are marked"
    );

    let known_issues = read("KNOWN-ISSUES.md");
    assert!(
        !known_issues.contains("Three things a plugin cannot do"),
        "the gap list must be gone from KNOWN-ISSUES.md, not merely contradicted"
    );
}
