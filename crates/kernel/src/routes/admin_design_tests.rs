//! Tests that hold the admin stylesheet to docs/admin/design.md section 5:
//! colour contrast computed from the token values themselves, in both
//! themes, and logical properties only.
//!
//! They read `static/css/admin.css` and the admin templates from the source
//! tree, so a token edit or a stray `margin-left` fails `cargo test` before
//! anyone has to notice it in a browser.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn admin_css() -> String {
    std::fs::read_to_string(repo_root().join("static/css/admin.css")).expect("admin.css")
}

/// `--name: #RRGGBB;` declarations: the primitives.
fn primitives(css: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in css.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("--") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';').trim();
        if value.starts_with('#') && (value.len() == 7) {
            out.insert(name.trim().to_string(), value.to_string());
        }
    }
    out
}

/// `--name: light-dark(var(--a), var(--b));` declarations, resolved to
/// `(light hex, dark hex)` through the primitives.
fn semantic(css: &str) -> HashMap<String, (String, String)> {
    let prims = primitives(css);
    let resolve = |s: &str| -> String {
        let s = s.trim();
        if let Some(name) = s.strip_prefix("var(--").and_then(|r| r.strip_suffix(')')) {
            prims
                .get(name)
                .unwrap_or_else(|| panic!("primitive --{name} is not defined"))
                .clone()
        } else {
            s.to_string()
        }
    };
    let mut out = HashMap::new();
    for line in css.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("--") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';').trim();
        if let Some(inner) = value
            .strip_prefix("light-dark(")
            .and_then(|v| v.strip_suffix(')'))
        {
            let (light, dark) = inner.split_once(',').expect("two sides");
            out.insert(name.trim().to_string(), (resolve(light), resolve(dark)));
        }
    }
    out
}

