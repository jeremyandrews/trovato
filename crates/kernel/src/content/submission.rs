//! The typed submission builder: the one place a posted form value becomes a
//! stored field value.
//!
//! Both item form routes used to build the stored field map from the POST body
//! alone, and `Item::update` replaced the whole map, so one save erased every
//! key the form had not rendered: a plugin written key, a key the type does not
//! declare, the values of a multi value field. A checked Boolean was stored as
//! the string `"1"` and `cardinality` was never counted.
//!
//! [`build_item_fields`] starts from the stored map and overwrites only the
//! fields the form rendered. The rendered list comes from the server side form
//! state (see `form::service::FormState`), never from the request.

use serde_json::{Map, Value};
use trovato_sdk::types::{ContentTypeDefinition, FieldDefinition, FieldType};

use crate::form::ValidationError;
use crate::tap::UserContext;

/// Every `(name, value)` pair of a urlencoded form body, repeats and order kept.
///
/// A `HashMap` keeps one value per name, which is exactly what a multi value
/// field cannot survive; this keeps them all.
#[derive(Debug, Clone, Default)]
pub struct PostedForm(pub Vec<(String, String)>);

impl PostedForm {
    /// The first value posted under `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether anything was posted under `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|(k, _)| k == name)
    }

    /// Every value posted for a multi value field, in delta order: `name[0]`,
    /// `name[1]`, … sorted by index, then any bare `name` repeats in the order
    /// they came. A non numeric index is ignored.
    pub fn values_of(&self, name: &str) -> Vec<&str> {
        let prefix = format!("{name}[");
        let mut indexed: Vec<(usize, &str)> = self
            .0
            .iter()
            .filter_map(|(k, v)| {
                let index = k.strip_prefix(&prefix)?.strip_suffix(']')?;
                Some((index.parse().ok()?, v.as_str()))
            })
            .collect();
        indexed.sort_by_key(|(i, _)| *i);
        let mut values: Vec<&str> = indexed.into_iter().map(|(_, v)| v).collect();
        values.extend(
            self.0
                .iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| v.as_str()),
        );
        values
    }
}

/// The text formats `user` may write: `plain_text` always, then each stored
/// format whose `use <format>` permission they hold, lightest first.
pub fn permitted_text_formats(user: &UserContext) -> Vec<String> {
    // `can` carries the administrator bypass, so the formats need no
    // separate admin arm: an administrator answers `true` to every one.
    let mut formats = vec![super::text_format::PLAIN_TEXT.to_string()];
    for format in super::text_format::all().iter() {
        if let Some(permission) = format.permission()
            && user.can(&permission)
        {
            formats.push(format.format.clone());
        }
    }
    formats
}

