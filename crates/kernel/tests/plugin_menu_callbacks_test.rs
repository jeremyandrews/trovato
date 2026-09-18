#![allow(clippy::unwrap_used, clippy::expect_used)]
//! No in-tree plugin declares a menu callback the kernel will never dispatch.
//!
//! The kernel dispatches a menu entry's `callback` only when its `handler_type`
//! is `"api"` (`routes::plugin_api::unreachable_callbacks`), and at startup it
//! logs a warning for every entry that names a callback without it. Sixteen
//! in-tree plugins did exactly that, 22 entries in all, so a default install
//! opened its log with a wall of warnings about the project's own code, which
//! teaches everyone reading it to ignore the one warning that exists to catch a
//! dead plugin route.
//!
//! The warning stays a warning: an external plugin that trips it still loads.
//! This test is the hard line for the plugins in this repository.
//!
//! It reads source rather than running the plugins, because the kernel only sees
//! a plugin's menu after building and loading its WASM, and CI builds only the
//! plugins its integration tests need. A check that silently skipped every
//! unbuilt plugin would pass for the wrong reason. The source rule is the kernel
//! rule restated for the two SDK types a `tap_menu` can return:
//!
//! - `MenuDefinition` has no `handler_type`; the kernel deserializes it as
//!   `"page"`. Any callback on one is unreachable, and `.callback(` is the only
//!   SDK builder of that name.
//! - `MenuRoute` is routed only as `MenuRoute::api(...)` or a literal carrying
//!   `handler_type: "api"`. A literal with a callback and any other handler type
//!   is unreachable.
//!
//! Needs no database.

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

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The source a plugin ships, without its unit-test module.
fn shipped_source(file: &Path) -> String {
    let source = fs::read_to_string(file).expect("read plugin source");
    source
        .split("#[cfg(test)]")
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Line numbers (1-based) of every unreachable callback declaration in `source`.
fn unreachable_callback_lines(source: &str) -> Vec<usize> {
    let line_of = |offset: usize| source[..offset].matches('\n').count() + 1;
    let mut lines: Vec<usize> = source
        .match_indices(".callback(")
        .map(|(at, _)| line_of(at))
        .collect();

    for literal in ["MenuRoute {", "MenuDefinition {"] {
        for (at, _) in source.match_indices(literal) {
            let body = &source[at..];
            let body = &body[..body
                .find("\n}")
                .or_else(|| body.find('}'))
                .unwrap_or(body.len())];
            let names_callback = body.split("callback:").nth(1).is_some_and(|rest| {
                !rest.trim_start().starts_with("String::new()")
                    && !rest.trim_start().starts_with("\"\"")
            });
            let is_api = body.contains("handler_type: \"api\"");
            if names_callback && !is_api {
                lines.push(line_of(at));
            }
        }
    }

    lines.sort_unstable();
    lines
}

#[test]
fn no_in_tree_plugin_declares_a_callback_the_kernel_never_dispatches() {
    let plugins = project_root().join("plugins");
    let mut scanned_menus = 0;
    let mut api_routes = 0;
    let mut offenders = Vec::new();

    for entry in fs::read_dir(&plugins).expect("read plugins/").flatten() {
        let dir = entry.path();
        let mut files = Vec::new();
        rust_files(&dir.join("src"), &mut files);
        for file in files {
            let source = shipped_source(&file);
            if source.contains("fn tap_menu") {
                scanned_menus += 1;
            }
            api_routes += source.matches("MenuRoute::api(").count();
            for line in unreachable_callback_lines(&source) {
                let relative = file.strip_prefix(project_root()).unwrap_or(&file);
                offenders.push(format!("{}:{line}", relative.display()));
            }
        }
    }

    // The scan has to be reading real plugins for a pass to mean anything.
    assert!(
        scanned_menus >= 20,
        "found only {scanned_menus} tap_menu implementations under plugins/"
    );
    assert!(
        api_routes >= 10,
        "found only {api_routes} MenuRoute::api routes, so the scan is not seeing plugin source"
    );

    assert!(
        offenders.is_empty(),
        "{} menu entr{} name a callback without handler_type \"api\"; the kernel logs a \
         warning for each at startup and never dispatches them. Remove the callback, or \
         declare the route with MenuRoute::api:\n{}",
        offenders.len(),
        if offenders.len() == 1 { "y" } else { "ies" },
        offenders.join("\n")
    );
}

/// The rule itself, on inputs whose answer is known.
#[test]
fn the_source_rule_matches_the_kernel_rule() {
    let unreachable = r#"
        MenuDefinition::new("/a", "A").callback("a").permission("x"),
        MenuRoute { path: "/b".into(), callback: "b".into(), handler_type: "page".into() }
    "#;
    assert_eq!(unreachable_callback_lines(unreachable).len(), 2);

    let reachable = r#"
        MenuDefinition::new("/a", "A").permission("x"),
        MenuRoute::api("POST", "/b", "b"),
        MenuRoute { path: "/c".into(), callback: "c".into(), handler_type: "api".into() }
    "#;
    assert_eq!(
        unreachable_callback_lines(reachable),
        Vec::<usize>::new(),
        "a reachable declaration was flagged"
    );
}