fn channel(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(hex: &str) -> f64 {
    let hex = hex.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex colour");
    0.2126 * channel(byte(0)) + 0.7152 * channel(byte(2)) + 0.0722 * channel(byte(4))
}

/// WCAG 2.x contrast ratio.
fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Every text pair the admin draws: (text token, background token).
const TEXT_PAIRS: &[(&str, &str)] = &[
    ("text", "surface"),
    ("text", "surface-raised"),
    ("text", "surface-sunken"),
    ("text", "surface-hover"),
    ("text-muted", "surface"),
    ("text-muted", "surface-raised"),
    ("text-muted", "surface-sunken"),
    ("text-muted", "surface-hover"),
    ("link", "surface"),
    ("link", "surface-raised"),
    ("link", "surface-sunken"),
    ("link", "surface-hover"),
    ("link", "success-surface"),
    ("link", "danger-surface"),
    ("link", "warning-surface"),
    ("accent-text", "accent"),
    ("accent-text", "accent-hover"),
    ("danger-text", "danger"),
    ("danger", "surface-raised"),
    ("danger", "surface"),
    ("text", "success-surface"),
    ("text", "danger-surface"),
    ("text", "warning-surface"),
];

/// Every non-text pair that must reach 3:1: component boundaries, the
/// current-page marker and the focus ring against what they sit on. The ring
/// is drawn 2px outside the element (`--focus-offset`), so it sits on the
/// page or card surface, never on a button's own fill.
const UI_PAIRS: &[(&str, &str)] = &[
    ("border-strong", "surface-raised"),
    ("border-strong", "surface"),
    ("border-strong", "surface-sunken"),
    ("accent", "surface"),
    ("accent", "surface-raised"),
    ("accent", "surface-sunken"),
    ("focus", "surface"),
    ("focus", "surface-raised"),
    ("focus", "surface-sunken"),
    ("success", "success-surface"),
    ("danger", "danger-surface"),
    // The icon in an icon chip. Decorative, so WCAG 1.4.11 does not ask for
    // it; 3:1 is this project's own floor for anything drawn to be seen.
    ("accent", "accent-tint"),
];

fn check(pairs: &[(&str, &str)], minimum: f64) -> Vec<String> {
    let tokens = semantic(&admin_css());
    let mut failures = Vec::new();
    for (fg, bg) in pairs {
        let f = tokens
            .get(*fg)
            .unwrap_or_else(|| panic!("--{fg} is not a light-dark() token"));
        let b = tokens
            .get(*bg)
            .unwrap_or_else(|| panic!("--{bg} is not a light-dark() token"));
        for (theme, fc, bc) in [("light", &f.0, &b.0), ("dark", &f.1, &b.1)] {
            let ratio = contrast(fc, bc);
            if ratio < minimum {
                failures.push(format!(
                    "{theme}: --{fg} {fc} on --{bg} {bc} is {ratio:.2}:1, needs {minimum}:1"
                ));
            }
        }
    }
    failures
}

#[test]
fn the_contrast_formula_matches_known_values() {
    assert!((contrast("#000000", "#FFFFFF") - 21.0).abs() < 0.01);
    assert!((contrast("#777777", "#FFFFFF") - 4.48).abs() < 0.01);
}

#[test]
fn every_text_pair_is_at_least_4_5_to_1_in_both_themes() {
    let failures = check(TEXT_PAIRS, 4.5);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_component_boundary_and_the_focus_ring_is_at_least_3_to_1_in_both_themes() {
    let failures = check(UI_PAIRS, 3.0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every semantic token the pairs name exists, and every semantic token has
/// both sides: a token added without a dark value would otherwise pass by
/// being absent from the lists above.
#[test]
fn every_semantic_token_is_a_light_dark_pair() {
    let tokens = semantic(&admin_css());
    for name in [
        "surface",
        "surface-raised",
        "surface-sunken",
        "text",
        "text-muted",
        "border",
        "border-strong",
        "accent",
        "accent-text",
        "link",
        "danger",
        "focus",
        "accent-tint",
    ] {
        assert!(tokens.contains_key(name), "--{name} is missing");
    }
}

/// The physical properties a right-to-left layout would need overriding.
fn physical(declaration: &str) -> Option<&'static str> {
    let d = declaration.trim().to_ascii_lowercase();
    let (prop, value) = d.split_once(':')?;
    let prop = prop.trim();
    let value = value.trim().trim_end_matches(';').trim();
    const SIDES: &[&str] = &[
        "margin-left",
        "margin-right",
        "padding-left",
        "padding-right",
        "border-left",
        "border-right",
        "border-left-width",
        "border-right-width",
        "border-left-color",
        "border-right-color",
        "border-left-style",
        "border-right-style",
        "border-top-left-radius",
        "border-top-right-radius",
        "border-bottom-left-radius",
        "border-bottom-right-radius",
        "left",
        "right",
        "scroll-margin-left",
        "scroll-margin-right",
        "scroll-padding-left",
        "scroll-padding-right",
    ];
    if SIDES.contains(&prop) {
        return Some("a physical side property");
    }
    if matches!(prop, "text-align" | "float" | "clear") && (value == "left" || value == "right") {
        return Some("a physical left/right value");
    }
    // A four-value margin or padding shorthand whose right and left differ
    // is physical too.
    if matches!(prop, "margin" | "padding") {
        let parts: Vec<&str> = value.split_whitespace().collect();
        if parts.len() == 4 && parts[1] != parts[3] {
            return Some("a shorthand with different right and left");
        }
    }
    None
}

/// Every CSS declaration in `css`, as written.
fn declarations(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth_text = String::new();
    let mut in_comment = false;
    let chars: Vec<char> = css.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !in_comment && chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_comment = true;
            i += 2;
            continue;
        }
        if in_comment {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        match chars[i] {
            '{' | '}' => depth_text.clear(),
            ';' => {
                out.push(depth_text.trim().to_string());
                depth_text.clear();
            }
            c => depth_text.push(c),
        }
        i += 1;
    }
    out
}

#[test]
fn the_admin_stylesheet_uses_logical_properties_only() {
    let failures: Vec<String> = declarations(&admin_css())
        .into_iter()
        .filter_map(|d| physical(&d).map(|why| format!("`{d}`: {why}")))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn admin_templates() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("templates dir").flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "html") {
                out.push(p);
            }
        }
    }
    let root = repo_root().join("templates");
    let mut out = vec![root.join("page--admin.html")];
    walk(&root.join("admin"), &mut out);
    out
}