/// The value a `settings.visible_when` condition
/// compares, as the form posts it: the posted value when the controlling field
/// was rendered and `posted` is given, the stored one otherwise. A Boolean is
/// `"1"` or `""`, whichever side it comes from.
fn controlling_value(
    content_type: &ContentTypeDefinition,
    name: &str,
    stored: Option<&Map<String, Value>>,
    rendered: &[String],
    posted: Option<&PostedForm>,
) -> String {
    let is_boolean = content_type
        .fields
        .iter()
        .any(|f| f.field_name == name && matches!(f.field_type, FieldType::Boolean));
    if let Some(posted) = posted
        && rendered.iter().any(|r| r == name)
    {
        let raw = posted.get(name).unwrap_or("");
        return match (is_boolean, is_truthy(raw)) {
            (true, true) => "1".to_string(),
            (true, false) => String::new(),
            (false, _) => raw.to_string(),
        };
    }
    match stored.and_then(|s| s.get(name)) {
        Some(Value::Bool(true)) => "1".to_string(),
        Some(Value::String(s)) if is_boolean => {
            if is_truthy(s) {
                "1".to_string()
            } else {
                String::new()
            }
        }
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Whether `field` is shown: true unless its `settings.visible_when` condition is
/// false for the controlling value (see `controlling_value`).
///
/// The form builder asks with the stored values to decide what to show first;
/// the submission builder asks with what was posted, so a field the editor
/// hid is not saved and not required.
pub fn is_visible(
    field: &FieldDefinition,
    content_type: &ContentTypeDefinition,
    stored: Option<&Map<String, Value>>,
    rendered: &[String],
    posted: Option<&PostedForm>,
) -> bool {
    field.visible_when_condition().is_none_or(|when| {
        controlling_value(content_type, &when.field, stored, rendered, posted) == when.equals
    })
}

/// Build the field map a form save writes.
///
/// Starts from `stored` (an add form passes `None`) and overwrites only the
/// fields named in `rendered`, each coerced by its [`FieldType`]: a Boolean to
/// JSON `true`/`false` (an absent rendered checkbox is `false`), Integer and
/// Float to numbers (empty removes the key), Text, Email, Date, File and
/// RecordReference to strings, TextLong to `{value, format}` when a format was
/// posted, Blocks, Compound and PageBuilder parsed from their JSON input. A
/// field whose `cardinality` is not 1 becomes an array of its non empty values,
/// and more values than a positive cardinality is a field error. Every key the
/// form did not render survives untouched, and so does a field whose
/// `visible_when` condition is false for the posted values: it is neither
/// written nor required.
///
/// Returns every error at once, so the form can show them all.
pub fn build_item_fields(
    stored: Option<&Map<String, Value>>,
    content_type: &ContentTypeDefinition,
    rendered: &[String],
    posted: &PostedForm,
    permitted_formats: &[String],
) -> Result<Map<String, Value>, Vec<ValidationError>> {
    let mut out = stored.cloned().unwrap_or_default();
    let mut errors = Vec::new();

    // The JSON widgets are parsed and checked by the existing compound and
    // block processors, which work on a map of raw strings; collect them here.
    let mut json_inputs = Map::new();
    let mut json_defs: Vec<FieldDefinition> = Vec::new();

    for field in content_type
        .fields
        .iter()
        .filter(|f| rendered.contains(&f.field_name))
        .filter(|f| is_visible(f, content_type, stored, rendered, Some(posted)))
    {
        let name = &field.field_name;
        if matches!(
            field.field_type,
            FieldType::Compound { .. } | FieldType::Blocks
        ) {
            if let Some(raw) = posted.get(name) {
                json_inputs.insert(name.clone(), Value::String(raw.to_string()));
            }
            json_defs.push(field.clone());
            continue;
        }

        if field.cardinality != 1 {
            let mut values = Vec::new();
            for raw in posted.values_of(name) {
                if raw.trim().is_empty() {
                    continue;
                }
                match coerce(field, raw, posted, permitted_formats) {
                    Ok(Some(v)) => values.push(v),
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
            }
            if field.cardinality > 0 && values.len() > field.cardinality as usize {
                errors.push(ValidationError::field(
                    name,
                    format!(
                        "{} allows at most {} values.",
                        field.label, field.cardinality
                    ),
                ));
            }
            if field.required && values.is_empty() {
                errors.push(required_error(field));
            }
            out.insert(name.clone(), Value::Array(values));
            continue;
        }

        if matches!(field.field_type, FieldType::Boolean) {
            let checked = posted.get(name).is_some_and(is_truthy);
            out.insert(name.clone(), Value::Bool(checked));
            continue;
        }

        let raw = posted.get(name).unwrap_or("");
        if field.required && raw.trim().is_empty() {
            errors.push(required_error(field));
            continue;
        }
        match coerce(field, raw, posted, permitted_formats) {
            Ok(Some(v)) => {
                out.insert(name.clone(), v);
            }
            Ok(None) => {
                out.remove(name);
            }
            Err(e) => errors.push(e),
        }
    }

    errors.extend(
        crate::content::compound::process_compound_fields(&mut json_inputs, &json_defs)
            .into_iter()
            .chain(crate::content::compound::process_blocks_fields(
                &mut json_inputs,
                &json_defs,
            ))
            .map(ValidationError::form),
    );
    out.extend(json_inputs);

    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// The "is required" error for `field`.
fn required_error(field: &FieldDefinition) -> ValidationError {
    ValidationError::field(&field.field_name, format!("{} is required.", field.label))
}

/// Whether a posted checkbox value means checked. A browser posts the input's
/// `value` (the kernel renders `1`); `true` and `on` are what a hand built
/// request or a browser default send.
fn is_truthy(raw: &str) -> bool {
    matches!(raw.trim(), "1" | "true" | "on")
}

/// Coerce one posted value by the field's type. `Ok(None)` means "no value":
/// the key is removed rather than stored as an empty string a number field
/// cannot hold.
fn coerce(
    field: &FieldDefinition,
    raw: &str,
    posted: &PostedForm,
    permitted_formats: &[String],
) -> Result<Option<Value>, ValidationError> {
    let name = &field.field_name;
    let trimmed = raw.trim();
    match &field.field_type {
        FieldType::Boolean => Ok(Some(Value::Bool(is_truthy(raw)))),
        FieldType::Integer => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<i64>()
                .map(|n| Some(Value::from(n)))
                .map_err(|_| {
                    ValidationError::field(name, format!("{} must be a whole number.", field.label))
                })
        }
        FieldType::Float => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(|n| Some(Value::Number(n)))
                .ok_or_else(|| {
                    ValidationError::field(name, format!("{} must be a number.", field.label))
                })
        }
        FieldType::Email => {
            if !trimmed.is_empty() && !trimmed.contains('@') {
                return Err(ValidationError::field(
                    name,
                    format!("{} must be a valid email address.", field.label),
                ));
            }
            Ok(Some(Value::String(trimmed.to_string())))
        }
        FieldType::TextLong => {
            let Some(format) = posted.get(&format!("{name}_format")) else {
                return Ok(Some(Value::String(raw.to_string())));
            };
            if !permitted_formats.iter().any(|f| f == format) {
                return Err(ValidationError::field(
                    name,
                    format!("{}: you may not use the {format} text format.", field.label),
                ));
            }
            Ok(Some(serde_json::json!({ "value": raw, "format": format })))
        }
        FieldType::PageBuilder => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            serde_json::from_str(trimmed).map(Some).map_err(|_| {
                ValidationError::field(name, format!("{}: invalid page data.", field.label))
            })
        }
        FieldType::Text { .. }
        | FieldType::Date
        | FieldType::File
        | FieldType::RecordReference(_)
        | FieldType::Compound { .. }
        | FieldType::Blocks => Ok(Some(Value::String(raw.to_string()))),
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn content_type() -> ContentTypeDefinition {
        ContentTypeDefinition {
            machine_name: "probe".to_string(),
            label: "Probe".to_string(),
            description: String::new(),
            title_label: None,
            fields: vec![
                FieldDefinition::new("field_text", FieldType::Text { max_length: None })
                    .label("Text"),
                FieldDefinition::new("field_flag", FieldType::Boolean).label("Flag"),
                FieldDefinition::new("field_count", FieldType::Integer).label("Count"),
                FieldDefinition::new("field_tags", FieldType::Text { max_length: None })
                    .label("Tags")
                    .cardinality(2),
                FieldDefinition::new("field_body", FieldType::TextLong).label("Body"),
            ],
        }
    }

    fn posted(pairs: &[(&str, &str)]) -> PostedForm {
        PostedForm(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn all_rendered() -> Vec<String> {
        content_type()
            .fields
            .iter()
            .map(|f| f.field_name.clone())
            .collect()
    }

    #[test]
    fn unrendered_keys_survive() {
        let stored = json!({"field_text": "old", "plugin_key": {"a": 1}, "field_flag": true});
        let out = build_item_fields(
            stored.as_object(),
            &content_type(),
            &["field_text".to_string()],
            &posted(&[("field_text", "new")]),
            &[],
        )
        .unwrap();
        assert_eq!(out["field_text"], json!("new"));
        assert_eq!(out["plugin_key"], json!({"a": 1}));
        assert_eq!(
            out["field_flag"],
            json!(true),
            "an unrendered Boolean is kept"
        );
    }

    #[test]
    fn booleans_and_numbers_are_typed() {
        let out = build_item_fields(
            None,
            &content_type(),
            &all_rendered(),
            &posted(&[("field_count", " 42 ")]),
            &[],
        )
        .unwrap();
        assert_eq!(out["field_flag"], json!(false), "absent checkbox is false");
        assert_eq!(out["field_count"], json!(42));

        let out = build_item_fields(
            None,
            &content_type(),
            &all_rendered(),
            &posted(&[("field_flag", "1"), ("field_count", "")]),
            &[],
        )
        .unwrap();
        assert_eq!(out["field_flag"], json!(true));
        assert!(
            out.get("field_count").is_none(),
            "empty number removes the key"
        );
    }

    #[test]
    fn a_bad_number_is_a_field_error() {
        let errors = build_item_fields(
            None,
            &content_type(),
            &all_rendered(),
            &posted(&[("field_count", "seven")]),
            &[],
        )
        .unwrap_err();
        assert_eq!(errors[0].field.as_deref(), Some("field_count"));
    }

    #[test]
    fn multi_values_are_collected_in_delta_order_and_counted() {
        let out = build_item_fields(
            None,
            &content_type(),
            &all_rendered(),
            &posted(&[
                ("field_tags[1]", "b"),
                ("field_tags[0]", "a"),
                ("field_tags[2]", ""),
            ]),
            &[],
        )
        .unwrap();
        assert_eq!(out["field_tags"], json!(["a", "b"]));

        let errors = build_item_fields(
            None,
            &content_type(),
            &all_rendered(),
            &posted(&[
                ("field_tags[0]", "a"),
                ("field_tags[1]", "b"),
                ("field_tags[2]", "c"),
            ]),
            &[],
        )
        .unwrap_err();
        assert_eq!(errors[0].message, "Tags allows at most 2 values.");
    }

    #[test]
    fn a_text_format_must_be_permitted() {
        let formats = vec!["plain_text".to_string(), "filtered_html".to_string()];
        let out = build_item_fields(
            None,
            &content_type(),
            &all_rendered(),
            &posted(&[
                ("field_body", "<p>x</p>"),
                ("field_body_format", "filtered_html"),
            ]),
            &formats,
        )
        .unwrap();
        assert_eq!(
            out["field_body"],
            json!({"value": "<p>x</p>", "format": "filtered_html"})
        );
        assert!(!out.contains_key("field_body_format"));

        let errors = build_item_fields(
            None,
            &content_type(),
            &all_rendered(),
            &posted(&[("field_body", "x"), ("field_body_format", "full_html")]),
            &formats,
        )
        .unwrap_err();
        assert_eq!(errors[0].field.as_deref(), Some("field_body"));
    }

    #[test]
    fn a_hidden_field_is_neither_saved_nor_required() {
        let mut ct = content_type();
        ct.fields.push(
            FieldDefinition::new("field_why", FieldType::Text { max_length: None })
                .label("Why")
                .required()
                .visible_when("field_flag", "1"),
        );
        let mut rendered = all_rendered();
        rendered.push("field_why".to_string());
        let stored = json!({"field_why": "kept"});

        // Flag unchecked: the posted value is dropped and nothing is required.
        let out = build_item_fields(
            stored.as_object(),
            &ct,
            &rendered,
            &posted(&[("field_why", "typed while hidden")]),
            &[],
        )
        .unwrap();
        assert_eq!(out["field_why"], json!("kept"));

        // Flag checked: the field is shown, saved, and required.
        let out = build_item_fields(
            stored.as_object(),
            &ct,
            &rendered,
            &posted(&[("field_flag", "1"), ("field_why", "because")]),
            &[],
        )
        .unwrap();
        assert_eq!(out["field_why"], json!("because"));
        let errors = build_item_fields(None, &ct, &rendered, &posted(&[("field_flag", "1")]), &[])
            .unwrap_err();
        assert_eq!(errors[0].field.as_deref(), Some("field_why"));
    }

    #[test]
    fn visibility_reads_the_stored_value_when_nothing_was_posted() {
        let mut ct = content_type();
        ct.fields.push(
            FieldDefinition::new("field_why", FieldType::Text { max_length: None })
                .visible_when("field_flag", "1"),
        );
        let why = ct.fields.last().unwrap().clone();
        let on = json!({"field_flag": true});
        let off = json!({"field_flag": false});
        assert!(is_visible(&why, &ct, on.as_object(), &[], None));
        assert!(!is_visible(&why, &ct, off.as_object(), &[], None));
        assert!(!is_visible(&why, &ct, None, &[], None));
    }
}
