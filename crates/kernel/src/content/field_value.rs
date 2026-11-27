//! A field value's form shape, owned by its field type: what a widget shows
//! for a stored value, and how a posted value goes back into that stored
//! shape.
//!
//! The form builder (`content::form`) read a stored value tolerantly: a bare
//! scalar or a `{"value": …}` wrapper, a reference as a uuid or as
//! `{"target_id": …}`. The submission builder (`content::submission`) wrote
//! back only the canonical shape. So a save with nothing changed rewrote
//! `{"value": "Barga"}` as `"Barga"`, a reference object as a bare uuid, a long
//! text without its `summary`, a number held as a string as nothing at all, and
//! a textarea's newlines as the CRLF a browser submits. What a field held
//! depended on which form last saved it.
//!
//! Both directions now live here, one function per direction, matched on the
//! [`FieldType`]:
//!
//! - [`form_text`], [`form_checked`] and [`form_format`] say what the widget
//!   shows for a stored value;
//! - [`is_unchanged`] says whether what came back is what was shown, in which
//!   case the stored value is kept exactly as it was;
//! - [`parse`] turns a changed posted value into a stored value, and
//!   [`reshape`] writes it into the stored value's shape (a wrapper stays a
//!   wrapper, a reference object keeps its other keys, a long text keeps its
//!   `summary`);
//! - [`check_json`] is the same reading applied to a value sent as JSON.

use serde_json::{Map, Value};
use trovato_sdk::types::{FieldDefinition, FieldType};

use crate::form::ValidationError;

/// The text format a long text field shows when the stored value names none:
/// the renderer (`routes::item::render_item_fields`) treats such a value as
/// plain text, so that is what the selector says.
const STORED_DEFAULT_FORMAT: &str = "plain_text";

/// The text format offered for a new value.
const NEW_DEFAULT_FORMAT: &str = "filtered_html";

/// The scalar inside a stored value: the `{"value": …}` wrapper's payload when
/// there is one, otherwise the value itself.
fn scalar_of(value: &Value) -> &Value {
    value.get("value").unwrap_or(value)
}

/// A scalar as text, or `None` for an array, an object or null.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// The text a single value widget shows for `value`, or `""` when there is
/// none.
///
/// Every shape a writer is known to store is read: a bare scalar or a
/// `{"value": …}` wrapper; a number held as a string; a reference as a uuid,
/// `{"target_id": …}` or a one element list; a date as `YYYY-MM-DD`, a longer
/// ISO timestamp (its date part, which is all a date input can show) or unix
/// seconds. The JSON widgets carry the value as JSON text.
pub fn form_text(field_type: &FieldType, value: Option<&Value>) -> String {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return match field_type {
            FieldType::Blocks => "[]".to_string(),
            FieldType::PageBuilder => r#"{"root":{"title":""},"content":[]}"#.to_string(),
            FieldType::Compound { .. } => r#"{"sections":[]}"#.to_string(),
            _ => String::new(),
        };
    };
    match field_type {
        FieldType::Blocks | FieldType::PageBuilder | FieldType::Compound { .. } => {
            serde_json::to_string(value).unwrap_or_default()
        }
        FieldType::RecordReference(_) | FieldType::CategoryReference(_) => reference_id(value),
        FieldType::Boolean => {
            if is_checked(value) {
                "1".to_string()
            } else {
                String::new()
            }
        }
        FieldType::Integer => match scalar_of(value) {
            Value::Number(n) => n
                .as_i64()
                .map(|i| i.to_string())
                .unwrap_or_else(|| n.to_string()),
            other => scalar_text(other).unwrap_or_default(),
        },
        FieldType::Float => match scalar_of(value) {
            Value::Number(n) => n
                .as_f64()
                .map(|f| f.to_string())
                .unwrap_or_else(|| n.to_string()),
            other => scalar_text(other).unwrap_or_default(),
        },
        FieldType::Date => date_text(scalar_of(value)),
        FieldType::Text { .. }
        | FieldType::TextLong
        | FieldType::Email
        | FieldType::File
        | FieldType::List => scalar_text(scalar_of(value)).unwrap_or_default(),
        // The entered text, not the number: `scalar_of` already reads a
        // `{"value": …}` wrapper, which is exactly how a Telephone is stored.
        FieldType::Telephone => scalar_text(scalar_of(value)).unwrap_or_default(),
        // A Link has no `value` key to read with `scalar_of`: its text widget
        // shows the `uri`.
        FieldType::Link => value
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        // `FieldType` is `#[non_exhaustive]`: a kind this kernel has no arm
        // for yet shows as text (see `trovato_sdk::types::FieldType`).
        _ => scalar_text(scalar_of(value)).unwrap_or_default(),
    }
}

