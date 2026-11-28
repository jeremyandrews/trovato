//! FR-8 Story 3.5 — the file reference seam: which files an item's field
//! values reference, and the `file_reference` rows and promotion that follow.
//!
//! Items reference uploaded files in two ways:
//!
//! - a **File field** stores the bare `file_managed.id` uuid (a string, or an
//!   array of strings when the field's cardinality is not 1); only the item
//!   type's field definitions say which values those are;
//! - any string may embed the canonical `local://{path}` storage URI or the
//!   `/files/{path}` public URL (block-editor images, rich-text `<img src>`,
//!   page-builder content). Both forms carry the same `{path}`.
//!
//! [`extract_file_references`] collects both, and [`sync_item_file_references`]
//! is the one statement set every item write runs inside its own transaction:
//! replace the item's `file_reference` rows, then promote every referenced
//! `Temporary` file to `Permanent` so the temporary file cleanup cannot delete
//! it. `ItemService` create, update and revert, and config import, all call it.
//!
//! The string scan is shape-agnostic (it recurses through arbitrary nested
//! objects/arrays) and HTML-aware (it scans *within* each string, so an `<img
//! src="/files/…">` inside a body field is found). It indexes only the two
//! forms the application actually produces; a reference stored under a
//! non-default `FILES_URL` (e.g. an absolute CDN URL) is not matched here, and
//! such files are not served through this kernel's `/files/{path}` route
//! either, so there is no serve-path enforcement gap.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use sqlx::PgConnection;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

use crate::file::FileStatus;

/// Characters permitted inside a file storage path. Scanning stops at the first
/// character outside this set — quotes, angle brackets, whitespace, JSON/HTML
/// punctuation — which cleanly bounds a URI embedded in markup or JSON.
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '~')
}

/// Extract every `local://{path}`, `s3://{path}` and `/files/{path}`
/// reference from `s`, pushing each as a normalized URI into `out`.
///
/// `/files/{path}` (the kernel's own proxy route) cannot tell which physical
/// backend served it from the URL alone, so it normalizes to `local://{path}`
/// — matching an S3-backed public file embedded this way is a known gap (see
/// the module doc), traded for not having to guess a backend here.
fn scan_string(s: &str, out: &mut BTreeSet<String>) {
    for (marker, scheme) in [("local://", "local"), ("s3://", "s3"), ("/files/", "local")] {
        let mut rest = s;
        while let Some(pos) = rest.find(marker) {
            let after = &rest[pos + marker.len()..];
            let path: String = after.chars().take_while(|c| is_path_char(*c)).collect();
            // Advance past this marker occurrence regardless of match outcome.
            rest = &after[path.len()..];
            if path.is_empty() || path.starts_with('/') || path.contains("..") {
                continue;
            }
            out.insert(format!("{scheme}://{path}"));
        }
    }
}

fn walk(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(s) => scan_string(s, out),
        serde_json::Value::Array(arr) => arr.iter().for_each(|v| walk(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| walk(v, out)),
        _ => {}
    }
}

/// Return the distinct set of files referenced by an item's `fields`, as
/// normalized `local://{path}` URIs (sorted, deduplicated).
fn extract_file_uris(fields: &serde_json::Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    walk(fields, &mut out);
    out.into_iter().collect()
}

/// The files an item's field values reference: storage URIs found in strings,
/// and `file_managed` ids held by File fields. Either may name a file that
/// does not exist; resolution against `file_managed` drops those.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileReferences {
    /// `local://{path}` or `s3://{path}` URIs, sorted and deduplicated.
    pub uris: Vec<String>,
    /// File field values, sorted and deduplicated.
    pub ids: Vec<Uuid>,
    /// The subset of `ids` held by a field whose `settings.private` is
    /// `true`: [`sync_item_file_references`] promotes these to
    /// `FileVisibility::Private`, in addition to the usual `Permanent`
    /// promotion. Sticky: nothing here ever demotes a file back to public,
    /// since another field could reference the same id unprivately.
    pub private_ids: Vec<Uuid>,
}

