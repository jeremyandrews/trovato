//! The long text field one content type calls `body` and another calls `field_body`.
//!
//! The kernel's `page` type declares `body`; `trovato_blog` declares
//! `field_body`. Both are legitimate names for the same thing, and every kernel
//! reader that renders this field for a person was written against one name or
//! the other, so each of them rendered nothing for half the site's content: a
//! blog teaser with no text, a `page` with no meta description. That is BL-21.
//!
//! Renaming either field is a content-model change for existing sites, needing a
//! migration for `item.fields`, `search_field_config` and every plugin reading
//! the other name, so nothing is renamed here. Instead every kernel reader goes
//! through [`body_text`], which reads both names.

use serde_json::Value;

/// The two names, in the order they are tried.
///
/// `field_body` comes first because it is the name a content type declares
/// explicitly through `tap_item_info`, while `body` is the kernel's own default.
/// An item carrying both is a content model that declared its own field, so the
/// declared one wins.
pub const BODY_FIELD_NAMES: [&str; 2] = ["field_body", "body"];

/// Read one field's text out of an item's `fields` object.
///
/// A field value is either `{"value": "..."}` or a bare string; both shapes are
/// stored in practice, and a field present but empty reads as absent, since an
/// empty teaser and no teaser render the same.
pub fn field_text(fields: &Value, name: &str) -> Option<String> {
    let value = fields.get(name)?;
    let text = value
        .get("value")
        .and_then(|v| v.as_str())
        .or_else(|| value.as_str())?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// The name this item carries its long text under, if it carries any.
///
/// For a reader that needs more than the text — the `format` alongside it, say —
/// so that it reads every part of the field from the same name.
pub fn body_field_name(fields: &Value) -> Option<&'static str> {
    BODY_FIELD_NAMES
        .into_iter()
        .find(|name| field_text(fields, name).is_some())
}

/// The item's long text, under whichever of the two names it carries.
pub fn body_text(fields: &Value) -> Option<String> {
    BODY_FIELD_NAMES
        .iter()
        .find_map(|name| field_text(fields, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn either_name_is_read() {
        for name in BODY_FIELD_NAMES {
            let fields = serde_json::json!({ name: { "value": "The text." } });
            assert_eq!(
                body_text(&fields).as_deref(),
                Some("The text."),
                "{name} must be read"
            );
            assert_eq!(body_field_name(&fields), Some(name));
        }
    }

    #[test]
    fn a_bare_string_is_a_field_value() {
        let fields = serde_json::json!({ "body": "Bare." });
        assert_eq!(body_text(&fields).as_deref(), Some("Bare."));
    }

    #[test]
    fn the_declared_field_wins_over_the_kernel_default() {
        let fields = serde_json::json!({
            "field_body": { "value": "Declared." },
            "body": { "value": "Default." },
        });
        assert_eq!(body_text(&fields).as_deref(), Some("Declared."));
        assert_eq!(body_field_name(&fields), Some("field_body"));
    }

    #[test]
    fn an_empty_field_reads_as_absent() {
        let fields = serde_json::json!({ "body": { "value": "" } });
        assert_eq!(body_text(&fields), None);
        assert_eq!(body_field_name(&fields), None);
    }

    #[test]
    fn neither_name_present_is_none() {
        let fields = serde_json::json!({ "field_summary": { "value": "Other." } });
        assert_eq!(body_text(&fields), None);
    }
}
