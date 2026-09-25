#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The project version is authored once. This is the test that keeps every copy
//! of it equal to the authored one.
//!
//! Trovato has one version number, `[workspace.package] version`. Most of the
//! tree derives it and cannot drift: every crate inherits it, `KERNEL_API_VERSION`
//! is parsed from it at compile time, and the publish workflow reads it out of
//! `Cargo.toml`. What is left are the plugin manifests, which the loader reads at
//! run time and which therefore hold literal strings, and the documentation that
//! states the current release.
//!
//! `scripts/sync-version.sh` writes both. This test is what makes that safe: it
//! walks every manifest and every generated block and names the first one that
//! disagrees, so the dozens of literal strings on disk stop being a risk even
//! though they remain literal strings on disk.
//!
//! Historically this was a fifteen row checklist in `docs/design/version-map.md`,
//! executed by hand, and two of those rows were "every one of the manifests".

use std::path::{Path, PathBuf};

/// The repository root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("the repository root resolves")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()))
}

/// The authored version: this crate inherits `[workspace.package] version`, so
/// cargo hands it to us and the test never parses `Cargo.toml` for it.
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The plugin API version, which is the project version without its patch.
fn api_version() -> String {
    let (major, minor) = trovato_kernel::plugin::KERNEL_API_VERSION;
    format!("{major}.{minor}")
}

/// Every `*.info.toml` under `plugins/`, as (display path, contents).
fn manifests() -> Vec<(String, String)> {
    let plugins = repo_root().join("plugins");
    let mut out = Vec::new();
    let mut stack = vec![plugins];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.to_string_lossy().ends_with(".info.toml")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                let name = path
                    .strip_prefix(repo_root())
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                out.push((name, text));
            }
        }
    }
    out.sort();
    out
}

/// The value of a bare `key = "value"` line, if the file has one.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key} = \"");
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&prefix)?.strip_suffix('"'))
}

#[test]
fn every_plugin_manifest_declares_the_project_version() {
    let found = manifests();
    assert!(
        found.len() >= 30,
        "expected to find the plugin manifests; found {}",
        found.len()
    );

    let expected_api = api_version();
    let mut failures = Vec::new();
    for (name, text) in &found {
        match field(text, "version") {
            Some(v) if v == version() => {}
            Some(v) => failures.push(format!(
                "{name}: version is \"{v}\", expected \"{}\"",
                version()
            )),
            None => failures.push(format!("{name}: declares no version")),
        }
        match field(text, "api_version") {
            Some(v) if v == expected_api => {}
            Some(v) => failures.push(format!(
                "{name}: api_version is \"{v}\", expected \"{expected_api}\""
            )),
            None => failures.push(format!("{name}: declares no api_version")),
        }
    }

    assert!(
        failures.is_empty(),
        "{} manifest field(s) disagree with the project version; \
         run scripts/sync-version.sh:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The contents of each `<!-- version:begin -->` block in a file, in order.
fn generated_blocks(relative: &str) -> Vec<String> {
    let text = read(relative);
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some((_, after)) = rest.split_once("<!-- version:begin -->") {
        let Some((block, tail)) = after.split_once("<!-- version:end -->") else {
            panic!("{relative} has an unclosed version:begin marker");
        };
        out.push(block.to_string());
        rest = tail;
    }
    out
}

#[test]
fn every_generated_documentation_block_names_the_current_version() {
    // Each file and how many generated blocks it carries. The count is asserted
    // so that deleting a marker pair fails here rather than silently removing a
    // block from the script's reach.
    //
    // Most blocks state the full release. One does not: the compatibility table
    // in Versioning.md is about API versions, so it names the API version and
    // would be wrong if it named a patch. Each block is therefore checked
    // against the string it is supposed to contain.
    let api = api_version();
    let expected: &[(&str, usize, &str)] = &[
        ("docs/design/Versioning.md", 5, version()),
        ("docs/design/version-map.md", 1, version()),
        ("README.md", 1, version()),
        ("ROADMAP.md", 1, version()),
        ("CONTRIBUTING.md", 1, version()),
        ("KNOWN-ISSUES.md", 1, version()),
        ("docs/RELEASING.md", 1, version()),
        ("SECURITY.md", 1, version()),
    ];

    let mut failures = Vec::new();
    for (file, count, needle) in expected {
        let blocks = generated_blocks(file);
        if blocks.len() != *count {
            failures.push(format!(
                "{file}: has {} generated block(s), expected {count}",
                blocks.len()
            ));
            continue;
        }
        for (i, block) in blocks.iter().enumerate() {
            // A block states either the full release or, where it is about the
            // plugin API alone, the API version. Either way a stale number fails.
            if !block.contains(needle) && !block.contains(&api) {
                failures.push(format!(
                    "{file}: generated block {} names neither {needle} nor {api}",
                    i + 1
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "generated documentation disagrees with the project version; \
         run scripts/sync-version.sh:\n{}",
        failures.join("\n")
    );
}

/// The issue template is YAML, so it carries no marker: the line is matched and
/// rewritten in place by the script, and checked here like the rest.
#[test]
fn the_issue_template_names_the_current_version() {
    let config = read(".github/ISSUE_TEMPLATE/config.yml");
    let expected = format!("outstanding in {}, before you file it.", version());
    assert!(
        config.contains(&expected),
        ".github/ISSUE_TEMPLATE/config.yml does not say `{expected}`; \
         run scripts/sync-version.sh"
    );
}

/// The API tuple is the project version with the patch component dropped. It is
/// derived at compile time, so this asserts the derivation rather than a value.
#[test]
fn the_plugin_api_version_is_the_project_version_without_its_patch() {
    let (major, minor) = trovato_kernel::plugin::KERNEL_API_VERSION;
    let expected = version()
        .rsplit_once('.')
        .map(|(head, _patch)| head.to_string())
        .expect("the project version has three components");
    assert_eq!(
        format!("{major}.{minor}"),
        expected,
        "KERNEL_API_VERSION does not follow the project version {}",
        version()
    );
}