/// Whether a Boolean widget shows `value` checked: `true`, `1`, or the
/// strings `"1"`, `"true"`, `"on"`, bare or wrapped.
pub fn form_checked(value: Option<&Value>) -> bool {
    value.is_some_and(is_checked)
}

fn is_checked(value: &Value) -> bool {
    match scalar_of(value) {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => is_truthy(s),
        _ => false,
    }
}

/// Whether a posted checkbox value means checked. A browser posts the input's
/// `value` (the kernel renders `1`); `true` and `on` are what a hand built
/// request or a browser default send.
pub fn is_truthy(raw: &str) -> bool {
    matches!(raw.trim(), "1" | "true" | "on")
}

/// The text format a long text's selector shows: the stored one; plain text
/// for a stored value that names none (how it renders); the default for a new
/// value.
pub fn form_format(value: Option<&Value>) -> String {
    match value.filter(|v| !v.is_null()) {
        None => NEW_DEFAULT_FORMAT.to_string(),
        Some(v) => v
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or(STORED_DEFAULT_FORMAT)
            .to_string(),
    }
}

/// The title a Link widget's sibling text input shows for a stored value.
pub fn form_link_title(value: Option<&Value>) -> String {
    value
        .filter(|v| !v.is_null())
        .and_then(|v| v.get("title"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether a Link widget's sibling "open in a new tab" checkbox shows
/// checked for a stored value.
pub fn form_link_new_tab(value: Option<&Value>) -> bool {
    value
        .filter(|v| !v.is_null())
        .and_then(|v| v.get("new_tab"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The values a multi value field's widgets show, one per stored value: a
/// stored list in order, a single stored value as one, nothing as none.
pub fn form_values(value: Option<&Value>) -> Vec<&Value> {
    match value {
        Some(Value::Array(values)) => values.iter().collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(single) => vec![single],
    }
}

/// The id a stored reference holds: a uuid string, `{"target_id": …}`,
/// `{"value": …}`, or the first of a list of those.
pub fn reference_id(value: &Value) -> String {
    let candidate = value
        .get("target_id")
        .or_else(|| value.get("value"))
        .unwrap_or(value);
    match candidate {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .first()
            .and_then(|v| v.get("target_id").unwrap_or(v).as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// The maximum length of a Link's `uri`.
const MAX_LINK_URI_LENGTH: usize = 2048;

/// The maximum length of a Link's `title`.
const MAX_LINK_TITLE_LENGTH: usize = 255;

/// The maximum length of a Telephone's entered text.
const MAX_TELEPHONE_LENGTH: usize = 32;

/// Whether `uri` is a Link field's allowed shape: an absolute `http` or
/// `https` URL with a host, or a local path ([`crate::routes::front::is_local_path`],
/// which covers an item reference written as `/item/<uuid>`).
///
/// Refused outright: `javascript:`, `data:`, `mailto:` and any other scheme,
/// a protocol-relative `//host`, a bare `example.com` with no path, and any
/// whitespace or control character. The one place this rule lives; the form
/// parser, the JSON check and the one write check every path reaches all call
/// it.
pub fn is_valid_link_uri(uri: &str) -> bool {
    if uri.is_empty() || uri.len() > MAX_LINK_URI_LENGTH {
        return false;
    }
    if uri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    match url::Url::parse(uri) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
        Err(_) => crate::routes::front::is_local_path(uri),
    }
}

/// The allowed keys of a `List` field, in order, read from
/// `settings.allowed_values` (`[{"key": "...", "label": "..."}]`).
pub fn list_options(field: &FieldDefinition) -> Vec<(String, String)> {
    field
        .settings
        .get("allowed_values")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| {
                    let key = v.get("key")?.as_str()?.to_string();
                    let label = v
                        .get("label")
                        .and_then(Value::as_str)
                        .map_or_else(|| key.clone(), str::to_string);
                    Some((key, label))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `key` is one of `field`'s allowed values: the one place a List
/// write is judged against its allowed keys.
pub fn is_valid_list_key(field: &FieldDefinition, key: &str) -> bool {
    list_options(field).iter().any(|(k, _)| k == key)
}

/// The label shown for a List field's stored `key`, or `None` when the key is
/// no longer one of the field's allowed values.
pub fn list_label_for(field: &FieldDefinition, key: &str) -> Option<String> {
    list_options(field)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, label)| label)
}

/// Whether `text` holds only the characters a telephone number may be entered
/// with: digits, spaces, and `+ ( ) - . /`.
pub fn is_valid_telephone_text(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= MAX_TELEPHONE_LENGTH
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '+' | '(' | ')' | '-' | '.' | '/'))
}

/// The E.164 form of an entered telephone number, when it has an explicit
/// country prefix: a leading `+` or `00`, whose remainder is 7 to 15 digits,
/// the first of them not `0`. Anything else (a national number with no
/// country prefix) has no E.164 form: it is stored as entered.
///
/// The one place this normalisation lives: every write path fills `e164` with
/// it before the value is checked, and the check refuses a written `e164`
/// that disagrees with it.
pub fn telephone_e164(entered: &str) -> Option<String> {
    let cleaned: String = entered
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '(' | ')' | '-' | '.' | '/'))
        .collect();
    let digits = if let Some(rest) = cleaned.strip_prefix('+') {
        rest
    } else if let Some(rest) = cleaned.strip_prefix("00") {
        rest
    } else {
        return None;
    };
    if digits.len() < 7 || digits.len() > 15 {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit()) || digits.starts_with('0') {
        return None;
    }
    Some(format!("+{digits}"))
}

/// The value-shape rule for a Link, List or Telephone field: the one place it
/// is judged, called from the form parser ([`parse`] builds its own value, but
/// uses the same predicates), the JSON check ([`check_one`]) and the one write
/// check every path reaches
/// ([`crate::content::field_values::check_field_values`]).
///
/// `None` means the value is valid (null is always valid: it clears the
/// field); `Some` is the refusal, without the field's label, which the caller
/// prefixes.
pub fn check_new_kind(field: &FieldDefinition, value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    match &field.field_type {
        FieldType::List => {
            let ok = matches!(value, Value::String(key) if is_valid_list_key(field, key));
            (!ok).then(|| "is not one of the allowed values.".to_string())
        }
        FieldType::Link => {
            let (uri, title, new_tab) = match value {
                Value::Object(obj) => (
                    obj.get("uri").and_then(Value::as_str),
                    obj.get("title"),
                    obj.get("new_tab"),
                ),
                Value::String(s) => (Some(s.as_str()), None, None),
                _ => (None, None, None),
            };
            if !uri.is_some_and(is_valid_link_uri) {
                return Some("is not a valid link.".to_string());
            }
            if let Some(title) = title
                && !title
                    .as_str()
                    .is_some_and(|s| s.chars().count() <= MAX_LINK_TITLE_LENGTH)
            {
                return Some(format!(
                    "title must be at most {MAX_LINK_TITLE_LENGTH} characters."
                ));
            }
            if let Some(new_tab) = new_tab
                && !new_tab.is_boolean()
            {
                return Some("new_tab must be true or false.".to_string());
            }
            None
        }
        FieldType::Telephone => {
            let (entered, e164) = match value {
                Value::String(s) => (Some(s.as_str()), None),
                Value::Object(obj) => (obj.get("value").and_then(Value::as_str), obj.get("e164")),
                _ => (None, None),
            };
            let Some(entered) = entered else {
                return Some("is not a valid telephone number.".to_string());
            };
            if !is_valid_telephone_text(entered) {
                return Some("must hold only digits, spaces and + ( ) - . /".to_string());
            }
            if let Some(e164) = e164 {
                let expected = telephone_e164(entered);
                let matches =
                    matches!(e164, Value::String(s) if Some(s.as_str()) == expected.as_deref());
                if !matches {
                    return Some("has an e164 that does not match its value.".to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Fill `e164` on every `Telephone` value `fields` holds, before the write is
/// checked: the one point every write path (`ItemService` create and update,
/// `trovato import`) passes a value through, so a caller that sends a bare
/// string or `{"value": …}` with no `e164` gets the canonical object stored,
/// and [`check_new_kind`] never sees a value it would otherwise refuse for a
/// missing `e164`. A field the write leaves unchanged (`previous` holds the
/// same value) is left alone.
pub fn canonicalize_field_values(
    fields: &mut Value,
    definitions: &[FieldDefinition],
    previous: Option<&Value>,
) {
    let Some(map) = fields.as_object_mut() else {
        return;
    };
    for field in definitions
        .iter()
        .filter(|f| matches!(f.field_type, FieldType::Telephone))
    {
        let name = &field.field_name;
        if previous.is_some_and(|p| p.get(name) == map.get(name)) {
            continue;
        }
        let Some(value) = map.get_mut(name) else {
            continue;
        };
        canonicalize_telephone_value(value);
    }
}

/// Canonicalise one Telephone value (or array of values) in place: a bare
/// string or a `{"value": …}` object missing `e164` gets it filled in from
/// [`telephone_e164`]; anything else is left for [`check_new_kind`] to refuse.
fn canonicalize_telephone_value(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for v in values.iter_mut() {
                canonicalize_telephone_value(v);
            }
        }
        Value::String(s) => {
            let entered = s.trim().to_string();
            let mut obj = serde_json::json!({ "value": entered });
            if let Some(e164) = telephone_e164(&entered) {
                obj["e164"] = Value::String(e164);
            }
            *value = obj;
        }
        Value::Object(obj) if obj.contains_key("value") && !obj.contains_key("e164") => {
            if let Some(entered) = obj.get("value").and_then(Value::as_str)
                && let Some(e164) = telephone_e164(entered)
            {
                obj.insert("e164".to_string(), Value::String(e164));
            }
        }
        _ => {}
    }
}

/// A stored date as a date input shows it, `YYYY-MM-DD`: the date part of a
/// timestamp string, unix seconds converted (UTC), anything else as it is.
fn date_text(value: &Value) -> String {
    match value {
        Value::String(s) => {
            let head = s.get(..10).unwrap_or(s);
            if is_iso_date(head) {
                head.to_string()
            } else {
                s.clone()
            }
        }
        Value::Number(n) => n
            .as_i64()
            .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| n.to_string()),
        other => scalar_text(other).unwrap_or_default(),
    }
}

fn is_iso_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() && s.len() == 10
}

/// Text as a browser returns it for comparison: a textarea's newlines come
/// back as CRLF, a single line input drops them, and an email input trims.
fn normalized(field_type: &FieldType, text: &str) -> String {
    match field_type {
        FieldType::TextLong => text.replace("\r\n", "\n").replace('\r', "\n"),
        FieldType::Email => text.replace(['\r', '\n'], "").trim().to_string(),
        FieldType::Integer | FieldType::Float | FieldType::Telephone => text.trim().to_string(),
        _ => text.replace(['\r', '\n'], ""),
    }
}

/// Whether a posted single value is what the widget showed for `stored`, so
/// the save keeps `stored` exactly as it is (absent stays absent).
///
/// `format` is the posted text format of a long text field. A JSON widget's
/// text is compared as JSON, so a widget that re-serializes it (key order,
/// spacing) is still unchanged.
pub fn is_unchanged(
    field_type: &FieldType,
    stored: Option<&Value>,
    raw: &str,
    format: Option<&str>,
) -> bool {
    match field_type {
        FieldType::Boolean => form_checked(stored) == is_truthy(raw),
        FieldType::Blocks | FieldType::PageBuilder | FieldType::Compound { .. } => {
            let shown = form_text(field_type, stored);
            raw == shown
                || serde_json::from_str::<Value>(raw).ok()
                    == serde_json::from_str::<Value>(&shown).ok()
        }
        FieldType::TextLong => {
            let same_text = normalized(field_type, raw)
                == normalized(field_type, &form_text(field_type, stored));
            // An empty field with no format posted, as a hand built request
            // sends it, is unchanged too.
            same_text
                && format.is_none_or(|f| {
                    f == form_format(stored) || (stored.is_none() && raw.is_empty())
                })
        }
        _ => normalized(field_type, raw) == normalized(field_type, &form_text(field_type, stored)),
    }
}

/// Write a changed value into the stored value's shape.
///
/// A `{"value": …}` wrapper stays a wrapper, with its other keys (a long
/// text's `summary`); a reference object keeps its other keys with the new
/// `target_id` (and drops a cached `title`, which named the old target);
/// anything else takes the new value as it is.
pub fn reshape(stored: Option<&Value>, new: Value) -> Value {
    let Some(Value::Object(old)) = stored else {
        return new;
    };
    if old.contains_key("target_id") {
        if let Value::String(id) = new {
            let mut out = old.clone();
            if out.get("target_id").and_then(Value::as_str) != Some(id.as_str()) {
                out.remove("title");
            }
            out.insert("target_id".to_string(), Value::String(id));
            return Value::Object(out);
        }
        return new;
    }
    if !old.contains_key("value") {
        return new;
    }
    let mut out = old.clone();
    match new {
        Value::Object(parts) => {
            for (k, v) in parts {
                out.insert(k, v);
            }
        }
        scalar => {
            out.insert("value".to_string(), scalar);
        }
    }
    Value::Object(out)
}

/// Parse one posted value by the field's type. `Ok(None)` means "no value":
/// the key is removed rather than stored as an empty string a number field
/// cannot hold. A long text with a posted format is `{value, format}`, and
/// the format must be one `permitted_formats` holds.
pub fn parse(
    field: &FieldDefinition,
    raw: &str,
    format: Option<&str>,
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
            // A textarea submits CRLF; what is stored is what was typed.
            let text = raw.replace("\r\n", "\n");
            let Some(format) = format else {
                return Ok(Some(Value::String(text)));
            };
            if !permitted_formats.iter().any(|f| f == format) {
                return Err(ValidationError::field(
                    name,
                    format!("{}: you may not use the {format} text format.", field.label),
                ));
            }
            Ok(Some(serde_json::json!({ "value": text, "format": format })))
        }
        FieldType::PageBuilder => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            serde_json::from_str(trimmed).map(Some).map_err(|_| {
                ValidationError::field(name, format!("{}: invalid page data.", field.label))
            })
        }
        FieldType::List => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            if !is_valid_list_key(field, trimmed) {
                return Err(ValidationError::field(
                    name,
                    format!("{} is not one of the allowed values.", field.label),
                ));
            }
            Ok(Some(Value::String(trimmed.to_string())))
        }
        FieldType::Telephone => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            if !is_valid_telephone_text(trimmed) {
                return Err(ValidationError::field(
                    name,
                    format!(
                        "{} must hold only digits, spaces and + ( ) - . /",
                        field.label
                    ),
                ));
            }
            let mut value = serde_json::json!({ "value": trimmed });
            if let Some(e164) = telephone_e164(trimmed) {
                value["e164"] = Value::String(e164);
            }
            Ok(Some(value))
        }
        FieldType::Text { .. }
        | FieldType::Date
        | FieldType::File
        | FieldType::RecordReference(_)
        | FieldType::CategoryReference(_)
        | FieldType::Compound { .. }
        | FieldType::Blocks => Ok(Some(Value::String(raw.to_string()))),
        // `FieldType` is `#[non_exhaustive]`: an unknown kind is kept as text.
        _ => Ok(Some(Value::String(raw.to_string()))),
    }
}

/// Check a value sent as JSON for `field` with the same reading the form
/// uses: every shape [`form_text`] reads is accepted, anything it would show
/// as empty or garbled is refused. Null clears the field and is always
/// accepted. A long text's format must be one the writer may use.
pub fn check_json(
    field: &FieldDefinition,
    value: &Value,
    permitted_formats: &[String],
) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    if crate::content::field_values::stores_a_list(field) {
        let values = form_values(Some(value));
        // A category reference's count is judged with its tags by the field
        // value check the item service runs (`content::field_values`, a 422
        // naming the field), so it is not refused here as a malformed body.
        if !matches!(field.field_type, FieldType::CategoryReference(_))
            && let Some(message) =
                crate::content::field_values::too_many_values(field, values.len())
        {
            return Err(message);
        }
        return values
            .into_iter()
            .try_for_each(|v| check_one(field, v, permitted_formats));
    }
    check_one(field, value, permitted_formats)
}

