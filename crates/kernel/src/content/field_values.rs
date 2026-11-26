//! The one check an item's field values pass on every write: how many values
//! each field holds against its `cardinality`, and, for a `CategoryReference`
//! field, that every value names a tag of the field's category.
//!
//! Every write path reaches it: `ItemService` create and update (the admin
//! content form, the item routes and their JSON API, and plugin `save-item`
//! all write through the service), the item form's submission builder (for
//! the count, so the form shows the error beside the field), and config
//! import's reference pass, which checks a whole set before writing any of it.
//! It validates writes only: stored data is never rewritten, and on an update a
//! field whose value the write leaves as it was is not judged again, so an
//! item saved before a field's cardinality was lowered stays editable.
//!
//! The check itself ([`check_field_values`]) is pure. The tag lookup it needs
//! is one query for the whole set ([`tag_categories`]); config import answers
//! it from the set it is importing plus that one query.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgConnection;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

/// One refused field value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldValueError {
    /// The field's machine name.
    pub field: String,
    /// What is wrong, in words for the person who wrote it.
    pub message: String,
}

/// A write refused by [`check_field_values`]: every refused value at once.
///
/// Returned inside `anyhow::Error`; callers find it with `downcast_ref` and
/// answer 422 (item API), a form error (admin form), or
/// `ERR_ITEM_INVALID_FIELDS` (plugin `save-item`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidFieldValues(pub Vec<FieldValueError>);

impl std::fmt::Display for InvalidFieldValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let messages: Vec<&str> = self.0.iter().map(|e| e.message.as_str()).collect();
        f.write_str(&messages.join(" "))
    }
}

impl std::error::Error for InvalidFieldValues {}

/// Whether `field` stores a list of values rather than one.
///
/// A field whose cardinality is not 1 does, and a `CategoryReference` field
/// always does: its value is the array of tag uuids gather's tag operators
/// read, whatever the cardinality. The item form renders such a field as the
/// repeating widget and posts it back as an array.
pub fn stores_a_list(field: &FieldDefinition) -> bool {
    field.cardinality != 1 || matches!(field.field_type, FieldType::CategoryReference(_))
}

/// The refusal for `count` values in `field`, or `None` when its cardinality
/// allows that many. `-1` (or any negative) allows any number.
pub fn too_many_values(field: &FieldDefinition, count: usize) -> Option<String> {
    let max = usize::try_from(field.cardinality).ok()?;
    (count > max).then(|| {
        if max == 1 {
            format!("{} allows at most 1 value.", field.label)
        } else {
            format!("{} allows at most {max} values.", field.label)
        }
    })
}

/// Field kinds whose single value is itself a structure (a section list, a
/// block array, a component tree): their JSON shape says nothing about how many
/// values the field holds, so cardinality does not apply.
fn has_structured_value(field: &FieldDefinition) -> bool {
    matches!(
        field.field_type,
        FieldType::Compound { .. } | FieldType::Blocks | FieldType::PageBuilder
    )
}

/// How many values a stored field value holds.
fn value_count(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Array(values) => values.len(),
        _ => 1,
    }
}

/// Whether the write changes `name` from what `previous` stored.
fn is_written(name: &str, fields: &Value, previous: Option<&Value>) -> bool {
    previous.is_none_or(|p| p.get(name) != fields.get(name))
}

/// The tag uuids the `CategoryReference` fields of `fields` name, for one
/// lookup. Values that are not uuids are left to [`check_field_values`].
pub fn category_tag_ids(
    fields: &Value,
    definitions: &[FieldDefinition],
    previous: Option<&Value>,
) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = definitions
        .iter()
        .filter(|d| matches!(d.field_type, FieldType::CategoryReference(_)))
        .filter(|d| is_written(&d.field_name, fields, previous))
        .filter_map(|d| fields.get(&d.field_name)?.as_array())
        .flatten()
        .filter_map(|v| v.as_str()?.parse::<Uuid>().ok())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Check `fields` against `definitions`: every refused value, or none.
///
/// `tag_categories` maps each tag uuid [`category_tag_ids`] returned to its
/// category; a uuid missing from it names no tag. `previous` is the stored
/// field map on an update: a field the write leaves unchanged is not checked.
pub fn check_field_values(
    fields: &Value,
    definitions: &[FieldDefinition],
    previous: Option<&Value>,
    tag_categories: &HashMap<Uuid, String>,
) -> Vec<FieldValueError> {
    let mut errors = Vec::new();
    for field in definitions {
        let name = &field.field_name;
        let Some(value) = fields.get(name) else {
            continue;
        };
        if !is_written(name, fields, previous) {
            continue;
        }
        let refuse = |message: String| FieldValueError {
            field: name.clone(),
            message,
        };

        if !has_structured_value(field)
            && let Some(message) = too_many_values(field, value_count(value))
        {
            errors.push(refuse(message));
        }

        let FieldType::CategoryReference(category) = &field.field_type else {
            continue;
        };
        let values = match value {
            Value::Null => continue,
            Value::Array(values) => values,
            _ => {
                errors.push(refuse(format!(
                    "{} holds a list of tag ids, not a single value.",
                    field.label
                )));
                continue;
            }
        };
        for v in values {
            let tag = v.as_str().and_then(|s| s.parse::<Uuid>().ok());
            if tag.and_then(|id| tag_categories.get(&id)) != Some(category) {
                let shown = v.as_str().map_or_else(|| v.to_string(), str::to_string);
                errors.push(refuse(format!(
                    "{}: {shown} is not a tag in the '{category}' category.",
                    field.label
                )));
            }
        }
    }
    errors
}