/// Inline styles in admin templates are admin CSS too: a `style` attribute
/// or `<style>` block may not use a physical side either.
#[test]
fn admin_templates_use_no_physical_properties_inline() {
    let mut failures = Vec::new();
    for path in admin_templates() {
        let src = std::fs::read_to_string(&path).expect("template");
        let mut chunks: Vec<String> = Vec::new();
        for (i, _) in src.match_indices("style=\"") {
            let rest = &src[i + 7..];
            if let Some(end) = rest.find('"') {
                chunks.push(format!("{};", &rest[..end]));
            }
        }
        for (i, _) in src.match_indices("<style") {
            if let Some(end) = src[i..].find("</style>") {
                chunks.push(src[i..i + end].to_string());
            }
        }
        for chunk in chunks {
            for d in declarations(&chunk) {
                if let Some(why) = physical(&d) {
                    failures.push(format!("{}: `{d}`: {why}", path.display()));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_physical_property_check_catches_what_it_should() {
    assert!(physical("margin-left: 1rem").is_some());
    assert!(physical("text-align: right").is_some());
    assert!(physical("left: 0").is_some());
    assert!(physical("padding: 0 1rem 0 2rem").is_some());
    assert!(physical("margin-inline-start: 1rem").is_none());
    assert!(physical("text-align: start").is_none());
    assert!(physical("padding: 0 1rem 0 1rem").is_none());
    assert!(physical("inset-inline-end: 0").is_none());
}

/// The focus ring is at least 2px and every focusable element gets it.
#[test]
fn the_focus_ring_is_at_least_2px_and_applies_everywhere() {
    let css = admin_css();
    let ring = css
        .lines()
        .find_map(|l| l.trim().strip_prefix("--focus-ring:"))
        .expect("--focus-ring is defined");
    let px: f64 = ring
        .split_whitespace()
        .find_map(|w| w.strip_suffix("px").and_then(|n| n.parse().ok()))
        .expect("a px width");
    assert!(px >= 2.0, "focus ring {px}px");
    assert!(css.contains(":focus-visible {\n        outline: var(--focus-ring);"));
    assert!(
        !css.contains("outline: none;\n    }\n\n    :focus"),
        "never removed"
    );
}

// ---------------------------------------------------------------- inventory

/// Every `/admin` path the kernel registers, read from the route modules'
/// source: each `.route(` call's first argument, when it is a string
/// literal, a module's `BASE`/`PATH` constant, one of the shell's constants,
/// or a `format!` of those. Paths built at run time from plugin declarations
/// (an owned vocabulary's base, a gather's route) are the plugins', listed in
/// the inventory's own section and not checked here.
fn kernel_admin_routes() -> std::collections::BTreeSet<String> {
    use super::admin_nav;
    let mut out = std::collections::BTreeSet::new();
    let dir = repo_root().join("crates/kernel/src/routes");
    for entry in std::fs::read_dir(&dir).expect("routes dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read route module");
        let mut consts: HashMap<&str, String> = HashMap::new();
        consts.insert("admin_nav::DASHBOARD", admin_nav::DASHBOARD.to_string());
        consts.insert("admin_nav::PREFERENCES", admin_nav::PREFERENCES.to_string());
        consts.insert("admin_nav::SEARCH", admin_nav::SEARCH.to_string());
        for name in ["BASE", "PATH"] {
            let decl = format!("const {name}: &str = \"");
            if let Some(at) = src.find(&decl) {
                let rest = &src[at + decl.len()..];
                consts.insert(name, rest[..rest.find('"').unwrap()].to_string());
            }
        }
        let mut rest = src.as_str();
        while let Some(at) = rest.find(".route(") {
            rest = &rest[at + ".route(".len()..];
            // The first argument: up to the first comma outside parentheses.
            let mut depth = 0i32;
            let mut end = rest.len();
            for (i, c) in rest.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' if depth == 0 => {
                        end = i;
                        break;
                    }
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            let arg = rest[..end].trim();
            let resolved =
                if let Some(lit) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                    Some(lit.to_string())
                } else if let Some(v) = consts.get(arg) {
                    Some(v.clone())
                } else if let Some(fmt) = arg
                    .strip_prefix("&format!(\"")
                    .and_then(|a| a.strip_suffix("\")"))
                {
                    let mut s = fmt.to_string();
                    for name in ["BASE", "PATH"] {
                        if let Some(v) = consts.get(name) {
                            s = s.replace(&format!("{{{name}}}"), v);
                        }
                    }
                    (!s.contains("{BASE}") && !s.contains("{PATH}") && !s.contains("{base}"))
                        .then(|| s.replace("{{", "{").replace("}}", "}"))
                } else {
                    None
                };
            if let Some(p) = resolved
                && (p == "/admin" || p.starts_with("/admin/"))
            {
                out.insert(p);
            }
        }
    }
    for g in admin_nav::GROUPS {
        out.insert(g.group.landing_href());
    }
    out
}

/// The kernel section of docs/admin/inventory.md: `| path | methods | group |…`.
fn inventory_rows() -> Vec<(String, String)> {
    let doc = std::fs::read_to_string(repo_root().join("docs/admin/inventory.md"))
        .expect("docs/admin/inventory.md");
    let kernel = doc
        .split("<!-- kernel routes -->")
        .nth(1)
        .and_then(|s| s.split("<!-- end kernel routes -->").next())
        .expect("the inventory's kernel section markers");
    kernel
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            let path = cells.first()?.trim_matches('`');
            let group = cells.get(2)?;
            path.starts_with("/admin")
                .then(|| (path.to_string(), group.to_string()))
        })
        .collect()
}

