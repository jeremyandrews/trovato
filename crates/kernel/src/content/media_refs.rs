//! B290 — the item reference seam: which items an item's `RecordReference`
//! field values point at, and the `item_reference` rows that follow.
//!
//! An item references another item by holding its uuid (or an array of them)
//! in a field of type `FieldType::RecordReference(_)` — `CategoryReference`
//! is a different field type and is never touched here. This is the same
//! shape [`crate::content::file_refs`] already indexes for File fields, kept
//! as its own table because a file and an item are different things a viewer
//! can delete: a media item's "is this still in use" question is answered
//! from `item_reference`, the same way a file's is answered from
//! `file_reference`.
//!
//! [`extract_media_references`] collects every reference an item's fields
//! hold, and [`sync_item_media_references`] is the statement pair every item
//! write runs after its own transaction commits (see
//! [`crate::content::item_service::ItemService`]'s create and update paths):
//! replace the item's `item_reference` rows. Unlike file references this is
//! not run inside the item's own transaction — a failure here does not
//! unwrite the item, it only leaves its usage rows stale until the next
//! save, which is acceptable for a count used to warn an editor, not to
//! enforce anything security sensitive.

use anyhow::{Context, Result};
use sqlx::{PgConnection, PgPool};
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

use super::file_refs::file_field_ids;

/// How many items currently reference `item_id` through a `RecordReference`
/// field. Used by the media library to show a usage count and to refuse a
/// delete the editor has not confirmed with `force`.
pub(crate) async fn usage_count(pool: &PgPool, item_id: Uuid) -> Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM item_reference WHERE referenced_item_id = $1")
        .bind(item_id)
        .fetch_one(pool)
        .await
        .context("failed to count item references")
}

/// One `RecordReference` field's value, as the field name and the referenced
/// item ids it names, in the field's order and deduplicated.
pub(crate) fn extract_media_references(
    fields: &serde_json::Value,
    definitions: &[FieldDefinition],
) -> Vec<(String, Uuid)> {
    let mut out = Vec::new();
    for def in definitions {
        if !matches!(def.field_type, FieldType::RecordReference(_)) {
            continue;
        }
        // A `RecordReference` value is a bare uuid string, or an array of
        // them — the exact shape a File field's value takes, so the same
        // parser applies.
        for id in file_field_ids(fields.get(&def.field_name)) {
            out.push((def.field_name.clone(), id));
        }
    }
    out
}

/// Rebuild `item_id`'s `item_reference` rows from `refs`: delete the item's
/// rows, then insert one per reference naming an item that still exists. Logs
/// and returns on a failed insert rather than leaving a partial delete+insert
/// — the caller runs this best-effort, outside the item's own transaction.
pub(crate) async fn sync_item_media_references(
    conn: &mut PgConnection,
    item_id: Uuid,
    refs: &[(String, Uuid)],
) -> Result<()> {
    sqlx::query("DELETE FROM item_reference WHERE referencing_item_id = $1")
        .bind(item_id)
        .execute(&mut *conn)
        .await
        .context("failed to clear item references")?;
    if refs.is_empty() {
        return Ok(());
    }
    let field_names: Vec<&str> = refs.iter().map(|(f, _)| f.as_str()).collect();
    let referenced_ids: Vec<Uuid> = refs.iter().map(|(_, id)| *id).collect();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item_reference (referencing_item_id, referenced_item_id, field_name, created) \
         SELECT $1, r.referenced_id, r.field_name, $4 \
         FROM UNNEST($2::uuid[], $3::text[]) AS r(referenced_id, field_name) \
         JOIN item ON item.id = r.referenced_id \
         ON CONFLICT DO NOTHING",
    )
    .bind(item_id)
    .bind(&referenced_ids)
    .bind(&field_names)
    .bind(now)
    .execute(&mut *conn)
    .await
    .context("failed to insert item references")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_ref(name: &str, target: &str) -> FieldDefinition {
        FieldDefinition::new(name, FieldType::RecordReference(target.into()))
    }

    #[test]
    fn extracts_single_and_array_values() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let mut gallery = record_ref("field_gallery", "media");
        gallery.cardinality = -1;
        let defs = vec![
            record_ref("field_hero", "media"),
            gallery,
            FieldDefinition::new("field_note", FieldType::Text { max_length: None }),
        ];
        let fields = serde_json::json!({
            "field_hero": a.to_string(),
            "field_gallery": [b.to_string(), "", a.to_string()],
            "field_note": Uuid::now_v7().to_string(),
        });
        let refs = extract_media_references(&fields, &defs);
        assert_eq!(
            refs,
            vec![
                ("field_hero".to_string(), a),
                ("field_gallery".to_string(), b),
                ("field_gallery".to_string(), a),
            ]
        );
    }

    #[test]
    fn category_reference_is_not_a_record_reference() {
        let defs = vec![FieldDefinition::new(
            "field_topics",
            FieldType::CategoryReference("topics".into()),
        )];
        let fields = serde_json::json!({ "field_topics": [Uuid::now_v7().to_string()] });
        assert!(extract_media_references(&fields, &defs).is_empty());
    }

    #[test]
    fn empty_fields_yield_nothing() {
        assert!(extract_media_references(&serde_json::json!({}), &[]).is_empty());
    }
}
