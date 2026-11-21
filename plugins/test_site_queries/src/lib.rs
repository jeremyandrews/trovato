//! B80 test fixture: a site plugin's six statements with no `raw_sql`.
//!
//! A real site plugin (trovato.rs) held `raw_sql` for six statements: a
//! listing of blog and news posts, a sitemap, a docs index, one doc's markdown
//! by its alias, and a translation sync that listed, read, wrote and deleted
//! `item_translation` rows. This fixture does the same work through the seams
//! B80 added, and declares no `db`, no `db_tables` and no `raw_sql`:
//!
//! | Statement | Here |
//! |---|---|
//! | `LISTING_SQL` | [`listing`]: `query::run`, two types, `created desc`, a limit |
//! | `SITEMAP_SQL` | [`sitemap`]: `query::run`, one type excluded, `changed desc`, paged |
//! | `DOCS_SQL` | [`docs`]: `query::run`, sorted by `fields.weight` then title |
//! | `DOC_MARKDOWN_SQL` | [`doc`]: `query::run`, a filter on `url` |
//! | `TRANSLATION_LIST/READ/DELETE_SQL` | [`translations`]: `list-translations`, `delete-translation` |
//! | `TRANSLATION_WRITE_SQL` | [`translations`]: `save-translation` |
//!
//! The kernel supplies what the SQL spelled out: the viewer's access, the
//! request's stage (the SQL's `stage_id = $1`), the language and each row's
//! `url` (the SQL's `url_alias` join).
//!
//! One driver, `tap_queue_worker`, takes `{"op": ...}` and returns what the op
//! found. Every op takes an optional `run`: a value of `fields.run` the suite
//! gives its own items, so a shared test database's other rows stay out of the
//! answer. It narrows the statement; it is not part of it.

use serde_json::{Map, Value, json};
use trovato_sdk::host;
use trovato_sdk::plugin_tap;

/// Rows per page when an op reads everything (the seam's bound is 100).
const PAGE: u32 = 100;

/// Pages an op reads before it stops, so a runaway result is bounded.
const MAX_PAGES: u32 = 50;

#[plugin_tap]
fn tap_queue_worker(input: Value) -> Value {
    match input.get("op").and_then(Value::as_str) {
        Some("listing") => listing(&input),
        Some("sitemap") => sitemap(&input),
        Some("docs") => docs(&input),
        Some("doc") => doc(&input),
        Some("translations") => translations(&input),
        other => json!({ "error": format!("unknown op {other:?}") }),
    }
}

fn str_of<'a>(input: &'a Value, key: &str, default: &'a str) -> &'a str {
    input.get(key).and_then(Value::as_str).unwrap_or(default)
}

/// The definition every op starts from: published content, narrowed to the
/// suite's run when it gives one.
fn published(input: &Value, mut filters: Vec<Value>) -> Vec<Value> {
    filters.push(json!({"field": "status", "operator": "equals", "value": 1}));
    if let Some(run) = input.get("run").and_then(Value::as_str) {
        filters.push(json!({"field": "fields.run", "operator": "equals", "value": run}));
    }
    filters
}

/// Every page of `definition`, in order, or the host's error code.
fn every_row(definition: &Value) -> Result<(Vec<Value>, u32), i32> {
    let mut rows = Vec::new();
    for page in 1..=MAX_PAGES {
        let result = host::query(definition, &json!({"page": page, "per_page": PAGE}))?;
        rows.extend(result.items);
        if !result.has_next {
            return Ok((rows, page));
        }
    }
    Ok((rows, MAX_PAGES))
}

/// `LISTING_SQL`: the newest posts of the given types, each with its URL.
fn listing(input: &Value) -> Value {
    let types = input
        .get("types")
        .cloned()
        .unwrap_or(json!(["blog", "news"]));
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(10);
    let definition = json!({
        "base_table": "item",
        "filters": published(input, vec![json!({"field": "type", "operator": "in", "value": types})]),
        "fields": [
            {"field_name": "id"}, {"field_name": "title"},
            {"field_name": "type"}, {"field_name": "created"}
        ],
        "sorts": [{"field": "created", "direction": "desc"}]
    });
    match host::query(&definition, &json!({"per_page": limit})) {
        Ok(result) => json!({ "posts": result.items }),
        Err(code) => json!({ "error": code }),
    }
}

/// `SITEMAP_SQL`: every published item but one type, most recently changed
/// first, read a page at a time.
fn sitemap(input: &Value) -> Value {
    let exclude = str_of(input, "exclude", "front_page");
    let definition = json!({
        "base_table": "item",
        "filters": published(input, vec![json!({"field": "type", "operator": "not_equals", "value": exclude})]),
        "fields": [{"field_name": "id"}, {"field_name": "changed"}],
        "sorts": [{"field": "changed", "direction": "desc"}, {"field": "id", "direction": "asc"}]
    });
    match every_row(&definition) {
        Ok((rows, pages)) => json!({ "entries": rows, "pages": pages }),
        Err(code) => json!({ "error": code }),
    }
}