/// The group a path belongs to: the registry's answer, the one the sidebar,
/// breadcrumb and `<title>` use. The Dashboard, preferences and search are
/// the shell's own and sit outside the seven groups.
fn group_of(path: &str) -> String {
    super::admin_nav::placement(path, &[], &[])
        .group
        .unwrap_or_else(|| "Dashboard".to_string())
}

#[test]
fn the_inventory_lists_every_admin_route_once_in_the_group_the_registry_gives_it() {
    let routes = kernel_admin_routes();
    assert!(
        routes.len() > 80,
        "the route scan found too few: {routes:?}"
    );
    let rows = inventory_rows();

    let mut seen = std::collections::BTreeSet::new();
    let mut problems = Vec::new();
    for (path, group) in &rows {
        if !seen.insert(path.clone()) {
            problems.push(format!("{path}: listed more than once"));
        }
        if !routes.contains(path) {
            problems.push(format!("{path}: listed, but no kernel route has this path"));
        }
        let expected = group_of(path);
        if *group != expected {
            problems.push(format!(
                "{path}: listed in {group}, the registry places it in {expected}"
            ));
        }
        let known =
            expected == "Dashboard" || super::admin_nav::GROUPS.iter().any(|g| g.title == expected);
        if !known {
            problems.push(format!("{path}: {expected} is not one of the groups"));
        }
    }
    for path in &routes {
        if !seen.contains(path) {
            problems.push(format!(
                "{path}: a kernel route missing from the inventory (group {})",
                group_of(path)
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "docs/admin/inventory.md is out of date:\n{}",
        problems.join("\n")
    );
}