/// Every file `fields` references, given the item type's field definitions.
///
/// A File field's value is a uuid string, or an array of them; anything else
/// in a File field (an empty string, a stray object) names no file.
pub(crate) fn extract_file_references(
    fields: &serde_json::Value,
    definitions: &[FieldDefinition],
) -> FileReferences {
    let file_fields: Vec<&FieldDefinition> = definitions
        .iter()
        .filter(|d| matches!(d.field_type, FieldType::File))
        .collect();
    let ids: BTreeSet<Uuid> = file_fields
        .iter()
        .flat_map(|d| file_field_ids(fields.get(&d.field_name)))
        .collect();
    let private_ids: BTreeSet<Uuid> = file_fields
        .iter()
        .filter(|d| d.settings.get("private").and_then(|v| v.as_bool()) == Some(true))
        .flat_map(|d| file_field_ids(fields.get(&d.field_name)))
        .collect();
    FileReferences {
        uris: extract_file_uris(fields),
        ids: ids.into_iter().collect(),
        private_ids: private_ids.into_iter().collect(),
    }
}

/// The file ids one File field's value holds, in the field's order: a uuid
/// string, or an array of them. Anything else names no file.
pub(crate) fn file_field_ids(value: Option<&serde_json::Value>) -> Vec<Uuid> {
    let values: Vec<&serde_json::Value> = match value {
        Some(serde_json::Value::Array(values)) => values.iter().collect(),
        Some(value) => vec![value],
        None => Vec::new(),
    };
    let mut ids: Vec<Uuid> = Vec::new();
    for id in values
        .into_iter()
        .filter_map(|v| v.as_str()?.trim().parse::<Uuid>().ok())
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// The field definitions of `item_type`, read inside the caller's
/// transaction. A type with no row, or no parseable field list, has none, so
/// only the string scan applies to its items.
pub(crate) async fn load_field_definitions(
    conn: &mut PgConnection,
    item_type: &str,
) -> Result<Vec<FieldDefinition>> {
    let raw: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT settings -> 'fields' FROM item_type WHERE type = $1")
            .bind(item_type)
            .fetch_optional(&mut *conn)
            .await
            .context("failed to read the item type's fields")?
            .flatten();
    Ok(match raw {
        Some(serde_json::Value::Array(defs)) => defs
            .into_iter()
            .filter_map(|d| serde_json::from_value(d).ok())
            .collect(),
        _ => Vec::new(),
    })
}

/// Rebuild `item_id`'s `file_reference` rows from `refs` and promote every file
/// the item now references out of `Temporary`, on the caller's connection, so
/// it commits or rolls back with the item write it belongs to.
///
/// Three statements whatever the number of files: delete the item's rows,
/// insert one per managed file matching a URI or an id, promote the referenced
/// temporary files.
pub(crate) async fn sync_item_file_references(
    conn: &mut PgConnection,
    item_id: Uuid,
    refs: &FileReferences,
) -> Result<()> {
    sqlx::query("DELETE FROM file_reference WHERE item_id = $1")
        .bind(item_id)
        .execute(&mut *conn)
        .await
        .context("failed to clear file references")?;
    if refs.uris.is_empty() && refs.ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO file_reference (file_id, item_id) \
         SELECT id, $1 FROM file_managed WHERE uri = ANY($2) OR id = ANY($3) \
         ON CONFLICT DO NOTHING",
    )
    .bind(item_id)
    .bind(&refs.uris)
    .bind(&refs.ids)
    .execute(&mut *conn)
    .await
    .context("failed to insert file references")?;
    sqlx::query(
        "UPDATE file_managed SET status = $2, changed = EXTRACT(EPOCH FROM NOW())::bigint \
         WHERE id IN (SELECT file_id FROM file_reference WHERE item_id = $1) AND status = $3",
    )
    .bind(item_id)
    .bind(FileStatus::Permanent as i16)
    .bind(FileStatus::Temporary as i16)
    .execute(&mut *conn)
    .await
    .context("failed to promote referenced files")?;

    // Fail-closed: a file attached to a private field is marked private
    // immediately, even though its bytes may still sit in the public
    // backend until an operator runs `trovato files migrate` to relocate
    // them. Never reversed here — another field could reference the same
    // file without being private.
    if !refs.private_ids.is_empty() {
        sqlx::query(
            "UPDATE file_managed SET visibility = 1, changed = EXTRACT(EPOCH FROM NOW())::bigint \
             WHERE id = ANY($1) AND visibility != 1",
        )
        .bind(&refs.private_ids)
        .execute(&mut *conn)
        .await
        .context("failed to mark private-field files private")?;
    }
    Ok(())
}