/// The category of each tag in `ids`, in one query.
pub async fn tag_categories(
    conn: &mut PgConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, category_id FROM category_tag WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&mut *conn)
            .await
            .context("failed to read the categories of the tags a write names")?;
    Ok(rows.into_iter().collect())
}

/// Run [`check_field_values`] for one write, looking the tags up in the
/// database: `Err` carrying [`InvalidFieldValues`] when anything is refused.
pub(crate) async fn validate_field_values(
    conn: &mut PgConnection,
    definitions: &[FieldDefinition],
    fields: &Value,
    previous: Option<&Value>,
) -> Result<()> {
    let ids = category_tag_ids(fields, definitions, previous);
    let categories = tag_categories(conn, &ids).await?;
    let errors = check_field_values(fields, definitions, previous, &categories);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(InvalidFieldValues(errors).into())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(name: &str, label: &str, field_type: FieldType, cardinality: i32) -> FieldDefinition {
        let mut f = FieldDefinition::new(name, field_type).label(label);
        f.cardinality = cardinality;
        f
    }

    #[test]
    fn cardinality_counts_values_and_minus_one_takes_any() {
        let defs = vec![
            field("one", "One", FieldType::Text { max_length: None }, 1),
            field("three", "Three", FieldType::Integer, 3),
            field("any", "Any", FieldType::Integer, -1),
            field("blocks", "Blocks", FieldType::Blocks, 1),
        ];
        let ok = json!({ "one": "a", "three": [1, 2, 3], "any": [1, 2, 3, 4, 5, 6, 7],
                         "blocks": [{}, {}, {}] });
        assert!(check_field_values(&ok, &defs, None, &HashMap::new()).is_empty());

        let bad = json!({ "one": ["a", "b"], "three": [1, 2, 3, 4] });
        let errors = check_field_values(&bad, &defs, None, &HashMap::new());
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            messages,
            vec![
                "One allows at most 1 value.",
                "Three allows at most 3 values."
            ]
        );
    }

    #[test]
    fn a_category_reference_takes_tags_of_its_category_only() {
        let topic = Uuid::now_v7();
        let venue = Uuid::now_v7();
        let unknown = Uuid::now_v7();
        let defs = vec![field(
            "topics",
            "Topics",
            FieldType::CategoryReference("topics".into()),
            -1,
        )];
        let categories: HashMap<Uuid, String> =
            [(topic, "topics".to_string()), (venue, "venues".to_string())].into();

        let fields = json!({ "topics": [topic, venue, unknown, "x"] });
        assert_eq!(
            category_tag_ids(&fields, &defs, None).len(),
            3,
            "every uuid is looked up, once"
        );
        let errors = check_field_values(&fields, &defs, None, &categories);
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0].message.contains(&venue.to_string()));
        assert!(errors[1].message.contains(&unknown.to_string()));
        assert!(errors[2].message.contains("x is not a tag"));

        let bare = json!({ "topics": topic.to_string() });
        assert_eq!(
            check_field_values(&bare, &defs, None, &categories)[0].message,
            "Topics holds a list of tag ids, not a single value."
        );
    }

    #[test]
    fn an_unchanged_field_is_not_judged_again() {
        let defs = vec![field("one", "One", FieldType::Integer, 1)];
        let stored = json!({ "one": [1, 2] });
        assert!(
            check_field_values(&stored, &defs, Some(&stored), &HashMap::new()).is_empty(),
            "a value saved before the limit stays saveable while untouched"
        );
        let written = json!({ "one": [1, 2, 3] });
        assert_eq!(
            check_field_values(&written, &defs, Some(&stored), &HashMap::new()).len(),
            1
        );
    }

    #[test]
    fn a_category_reference_is_always_a_list() {
        let tags = field("t", "T", FieldType::CategoryReference("c".into()), 1);
        assert!(stores_a_list(&tags));
        assert!(!stores_a_list(&field("x", "X", FieldType::Integer, 1)));
        assert!(stores_a_list(&field("x", "X", FieldType::Integer, 2)));
    }
}
