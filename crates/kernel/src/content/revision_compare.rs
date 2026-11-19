//! Field-by-field comparison of two revisions of an item.
//!
//! Computed from the two `fields` snapshots and titles, never read from
//! `item_revision.change_summary`, which nothing writes. The caller hands in
//! snapshots already cut to what the viewer may see
//! ([`crate::content::ItemService::load_revisions`]), so a field missing from
//! both is not reported at all, not even as removed.
//!
//! Nothing here produces markup. Each changed value is a list of
//! [`DiffSegment`]s of plain text; the template wraps them in elements and
//! Tera's autoescape escapes the text, so no field content ever reaches a page
//! unescaped.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use similar::{ChangeTag, TextDiff};
use trovato_sdk::types::FieldDefinition;

/// How long one value's diff may take before `similar` settles for a coarser
/// (still correct) answer. Bounds the cost of a compare of two very long texts.
const DIFF_TIMEOUT: Duration = Duration::from_millis(250);

/// What a run of text is in the newer revision relative to the older.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    /// In both.
    Equal,
    /// Only in the newer.
    Added,
    /// Only in the older.
    Removed,
}

/// A run of text, as it is in one or both revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffSegment {
    pub kind: SegmentKind,
    pub text: String,
}

/// How a field differs between the two revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// Present only in the newer revision.
    Added,
    /// Present only in the older revision.
    Removed,
    /// Present in both, with different values.
    Changed,
}

/// One field that differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldChange {
    /// Machine name.
    pub name: String,
    /// The label the content type gives it.
    pub label: String,
    pub kind: ChangeKind,
    /// The value as text, marked up by run.
    pub segments: Vec<DiffSegment>,
}

/// Two revisions compared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionComparison {
    /// The title's diff, or `None` when the title is the same.
    pub title: Option<Vec<DiffSegment>>,
    /// Every field that differs, in the content type's field order, then any
    /// field the type no longer declares, by name.
    pub fields: Vec<FieldChange>,
    /// How many fields are present and equal in both.
    pub unchanged: usize,
}

/// The label a field is shown under: the one the content type declares, else
/// one derived from the machine name (`field_start_date` → `start date`).
pub fn field_label(definitions: &[FieldDefinition], name: &str) -> String {
    definitions
        .iter()
        .find(|f| f.field_name == name && !f.label.trim().is_empty())
        .map(|f| f.label.clone())
        .unwrap_or_else(|| {
            name.strip_prefix("field_")
                .unwrap_or(name)
                .replace('_', " ")
        })
}

/// Compare an older revision (`from`) with a newer one (`to`).
///
/// Per field name over both snapshots: added, removed, changed (by JSON
/// equality) or unchanged. A changed text value gets a word diff, or a line
/// diff when either side spans lines; any other value is shown as pretty JSON
/// through the same line diff.
pub fn compare_revisions(
    from_title: &str,
    from_fields: &Value,
    to_title: &str,
    to_fields: &Value,
    definitions: &[FieldDefinition],
) -> RevisionComparison {
    let empty = serde_json::Map::new();
    let from = from_fields.as_object().unwrap_or(&empty);
    let to = to_fields.as_object().unwrap_or(&empty);

    // The type's order first, then whatever it no longer declares, by name.
    let mut names: Vec<&str> = definitions
        .iter()
        .map(|d| d.field_name.as_str())
        .filter(|n| from.contains_key(*n) || to.contains_key(*n))
        .collect();
    let mut undeclared: Vec<&str> = from
        .keys()
        .chain(to.keys())
        .map(String::as_str)
        .filter(|n| !definitions.iter().any(|d| d.field_name == *n))
        .collect();
    undeclared.sort_unstable();
    undeclared.dedup();
    names.extend(undeclared);

    let mut fields = Vec::new();
    let mut unchanged = 0;
    for name in names {
        let (kind, segments) = match (from.get(name), to.get(name)) {
            (Some(old), Some(new)) if old == new => {
                unchanged += 1;
                continue;
            }
            (Some(old), Some(new)) => (ChangeKind::Changed, diff_values(old, new)),
            (Some(old), None) => (ChangeKind::Removed, whole(SegmentKind::Removed, old)),
            (None, Some(new)) => (ChangeKind::Added, whole(SegmentKind::Added, new)),
            (None, None) => continue,
        };
        fields.push(FieldChange {
            name: name.to_string(),
            label: field_label(definitions, name),
            kind,
            segments,
        });
    }

    let title = (from_title != to_title).then(|| diff_text(from_title, to_title));

    RevisionComparison {
        title,
        fields,
        unchanged,
    }
}