/// Load `item_type`'s field definitions, extract what `fields` references and
/// sync it, all on `conn`: the whole seam for a writer that has not already
/// extracted the references (config import).
pub(crate) async fn sync_item_file_references_for(
    conn: &mut PgConnection,
    item_id: Uuid,
    item_type: &str,
    fields: &serde_json::Value,
) -> Result<()> {
    let definitions = load_field_definitions(conn, item_type).await?;
    let refs = extract_file_references(fields, &definitions);
    sync_item_file_references(conn, item_id, &refs).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_local_uri_from_file_field() {
        let fields = serde_json::json!({
            "field_attachment": { "uri": "local://tenant/2026/02/abc123_report.pdf" }
        });
        assert_eq!(
            extract_file_uris(&fields),
            vec!["local://tenant/2026/02/abc123_report.pdf".to_string()]
        );
    }

    #[test]
    fn extracts_public_url_from_html_body() {
        let fields = serde_json::json!({
            "field_body": {
                "value": "<p>see <img src=\"/files/2026/02/def456_photo.jpg\" alt=\"x\"></p>"
            }
        });
        assert_eq!(
            extract_file_uris(&fields),
            vec!["local://2026/02/def456_photo.jpg".to_string()]
        );
    }

    #[test]
    fn recurses_arrays_and_blocks_and_dedupes() {
        let fields = serde_json::json!({
            "blocks": [
                { "type": "image", "data": { "file": { "url": "/files/2026/02/a_1.png" } } },
                { "type": "image", "data": { "file": { "url": "/files/2026/02/a_1.png" } } },
                { "type": "gallery", "images": [
                    "local://2026/02/b_2.png",
                    "/files/2026/03/c_3.png"
                ]}
            ]
        });
        assert_eq!(
            extract_file_uris(&fields),
            vec![
                "local://2026/02/a_1.png".to_string(),
                "local://2026/02/b_2.png".to_string(),
                "local://2026/03/c_3.png".to_string(),
            ]
        );
    }

    #[test]
    fn ignores_traversal_and_non_file_strings() {
        let fields = serde_json::json!({
            "a": "just some text with no files",
            "b": "https://example.com/page",
            "c": "local://../etc/passwd",
            "d": "/files/../secret",
        });
        assert!(extract_file_uris(&fields).is_empty());
    }

    #[test]
    fn file_field_uuids_are_collected_single_and_array() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let mut gallery = FieldDefinition::new("field_gallery", FieldType::File);
        gallery.cardinality = -1;
        let defs = vec![
            FieldDefinition::new("field_logo", FieldType::File),
            gallery,
            FieldDefinition::new("field_note", FieldType::Text { max_length: None }),
        ];
        let fields = serde_json::json!({
            "field_logo": a.to_string(),
            "field_gallery": [b.to_string(), "", a.to_string()],
            // Not a File field: a uuid here is just text.
            "field_note": Uuid::now_v7().to_string(),
        });
        let refs = extract_file_references(&fields, &defs);
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(refs.ids, expected);
        assert!(refs.uris.is_empty());
    }

    #[test]
    fn empty_or_scalar_fields_yield_nothing() {
        assert!(extract_file_uris(&serde_json::json!({})).is_empty());
        assert!(extract_file_uris(&serde_json::Value::Null).is_empty());
        assert!(extract_file_uris(&serde_json::json!(42)).is_empty());
    }
}