fn check_one(
    field: &FieldDefinition,
    value: &Value,
    permitted_formats: &[String],
) -> Result<(), String> {
    let label = &field.label;
    let inner = scalar_of(value);
    let wrapped_scalar = !inner.is_object() && !inner.is_array();
    let ok = match &field.field_type {
        FieldType::Blocks => value.is_array(),
        FieldType::Compound { .. } | FieldType::PageBuilder => value.is_object(),
        FieldType::RecordReference(_) => {
            value.is_string() || !reference_id(value).is_empty() || value.is_number()
        }
        // Whether the uuid names a tag of the field's category is the field
        // value check's to decide, with the tag lookup.
        FieldType::CategoryReference(_) => value.is_string(),
        FieldType::Boolean => match inner {
            Value::Bool(_) | Value::Number(_) | Value::Null => true,
            Value::String(s) => {
                matches!(s.trim(), "" | "0" | "1" | "true" | "false" | "on" | "off")
            }
            _ => false,
        },
        FieldType::Integer => match inner {
            Value::Number(n) => n.is_i64() || n.is_u64(),
            Value::String(s) => s.trim().is_empty() || s.trim().parse::<i64>().is_ok(),
            Value::Null => true,
            _ => false,
        },
        FieldType::Float => match inner {
            Value::Number(_) | Value::Null => true,
            Value::String(s) => s.trim().is_empty() || s.trim().parse::<f64>().is_ok(),
            _ => false,
        },
        FieldType::Email => match inner {
            Value::String(s) => s.trim().is_empty() || s.contains('@'),
            Value::Null => true,
            _ => false,
        },
        FieldType::TextLong => {
            if let Some(format) = value.get("format") {
                let Some(format) = format.as_str() else {
                    return Err(format!("{label}: the text format must be a string."));
                };
                if !permitted_formats.iter().any(|f| f == format) {
                    return Err(format!(
                        "{label}: you may not use the {format} text format."
                    ));
                }
            }
            wrapped_scalar
        }
        FieldType::Text { .. } | FieldType::Date | FieldType::File => wrapped_scalar,
        FieldType::List | FieldType::Link | FieldType::Telephone => {
            return check_new_kind(field, value)
                .map_or(Ok(()), |message| Err(format!("{label}: {message}")));
        }
        // `FieldType` is `#[non_exhaustive]`: an unknown kind takes a scalar.
        _ => wrapped_scalar,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{label}: a value of this shape is not a {}.",
            type_name(&field.field_type)
        ))
    }
}