/// A value as the text a reader compares: a string as itself, a formatted text
/// (`{"value": .., "format": ..}`) as its value, anything else as pretty JSON.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(o)
            if o.keys().all(|k| k == "value" || k == "format")
                && o.get("value").is_some_and(Value::is_string) =>
        {
            o.get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        other => pretty(other),
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn whole(kind: SegmentKind, value: &Value) -> Vec<DiffSegment> {
    vec![DiffSegment {
        kind,
        text: as_text(value),
    }]
}

/// Two different values. When their texts are equal the difference is in
/// something the text leaves out (a text format, say), so both are shown as
/// pretty JSON instead, where it is visible.
fn diff_values(old: &Value, new: &Value) -> Vec<DiffSegment> {
    let (old_text, new_text) = (as_text(old), as_text(new));
    if old_text == new_text {
        diff_text(&pretty(old), &pretty(new))
    } else {
        diff_text(&old_text, &new_text)
    }
}

/// A word diff, or a line diff when either side has more than one line;
/// adjacent runs of one kind are merged.
fn diff_text(old: &str, new: &str) -> Vec<DiffSegment> {
    let mut config = TextDiff::configure();
    config.timeout(DIFF_TIMEOUT);
    let diff = if old.contains('\n') || new.contains('\n') {
        config.diff_lines(old, new)
    } else {
        config.diff_words(old, new)
    };

    let mut segments: Vec<DiffSegment> = Vec::new();
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => SegmentKind::Equal,
            ChangeTag::Insert => SegmentKind::Added,
            ChangeTag::Delete => SegmentKind::Removed,
        };
        match segments.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(change.value()),
            _ => segments.push(DiffSegment {
                kind,
                text: change.value().to_string(),
            }),
        }
    }
    segments
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use trovato_sdk::types::FieldType;

    fn defs() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::new("field_city", FieldType::Text { max_length: None }).label("City"),
            FieldDefinition::new("field_notes", FieldType::TextLong).label("Editor Notes"),
            FieldDefinition::new("field_online", FieldType::Boolean).label("Online"),
            FieldDefinition::new("field_country", FieldType::Text { max_length: None })
                .label("Country"),
        ]
    }

    fn text_of(segments: &[DiffSegment], kind: SegmentKind) -> String {
        segments
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.text.as_str())
            .collect()
    }

    #[test]
    fn added_removed_changed_and_unchanged_per_field() {
        let from = json!({
            "field_city": {"value": "Barga"},
            "field_online": false,
            "field_country": "Italy",
        });
        let to = json!({
            "field_city": {"value": "Pisa"},
            "field_notes": {"value": "New notes", "format": "filtered_html"},
            "field_country": "Italy",
        });
        let cmp = compare_revisions("T", &from, "T", &to, &defs());

        assert_eq!(cmp.title, None, "an equal title is not a change");
        assert_eq!(cmp.unchanged, 1, "field_country is the one unchanged field");
        let kinds: Vec<(&str, ChangeKind)> = cmp
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("field_city", ChangeKind::Changed),
                ("field_notes", ChangeKind::Added),
                ("field_online", ChangeKind::Removed),
            ],
            "in the content type's order"
        );

        let city = &cmp.fields[0];
        assert_eq!(city.label, "City");
        assert_eq!(text_of(&city.segments, SegmentKind::Removed), "Barga");
        assert_eq!(text_of(&city.segments, SegmentKind::Added), "Pisa");

        let notes = &cmp.fields[1];
        assert_eq!(
            notes.segments,
            vec![DiffSegment {
                kind: SegmentKind::Added,
                text: "New notes".to_string()
            }]
        );
        let online = &cmp.fields[2];
        assert_eq!(text_of(&online.segments, SegmentKind::Removed), "false");
    }

    #[test]
    fn the_title_is_compared_by_word() {
        let cmp = compare_revisions(
            "RustConf 2026 Portland",
            &json!({}),
            "RustConf 2026 Montreal",
            &json!({}),
            &defs(),
        );
        let title = cmp.title.unwrap();
        assert_eq!(text_of(&title, SegmentKind::Equal), "RustConf 2026 ");
        assert_eq!(text_of(&title, SegmentKind::Removed), "Portland");
        assert_eq!(text_of(&title, SegmentKind::Added), "Montreal");
        assert!(cmp.fields.is_empty());
        assert_eq!(cmp.unchanged, 0);
    }

    #[test]
    fn multi_line_text_is_compared_by_line() {
        let from = json!({"field_notes": {"value": "one\ntwo\nthree\n", "format": "plain_text"}});
        let to = json!({"field_notes": {"value": "one\nTWO\nthree\n", "format": "plain_text"}});
        let cmp = compare_revisions("T", &from, "T", &to, &defs());
        let segs = &cmp.fields[0].segments;
        assert_eq!(text_of(segs, SegmentKind::Removed), "two\n");
        assert_eq!(text_of(segs, SegmentKind::Added), "TWO\n");
        assert_eq!(text_of(segs, SegmentKind::Equal), "one\nthree\n");
    }

    #[test]
    fn non_text_values_compare_as_pretty_json() {
        let from = json!({"field_tags": [1, 2, 3]});
        let to = json!({"field_tags": [1, 3]});
        let cmp = compare_revisions("T", &from, "T", &to, &defs());
        let field = &cmp.fields[0];
        assert_eq!(field.kind, ChangeKind::Changed);
        assert_eq!(
            field.label, "tags",
            "an undeclared field's label is derived"
        );
        assert_eq!(text_of(&field.segments, SegmentKind::Removed).trim(), "2,");
    }

    #[test]
    fn a_change_of_format_alone_is_shown_as_json() {
        let from = json!({"field_notes": {"value": "Same", "format": "plain_text"}});
        let to = json!({"field_notes": {"value": "Same", "format": "full_html"}});
        let cmp = compare_revisions("T", &from, "T", &to, &defs());
        let segs = &cmp.fields[0].segments;
        assert!(text_of(segs, SegmentKind::Removed).contains("plain_text"));
        assert!(text_of(segs, SegmentKind::Added).contains("full_html"));
    }

    #[test]
    fn markup_in_a_value_is_left_as_text_for_the_template_to_escape() {
        let from = json!({"field_city": "<b>Barga</b>"});
        let to = json!({"field_city": "<script>alert(1)</script>"});
        let cmp = compare_revisions("T", &from, "T", &to, &defs());
        let segs = &cmp.fields[0].segments;
        assert_eq!(
            text_of(segs, SegmentKind::Added),
            "<script>alert(1)</script>"
        );
    }

    #[test]
    fn a_field_in_neither_snapshot_is_not_reported() {
        // What the access filter leaves: the field is gone from both sides.
        let cmp = compare_revisions("T", &json!({}), "T", &json!({}), &defs());
        assert!(cmp.fields.is_empty());
        assert_eq!(cmp.unchanged, 0);
    }
}