/// `DOCS_SQL`: the docs index, by weight (unweighted last) then title. A doc
/// with no alias has nowhere to be listed under `/learn`, so it is skipped,
/// which is what the statement's inner join did.
fn docs(input: &Value) -> Value {
    let doc_type = str_of(input, "type", "docs");
    let definition = json!({
        "base_table": "item",
        "filters": published(input, vec![json!({"field": "type", "operator": "equals", "value": doc_type})]),
        "fields": [
            {"field_name": "id"}, {"field_name": "title"},
            {"field_name": "fields.weight", "label": "weight"}
        ],
        "sorts": [
            {"field": "fields.weight", "direction": "asc", "nulls": "last"},
            {"field": "title", "direction": "asc"}
        ]
    });
    match every_row(&definition) {
        Ok((rows, _)) => {
            let listed: Vec<Value> = rows
                .into_iter()
                .filter(|row| {
                    row.get("url")
                        .and_then(Value::as_str)
                        .is_some_and(|url| !url.starts_with("/item/"))
                })
                .collect();
            json!({ "docs": listed })
        }
        Err(code) => json!({ "error": code }),
    }
}

/// `DOC_MARKDOWN_SQL`: one doc's markdown, found by the address it is served
/// at.
fn doc(input: &Value) -> Value {
    let doc_type = str_of(input, "type", "docs");
    let slug = str_of(input, "slug", "");
    let definition = json!({
        "base_table": "item",
        "filters": published(input, vec![
            json!({"field": "type", "operator": "equals", "value": doc_type}),
            json!({"field": "url", "operator": "equals", "value": format!("/learn/{slug}")})
        ]),
        "fields": [{"field_name": "fields.markdown", "label": "markdown"}]
    });
    match host::query(&definition, &json!({"per_page": 1})) {
        Ok(result) => {
            let markdown = result
                .items
                .first()
                .and_then(|row| row.get("markdown"))
                .cloned()
                .unwrap_or(Value::Null);
            json!({ "markdown": markdown })
        }
        Err(code) => json!({ "error": code }),
    }
}

/// The translation sync: make the stored translations of one type's items
/// match `declared`, `{item_id: {language: {"title", "fields"}}}`.
///
/// One `query::run` names the items, one `list-translations` reads every stored
/// translation of them (the statement read one at a time), and each difference
/// is one `save-translation` or `delete-translation`. A translation already as
/// declared is not written, so its `changed` does not move.
fn translations(input: &Value) -> Value {
    let item_type = str_of(input, "type", "docs");
    let empty = Map::new();
    let declared = input
        .get("declared")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let definition = json!({
        "base_table": "item",
        "filters": published(input, vec![json!({"field": "type", "operator": "equals", "value": item_type})]),
        "fields": [{"field_name": "id"}],
        "sorts": [{"field": "id", "direction": "asc"}]
    });
    let ids: Vec<String> = match host::query(&definition, &json!({"per_page": PAGE})) {
        Ok(result) => result
            .items
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
            .collect(),
        Err(code) => return json!({ "error": code, "at": "query" }),
    };
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let stored = match host::list_translations(&id_refs) {
        Ok(stored) => stored,
        Err(code) => return json!({ "error": code, "at": "list" }),
    };

    let (mut written, mut unchanged, mut removed) = (0u32, 0u32, 0u32);
    let mut errors = Vec::new();
    for (item_id, languages) in declared {
        for (language, wanted) in languages.as_object().into_iter().flatten() {
            let title = wanted.get("title").and_then(Value::as_str).unwrap_or("");
            let fields = wanted.get("fields").cloned().unwrap_or_else(|| json!({}));
            let same = stored.iter().any(|t| {
                t.item_id.to_string() == *item_id
                    && t.language == *language
                    && t.title == title
                    && t.fields == fields
            });
            if same {
                unchanged += 1;
                continue;
            }
            let translation = json!({
                "item_id": item_id, "language": language, "title": title, "fields": fields
            });
            match host::save_translation(&translation) {
                Ok(_) => written += 1,
                Err(code) => {
                    errors.push(json!({"save": format!("{item_id}:{language}"), "code": code}));
                }
            }
        }
    }
    for t in &stored {
        let item_id = t.item_id.to_string();
        let kept = declared
            .get(&item_id)
            .and_then(|languages| languages.get(&t.language))
            .is_some();
        if kept {
            continue;
        }
        match host::delete_translation(&item_id, &t.language) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(code) => {
                errors.push(json!({"delete": format!("{item_id}:{}", t.language), "code": code}));
            }
        }
    }
    json!({
        "items": ids.len(),
        "stored": stored.len(),
        "written": written,
        "unchanged": unchanged,
        "removed": removed,
        "errors": errors
    })
}