fn type_name(field_type: &FieldType) -> &'static str {
    match field_type {
        FieldType::Text { .. } => "text value",
        FieldType::TextLong => "long text value",
        FieldType::Integer => "whole number",
        FieldType::Float => "number",
        FieldType::Boolean => "Boolean",
        FieldType::RecordReference(_) => "reference",
        FieldType::CategoryReference(_) => "tag reference",
        FieldType::File => "file reference",
        FieldType::Date => "date",
        FieldType::Email => "email address",
        FieldType::Compound { .. } => "compound value",
        FieldType::Blocks => "list of blocks",
        FieldType::PageBuilder => "page",
        FieldType::Link => "link",
        FieldType::List => "list value",
        FieldType::Telephone => "telephone number",
        _ => "value",
    }
}

/// Check every declared field a JSON write changes: a field whose value is
/// what `stored` already holds is not checked, so an item a plugin wrote in
/// some other shape stays saveable. Returns every message at once.
pub fn check_json_fields(
    fields: &[FieldDefinition],
    sent: &Map<String, Value>,
    stored: Option<&Map<String, Value>>,
    permitted_formats: &[String],
) -> Vec<String> {
    fields
        .iter()
        .filter_map(|field| {
            let value = sent.get(&field.field_name)?;
            if stored.and_then(|s| s.get(&field.field_name)) == Some(value) {
                return None;
            }
            check_json(field, value, permitted_formats).err()
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text() -> FieldType {
        FieldType::Text { max_length: None }
    }

    #[test]
    fn every_known_shape_reads_back_as_the_widget_text() {
        assert_eq!(
            form_text(&text(), Some(&json!({"value": "Barga"}))),
            "Barga"
        );
        assert_eq!(form_text(&FieldType::Integer, Some(&json!("7"))), "7");
        assert_eq!(
            form_text(&FieldType::Integer, Some(&json!({"value": 2}))),
            "2"
        );
        assert_eq!(form_text(&FieldType::Float, Some(&json!(2))), "2");
        assert_eq!(
            form_text(&FieldType::Date, Some(&json!("2026-01-01T10:00:00Z"))),
            "2026-01-01"
        );
        assert_eq!(
            form_text(&FieldType::Date, Some(&json!(1_763_769_600))),
            "2025-11-22"
        );
        assert_eq!(
            form_text(
                &FieldType::RecordReference(String::new()),
                Some(&json!({"target_id": "u"}))
            ),
            "u"
        );
        assert!(form_checked(Some(&json!("1"))));
        assert!(form_checked(Some(&json!({"value": true}))));
        assert!(!form_checked(Some(&json!("0"))));
        assert_eq!(form_format(Some(&json!("plain"))), "plain_text");
        assert_eq!(form_format(None), "filtered_html");
    }

    #[test]
    fn a_browser_round_trip_is_unchanged() {
        let body = json!({"value": "a\nb\n", "format": "full_html"});
        assert!(is_unchanged(
            &FieldType::TextLong,
            Some(&body),
            "a\r\nb\r\n",
            Some("full_html")
        ));
        assert!(!is_unchanged(
            &FieldType::TextLong,
            Some(&body),
            "a\r\nb\r\n",
            Some("plain_text")
        ));
        assert!(is_unchanged(
            &text(),
            Some(&json!("line\nbreak")),
            "linebreak",
            None
        ));
        assert!(is_unchanged(
            &FieldType::Email,
            Some(&json!(" a@b.c ")),
            "a@b.c",
            None
        ));
        assert!(is_unchanged(&FieldType::Boolean, None, "", None));
        assert!(is_unchanged(&FieldType::Blocks, None, "[]", None));
        assert!(is_unchanged(
            &FieldType::PageBuilder,
            Some(&json!({"root": {}, "content": []})),
            r#"{"content":[],"root":{}}"#,
            None
        ));
    }

    #[test]
    fn a_changed_value_keeps_the_stored_shape() {
        assert_eq!(
            reshape(Some(&json!({"value": "a"})), json!("b")),
            json!({"value": "b"})
        );
        assert_eq!(
            reshape(
                Some(&json!({"value": "a", "format": "full_html", "summary": "s"})),
                json!({"value": "b", "format": "plain_text"})
            ),
            json!({"value": "b", "format": "plain_text", "summary": "s"})
        );
        assert_eq!(
            reshape(
                Some(&json!({"target_id": "u1", "target_type": "t", "title": "Old"})),
                json!("u2")
            ),
            json!({"target_id": "u2", "target_type": "t"})
        );
        assert_eq!(reshape(Some(&json!("a")), json!("b")), json!("b"));
        assert_eq!(reshape(None, json!(5)), json!(5));
    }

    #[test]
    fn json_values_are_checked_by_the_same_reading() {
        let f = |t: FieldType| FieldDefinition::new("f", t).label("F");
        let formats = vec!["plain_text".to_string()];
        assert!(check_json(&f(FieldType::Integer), &json!({"value": 2}), &formats).is_ok());
        assert!(check_json(&f(FieldType::Integer), &json!("seven"), &formats).is_err());
        assert!(check_json(&f(text()), &json!({"a": 1}), &formats).is_err());
        assert!(
            check_json(
                &f(FieldType::TextLong),
                &json!({"value": "x", "format": "full_html"}),
                &formats
            )
            .is_err()
        );
        assert!(
            check_json(
                &f(FieldType::TextLong),
                &json!({"value": "x", "format": "plain_text"}),
                &formats
            )
            .is_ok()
        );
        assert!(check_json(&f(FieldType::Blocks), &json!({}), &formats).is_err());
        assert!(check_json(&f(text()).cardinality(2), &json!(["a", "b", "c"]), &formats).is_err());
        assert!(
            check_json(
                &f(FieldType::RecordReference(String::new())),
                &json!([{"target_id": "u"}]),
                &formats
            )
            .is_ok()
        );
    }
}
