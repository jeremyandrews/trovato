#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The content type's field list draws each field's index state (B207):
//! queued, building (waiting, copying values, gathering statistics, or a
//! PostgreSQL build phase, with a percentage), ready with its build time,
//! failed, dropping, not indexed, and cannot be indexed.
//!
//! The first version passed the build time to `t()` under the placeholder
//! name `s`, which is `t()`'s own source string argument, so the page failed
//! to render (a 500) as soon as any index was ready. Only the building and
//! queued states had been looked at by then.

mod common;

use serde_json::json;
use trovato_kernel::theme::ThemeEngine;

fn engine() -> ThemeEngine {
    ThemeEngine::new(
        &[common::project_root().join("templates")],
        None,
        std::sync::Arc::new(trovato_kernel::metrics::Metrics::new()),
    )
    .expect("the templates load")
}

fn field(name: &str) -> serde_json::Value {
    json!({
        "field_name": name, "field_type": "Text", "label": name,
        "required": false, "cardinality": 1, "settings": {},
    })
}

fn status(state: &str, phase: Option<&str>, percent: Option<i64>) -> serde_json::Value {
    json!({
        "field_name": "", "kind": "text", "index_name": "item_fx_x",
        "state": state, "error": if state == "failed" { json!("disk full") } else { json!(null) },
        "build_ms": if state == "ready" { json!(1234) } else { json!(null) },
        "phase": phase, "percent": percent,
    })
}

#[test]
fn the_field_list_draws_every_index_state() {
    let rows: Vec<(&str, serde_json::Value, &str)> = vec![
        (
            "ready",
            json!({"available": true, "marked": true, "status": status("ready", None, None)}),
            "built in 1.2 s",
        ),
        (
            "waiting",
            json!({"available": true, "marked": true, "status": status("building", Some("waiting"), None)}),
            "waiting for another index build",
        ),
        (
            "filling",
            json!({"available": true, "marked": true, "status": status("building", Some("filling"), Some(42))}),
            "copying values",
        ),
        (
            "statistics",
            json!({"available": true, "marked": true, "status": status("building", Some("statistics"), None)}),
            "gathering statistics",
        ),
        (
            "pg_phase",
            json!({"available": true, "marked": true,
                   "status": status("building", Some("building index: scanning table"), Some(7))}),
            "building index: scanning table",
        ),
        (
            "failed",
            json!({"available": true, "marked": true, "status": status("failed", None, None)}),
            "disk full",
        ),
        (
            "dropping",
            json!({"available": true, "marked": false, "status": status("dropping", None, None)}),
            "Dropping index",
        ),
        (
            "queued",
            json!({"available": true, "marked": true, "status": null}),
            "Index queued",
        ),
        (
            "unmarked",
            json!({"available": true, "marked": false, "status": null}),
            "Not indexed",
        ),
        (
            "cannot",
            json!({"available": false, "marked": false, "status": null}),
            "Cannot be indexed",
        ),
    ];

    let fields: Vec<_> = rows.iter().map(|(name, _, _)| field(name)).collect();
    let indexes: serde_json::Map<String, serde_json::Value> = rows
        .iter()
        .map(|(name, ix, _)| ((*name).to_string(), ix.clone()))
        .collect();

    let mut context = tera::Context::new();
    context.insert(
        "content_type",
        &json!({"machine_name": "event", "label": "Event", "fields": fields}),
    );
    context.insert("fields", &fields);
    context.insert("field_indexes", &indexes);
    context.insert("field_indexes_building", &true);
    context.insert("add_field_form", "");
    context.insert("path", "/admin/structure/types/event/fields");

    let engine = engine();
    let html = trovato_kernel::i18n::scope("en", || {
        engine.tera().render("admin/field-list.html", &context)
    })
    .unwrap_or_else(|e| {
        let mut chain = format!("{e}");
        let mut source = std::error::Error::source(&e);
        while let Some(s) = source {
            chain.push_str(&format!(": {s}"));
            source = s.source();
        }
        panic!("the field list does not render: {chain}")
    });

    for (name, _, expected) in &rows {
        let at = html
            .find(&format!(r#"<tr data-field="{name}">"#))
            .unwrap_or_else(|| panic!("no row for {name}"));
        let row = &html[at..at + html[at..].find("</tr>").unwrap()];
        assert!(
            row.contains(&expected.replace('\'', "&#x27;")) || row.contains(expected),
            "{name}: expected {expected:?} in:\n{row}"
        );
    }
}
