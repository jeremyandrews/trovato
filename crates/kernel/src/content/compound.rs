//! Compound field validation.
//!
//! Validates compound field sections against their schema definitions.

use trovato_sdk::types::{CompoundSection, FieldDefinition, FieldType, SectionTypeSchema};

/// Validate compound field sections against the field definition.
///
/// Returns a list of error messages (empty if valid).
pub fn validate_compound_field(
    _field_name: &str,
    sections: &[CompoundSection],
    field_def: &FieldDefinition,
) -> Vec<String> {
    let mut errors = Vec::new();

    // Extract constraints from field type
    let (allowed_types, min_items, max_items) = match &field_def.field_type {
        FieldType::Compound {
            allowed_types,
            min_items,
            max_items,
        } => (allowed_types.clone(), *min_items, *max_items),
        _ => return errors,
    };

    // Enforce FieldDefinition.required: a required compound field must have at least one section
    if field_def.required && sections.is_empty() {
        errors.push(format!(
            "{}: at least one section is required",
            field_def.label
        ));
    }

    // Check min/max items
    let count = sections.len();
    if let Some(min) = min_items.filter(|&m| count < m) {
        errors.push(format!(
            "{}: requires at least {} section(s), found {}",
            field_def.label, min, count
        ));
    }
    if let Some(max) = max_items.filter(|&m| count > m) {
        errors.push(format!(
            "{}: allows at most {} section(s), found {}",
            field_def.label, max, count
        ));
        // Don't validate individual sections when count constraint is violated
        return errors;
    }

    // Parse section type schemas from settings
    let section_schemas = parse_section_schemas(&field_def.settings);

    // Validate each section (1-based numbering for user-facing messages)
    for (i, section) in sections.iter().enumerate() {
        let pos = i + 1; // 1-based for error messages

        // Check section type is allowed
        if !allowed_types.contains(&section.section_type) {
            errors.push(format!(
                "{}: section {} has unknown type '{}'",
                field_def.label, pos, section.section_type
            ));
            continue;
        }

        // Find matching schema
        let schema = section_schemas
            .iter()
            .find(|s| s.machine_name == section.section_type);

        let Some(schema) = schema else {
            errors.push(format!(
                "{}: no schema defined for section type '{}'",
                field_def.label, section.section_type
            ));
            continue;
        };

        // Validate section data against schema fields
        let data = section.data.as_object();
        for field_schema in &schema.fields {
            let sub_value = data.and_then(|d| d.get(&field_schema.field_name));

            if field_schema.required {
                let is_empty = match sub_value {
                    None => true,
                    Some(v) if v.is_null() => true,
                    Some(v) => {
                        // Check for empty string values (including nested {value: ""})
                        if let Some(s) = v.as_str() {
                            s.trim().is_empty()
                        } else if let Some(s) = v.get("value").and_then(|inner| inner.as_str()) {
                            s.trim().is_empty()
                        } else {
                            false
                        }
                    }
                };

                if is_empty {
                    errors.push(format!(
                        "{}: section {} ({}): '{}' is required",
                        field_def.label, pos, schema.label, field_schema.label
                    ));
                }
            }

            // Type-specific validation
            if let Some(val) = sub_value {
                validate_sub_field(
                    &field_def.label,
                    pos,
                    &field_schema.field_type,
                    val,
                    &field_schema.label,
                    &mut errors,
                );
            }
        }
    }

    errors
}

/// Extract a string from a value that may be raw or wrapped in `{value: "..."}`.
fn extract_str(value: &serde_json::Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("value").and_then(|v| v.as_str()))
}

/// Validate a sub-field value against its type.
/// `field_label` is the parent field's human-readable label.
/// `section_pos` is 1-based section position for error messages.
fn validate_sub_field(
    field_label: &str,
    section_pos: usize,
    field_type: &FieldType,
    value: &serde_json::Value,
    label: &str,
    errors: &mut Vec<String>,
) {
    match field_type {
        FieldType::Text { max_length } => {
            if let (Some(max), Some(text)) = (max_length, extract_str(value)) {
                // Use char count (not byte count) to match HTML maxlength behavior
                if text.chars().count() > *max {
                    errors.push(format!(
                        "{field_label}: section {section_pos} '{label}' exceeds max length of {max}"
                    ));
                }
            }
        }
        FieldType::Integer => {
            let num = value
                .as_i64()
                .or_else(|| value.get("value").and_then(|v| v.as_i64()));
            if num.is_none() && !value.is_null() {
                let is_invalid =
                    extract_str(value).is_some_and(|s| !s.is_empty() && s.parse::<i64>().is_err());
                if is_invalid {
                    errors.push(format!(
                        "{field_label}: section {section_pos} '{label}' must be an integer"
                    ));
                }
            }
        }
        FieldType::Float => {
            let num = value
                .as_f64()
                .or_else(|| value.get("value").and_then(|v| v.as_f64()));
            if num.is_none() && !value.is_null() {
                let is_invalid =
                    extract_str(value).is_some_and(|s| !s.is_empty() && s.parse::<f64>().is_err());
                if is_invalid {
                    errors.push(format!(
                        "{field_label}: section {section_pos} '{label}' must be a number"
                    ));
                }
            }
        }
        FieldType::Email => {
            if extract_str(value).is_some_and(|s| !s.is_empty() && !is_valid_email(s)) {
                errors.push(format!(
                    "{field_label}: section {section_pos} '{label}' must be a valid email address"
                ));
            }
        }
        FieldType::Date => {
            if extract_str(value).is_some_and(|s| !s.is_empty() && !is_valid_date(s)) {
                errors.push(format!(
                    "{field_label}: section {section_pos} '{label}' must be a valid date (YYYY-MM-DD)"
                ));
            }
        }
        FieldType::Boolean => {
            // Booleans are flexible — accept bool, number, or string "0"/"1"/"true"/"false"
        }
        _ => {
            // File, RecordReference: no additional format validation
        }
    }

    // Cross-cutting: validate format field if present in {value, format} structures.
    // Only "plain_text" and "filtered_html" are safe; reject others to prevent
    // bypassing text sanitization in the rendering pipeline.
    if value
        .get("format")
        .and_then(|f| f.as_str())
        .is_some_and(|fmt| !matches!(fmt, "plain_text" | "filtered_html"))
    {
        let fmt = value.get("format").and_then(|f| f.as_str()).unwrap_or("");
        errors.push(format!(
            "{field_label}: section {section_pos} '{label}' has unsupported format '{fmt}'"
        ));
    }
}

/// Check if a string is a valid YYYY-MM-DD date with proper month-day validation.
/// Enforces strict formatting: exactly 4-digit year, 2-digit month, 2-digit day.
fn is_valid_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    // Enforce strict digit counts: YYYY-MM-DD
    if parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    let Ok(y) = parts[0].parse::<u32>() else {
        return false;
    };
    let Ok(m) = parts[1].parse::<u32>() else {
        return false;
    };
    let Ok(d) = parts[2].parse::<u32>() else {
        return false;
    };
    if y < 1 || !(1..=12).contains(&m) || d < 1 {
        return false;
    }
    let max_day = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    d <= max_day
}

/// Basic email validation: requires exactly one `@` with non-empty local and domain parts,
/// and the domain must contain a dot. Matches HTML5 `type="email"` semantics loosely.
fn is_valid_email(s: &str) -> bool {
    let parts: Vec<&str> = s.splitn(3, '@').collect();
    if parts.len() != 2 {
        return false; // must have exactly one @
    }
    let local = parts[0];
    let domain = parts[1];
    !local.is_empty() && !domain.is_empty() && domain.contains('.')
}

/// Parse section type schemas from field definition settings.
pub fn parse_section_schemas(settings: &serde_json::Value) -> Vec<SectionTypeSchema> {
    let raw = settings.get("section_types");
    match raw.map(|v| serde_json::from_value::<Vec<SectionTypeSchema>>(v.clone())) {
        Some(Ok(schemas)) => schemas,
        Some(Err(e)) => {
            tracing::warn!(error = %e, "failed to parse section_types from field settings");
            Vec::new()
        }
        None => Vec::new(),
    }
}

/// Validate required non-compound fields have non-empty values.
///
/// Returns a list of validation error messages (empty if all valid).
pub fn validate_required_fields(
    fields_json: &serde_json::Map<String, serde_json::Value>,
    content_type_fields: &[FieldDefinition],
) -> Vec<String> {
    let mut errors = Vec::new();

    for field_def in content_type_fields {
        if !field_def.required || matches!(&field_def.field_type, FieldType::Compound { .. }) {
            continue;
        }

        let value = fields_json.get(&field_def.field_name);
        let is_empty = match value {
            None => true,
            Some(v) if v.is_null() => true,
            Some(v) => {
                if let Some(s) = v.as_str() {
                    s.trim().is_empty()
                } else if let Some(s) = v.get("value").and_then(|inner| inner.as_str()) {
                    s.trim().is_empty()
                } else {
                    false
                }
            }
        };

        if is_empty {
            errors.push(format!("{} is required.", field_def.label));
        }
    }

    errors
}

/// Strip unexpected keys from compound section data, keeping only fields
/// defined in the schema for each section type.
fn sanitize_compound_sections(
    parsed: &serde_json::Value,
    field_def: &FieldDefinition,
) -> serde_json::Value {
    let section_schemas = parse_section_schemas(&field_def.settings);

    let Some(sections) = parsed.get("sections").and_then(|s| s.as_array()) else {
        return parsed.clone();
    };

    let sanitized_sections: Vec<serde_json::Value> = sections
        .iter()
        .enumerate()
        .map(|(idx, section)| {
            let section_type = section.get("type").and_then(|t| t.as_str()).unwrap_or("");

            // Find schema for this section type
            let schema = section_schemas
                .iter()
                .find(|s| s.machine_name == section_type);

            let Some(schema) = schema else {
                // No schema found — strip all data keys as we cannot validate them
                let mut clean_section = serde_json::Map::new();
                clean_section.insert(
                    "type".to_string(),
                    serde_json::Value::String(section_type.to_string()),
                );
                clean_section.insert(
                    "weight".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(idx)),
                );
                clean_section.insert("data".to_string(), serde_json::json!({}));
                return serde_json::Value::Object(clean_section);
            };

            // Build allowed field names set
            let allowed_keys: std::collections::HashSet<&str> = schema
                .fields
                .iter()
                .map(|f| f.field_name.as_str())
                .collect();

            // Strip data keys not in schema
            let clean_data = if let Some(data) = section.get("data").and_then(|d| d.as_object()) {
                let mut filtered = serde_json::Map::new();
                for (k, v) in data {
                    if allowed_keys.contains(k.as_str()) {
                        filtered.insert(k.clone(), v.clone());
                    }
                }
                serde_json::Value::Object(filtered)
            } else {
                serde_json::json!({})
            };

            // Rebuild section with sanitized data and normalized weight
            let mut clean_section = serde_json::Map::new();
            clean_section.insert(
                "type".to_string(),
                serde_json::Value::String(section_type.to_string()),
            );
            clean_section.insert(
                "weight".to_string(),
                serde_json::Value::Number(serde_json::Number::from(idx)),
            );
            clean_section.insert("data".to_string(), clean_data);
            serde_json::Value::Object(clean_section)
        })
        .collect();

    serde_json::json!({ "sections": sanitized_sections })
}

/// Process compound fields from form submission: parse JSON strings from hidden
/// inputs, validate each compound field, and replace raw strings with parsed JSON.
///
/// Returns a list of validation error messages (empty if all valid).
/// Maximum byte size for a single compound field JSON payload (512 KB).
const MAX_COMPOUND_JSON_BYTES: usize = 512 * 1024;

/// Maximum number of sections allowed in a single compound field (prevents DoS via
/// algorithmic complexity in schema lookup and validation).
const MAX_COMPOUND_SECTIONS: usize = 100;

pub fn process_compound_fields(
    fields_json: &mut serde_json::Map<String, serde_json::Value>,
    content_type_fields: &[FieldDefinition],
) -> Vec<String> {
    let mut errors = Vec::new();

    for field_def in content_type_fields {
        if !matches!(&field_def.field_type, FieldType::Compound { .. }) {
            continue;
        }

        // Extract the raw JSON string from the hidden input (if present)
        let raw_str = fields_json
            .get(&field_def.field_name)
            .and_then(|v| v.as_str())
            .map(String::from);

        let Some(s) = raw_str else {
            // Field missing from form — check if required
            if field_def.required {
                errors.push(format!(
                    "{}: at least one section is required",
                    field_def.label
                ));
            }
            continue;
        };

        // Reject oversized payloads before parsing
        if s.len() > MAX_COMPOUND_JSON_BYTES {
            errors.push(format!(
                "{}: compound field data exceeds maximum size",
                field_def.label
            ));
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(parsed) => {
                let sections: Vec<CompoundSection> = parsed
                    .get("sections")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                if sections.len() > MAX_COMPOUND_SECTIONS {
                    errors.push(format!(
                        "{}: too many sections (maximum {})",
                        field_def.label, MAX_COMPOUND_SECTIONS
                    ));
                    continue;
                }

                let field_errors =
                    validate_compound_field(&field_def.field_name, &sections, field_def);
                errors.extend(field_errors);

                // Strip unexpected keys from section data based on schema
                let sanitized = sanitize_compound_sections(&parsed, field_def);
                fields_json.insert(field_def.field_name.clone(), sanitized);
            }
            Err(_) => {
                errors.push(format!("{}: invalid compound field data", field_def.label));
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use trovato_sdk::types::{SectionFieldSchema, SectionTypeSchema};

    fn make_field_def(
        allowed_types: Vec<&str>,
        min_items: Option<usize>,
        max_items: Option<usize>,
        section_types: Vec<SectionTypeSchema>,
    ) -> FieldDefinition {
        make_field_def_with_required(allowed_types, min_items, max_items, section_types, false)
    }

    fn make_field_def_with_required(
        allowed_types: Vec<&str>,
        min_items: Option<usize>,
        max_items: Option<usize>,
        section_types: Vec<SectionTypeSchema>,
        required: bool,
    ) -> FieldDefinition {
        FieldDefinition {
            field_name: "body".to_string(),
            field_type: FieldType::Compound {
                allowed_types: allowed_types.into_iter().map(String::from).collect(),
                min_items,
                max_items,
            },
            label: "Body".to_string(),
            required,
            cardinality: 1,
            settings: serde_json::json!({
                "section_types": section_types,
            }),
        }
    }

    fn text_schema() -> SectionTypeSchema {
        SectionTypeSchema {
            machine_name: "text".to_string(),
            label: "Text".to_string(),
            fields: vec![SectionFieldSchema {
                field_name: "body".to_string(),
                field_type: FieldType::TextLong,
                label: "Body".to_string(),
                required: true,
            }],
        }
    }

    fn image_schema() -> SectionTypeSchema {
        SectionTypeSchema {
            machine_name: "image".to_string(),
            label: "Image".to_string(),
            fields: vec![
                SectionFieldSchema {
                    field_name: "file_id".to_string(),
                    field_type: FieldType::Text { max_length: None },
                    label: "File".to_string(),
                    required: true,
                },
                SectionFieldSchema {
                    field_name: "alt".to_string(),
                    field_type: FieldType::Text {
                        max_length: Some(255),
                    },
                    label: "Alt text".to_string(),
                    required: false,
                },
            ],
        }
    }

    #[test]
    fn valid_compound_passes() {
        let field_def = make_field_def(
            vec!["text", "image"],
            None,
            None,
            vec![text_schema(), image_schema()],
        );
        let sections = vec![
            CompoundSection {
                section_type: "text".to_string(),
                weight: 0,
                data: serde_json::json!({"body": {"value": "Hello world", "format": "filtered_html"}}),
            },
            CompoundSection {
                section_type: "image".to_string(),
                weight: 1,
                data: serde_json::json!({"file_id": "abc-123", "alt": "A photo"}),
            },
        ];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn empty_sections_passes_with_no_min() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        let errors = validate_compound_field("body", &[], &field_def);
        assert!(errors.is_empty());
    }

    #[test]
    fn min_items_violation() {
        let field_def = make_field_def(vec!["text"], Some(2), None, vec![text_schema()]);
        let sections = vec![CompoundSection {
            section_type: "text".to_string(),
            weight: 0,
            data: serde_json::json!({"body": {"value": "Hello", "format": "plain_text"}}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("at least 2"));
    }

    #[test]
    fn max_items_violation() {
        let field_def = make_field_def(vec!["text"], None, Some(1), vec![text_schema()]);
        let sections = vec![
            CompoundSection {
                section_type: "text".to_string(),
                weight: 0,
                data: serde_json::json!({"body": {"value": "A", "format": "plain_text"}}),
            },
            CompoundSection {
                section_type: "text".to_string(),
                weight: 1,
                data: serde_json::json!({"body": {"value": "B", "format": "plain_text"}}),
            },
        ];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("at most 1"));
    }

    #[test]
    fn unknown_section_type() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        let sections = vec![CompoundSection {
            section_type: "video".to_string(),
            weight: 0,
            data: serde_json::json!({}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("unknown type 'video'"));
    }

    #[test]
    fn missing_required_sub_field() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        let sections = vec![CompoundSection {
            section_type: "text".to_string(),
            weight: 0,
            data: serde_json::json!({}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("is required"));
    }

    #[test]
    fn empty_required_sub_field() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        let sections = vec![CompoundSection {
            section_type: "text".to_string(),
            weight: 0,
            data: serde_json::json!({"body": {"value": "", "format": "plain_text"}}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("is required"));
    }

    #[test]
    fn text_max_length_violation() {
        let schema = SectionTypeSchema {
            machine_name: "short".to_string(),
            label: "Short text".to_string(),
            fields: vec![SectionFieldSchema {
                field_name: "title".to_string(),
                field_type: FieldType::Text {
                    max_length: Some(5),
                },
                label: "Title".to_string(),
                required: false,
            }],
        };
        let field_def = make_field_def(vec!["short"], None, None, vec![schema]);
        let sections = vec![CompoundSection {
            section_type: "short".to_string(),
            weight: 0,
            data: serde_json::json!({"title": "Too long text"}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("max length"));
    }

    #[test]
    fn required_compound_rejects_empty() {
        let field_def =
            make_field_def_with_required(vec!["text"], None, None, vec![text_schema()], true);
        let errors = validate_compound_field("body", &[], &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("at least one section is required"));
    }

    #[test]
    fn required_compound_accepts_non_empty() {
        let field_def =
            make_field_def_with_required(vec!["text"], None, None, vec![text_schema()], true);
        let sections = vec![CompoundSection {
            section_type: "text".to_string(),
            weight: 0,
            data: serde_json::json!({"body": {"value": "Hello", "format": "plain_text"}}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn max_length_counts_chars_not_bytes() {
        // 5 chars that are multi-byte in UTF-8: "héllo" (6 bytes, 5 chars)
        let schema = SectionTypeSchema {
            machine_name: "short".to_string(),
            label: "Short text".to_string(),
            fields: vec![SectionFieldSchema {
                field_name: "title".to_string(),
                field_type: FieldType::Text {
                    max_length: Some(5),
                },
                label: "Title".to_string(),
                required: false,
            }],
        };
        let field_def = make_field_def(vec!["short"], None, None, vec![schema]);
        let sections = vec![CompoundSection {
            section_type: "short".to_string(),
            weight: 0,
            // "héllo" = 5 chars, 6 bytes — should pass max_length of 5
            data: serde_json::json!({"title": "h\u{00e9}llo"}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert!(
            errors.is_empty(),
            "Expected no errors for 5-char string, got: {errors:?}"
        );
    }

    #[test]
    fn float_rejects_non_numeric_string() {
        let schema = SectionTypeSchema {
            machine_name: "metric".to_string(),
            label: "Metric".to_string(),
            fields: vec![SectionFieldSchema {
                field_name: "value".to_string(),
                field_type: FieldType::Float,
                label: "Value".to_string(),
                required: false,
            }],
        };
        let field_def = make_field_def(vec!["metric"], None, None, vec![schema]);
        let sections = vec![CompoundSection {
            section_type: "metric".to_string(),
            weight: 0,
            data: serde_json::json!({"value": "not-a-number"}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("must be a number"));
    }

    #[test]
    fn float_accepts_valid_number() {
        let schema = SectionTypeSchema {
            machine_name: "metric".to_string(),
            label: "Metric".to_string(),
            fields: vec![SectionFieldSchema {
                field_name: "value".to_string(),
                field_type: FieldType::Float,
                label: "Value".to_string(),
                required: false,
            }],
        };
        let field_def = make_field_def(vec!["metric"], None, None, vec![schema]);
        let sections = vec![CompoundSection {
            section_type: "metric".to_string(),
            weight: 0,
            data: serde_json::json!({"value": "3.14"}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn sanitize_strips_unexpected_keys() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        let parsed = serde_json::json!({
            "sections": [{
                "type": "text",
                "weight": 0,
                "data": {
                    "body": {"value": "Hello", "format": "plain_text"},
                    "injected_key": "malicious data",
                    "extra": 42
                }
            }]
        });
        let result = sanitize_compound_sections(&parsed, &field_def);
        let sections = result.get("sections").unwrap().as_array().unwrap();
        let data = sections[0].get("data").unwrap().as_object().unwrap();
        assert!(
            data.contains_key("body"),
            "Should keep 'body' (schema field)"
        );
        assert!(
            !data.contains_key("injected_key"),
            "Should strip 'injected_key'"
        );
        assert!(!data.contains_key("extra"), "Should strip 'extra'");
    }

    #[test]
    fn validate_required_fields_catches_missing() {
        let field_defs = vec![FieldDefinition {
            field_name: "summary".to_string(),
            field_type: FieldType::Text { max_length: None },
            label: "Summary".to_string(),
            required: true,
            cardinality: 1,
            settings: serde_json::json!({}),
        }];
        let fields = serde_json::Map::new();
        let errors = validate_required_fields(&fields, &field_defs);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Summary is required"));
    }

    #[test]
    fn validate_required_fields_passes_with_value() {
        let field_defs = vec![FieldDefinition {
            field_name: "summary".to_string(),
            field_type: FieldType::Text { max_length: None },
            label: "Summary".to_string(),
            required: true,
            cardinality: 1,
            settings: serde_json::json!({}),
        }];
        let mut fields = serde_json::Map::new();
        fields.insert("summary".to_string(), serde_json::json!("A summary"));
        let errors = validate_required_fields(&fields, &field_defs);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_required_fields_skips_compound() {
        let field_defs = vec![make_field_def_with_required(
            vec!["text"],
            None,
            None,
            vec![text_schema()],
            true,
        )];
        // Compound fields are handled by process_compound_fields, not validate_required_fields
        let fields = serde_json::Map::new();
        let errors = validate_required_fields(&fields, &field_defs);
        assert!(errors.is_empty(), "Should skip compound fields");
    }

    #[test]
    fn date_rejects_impossible_dates() {
        assert!(!is_valid_date("2025-02-29")); // not a leap year
        assert!(!is_valid_date("2025-04-31")); // April has 30 days
        assert!(!is_valid_date("2025-06-31")); // June has 30 days
        assert!(!is_valid_date("2025-02-30")); // February never has 30
    }

    #[test]
    fn date_accepts_valid_dates() {
        assert!(is_valid_date("2024-02-29")); // leap year
        assert!(is_valid_date("2025-01-31")); // January 31
        assert!(is_valid_date("2025-12-25")); // Christmas
        assert!(is_valid_date("2000-02-29")); // divisible by 400
    }

    #[test]
    fn date_rejects_feb_29_on_century_non_leap() {
        assert!(!is_valid_date("1900-02-29")); // divisible by 100 but not 400
    }

    #[test]
    fn date_rejects_non_padded_format() {
        assert!(!is_valid_date("2025-1-1")); // single-digit month and day
        assert!(!is_valid_date("2025-1-15")); // single-digit month
        assert!(!is_valid_date("2025-12-1")); // single-digit day
        assert!(!is_valid_date("25-01-15")); // two-digit year
    }

    #[test]
    fn email_rejects_trivially_invalid() {
        assert!(!is_valid_email("@")); // no local part
        assert!(!is_valid_email("@@@@")); // multiple @
        assert!(!is_valid_email("user@")); // no domain
        assert!(!is_valid_email("@domain.com")); // no local
        assert!(!is_valid_email("user@nodot")); // no dot in domain
        assert!(!is_valid_email("just a string")); // no @
    }

    #[test]
    fn email_accepts_valid() {
        assert!(is_valid_email("user@example.com"));
        assert!(is_valid_email("a@b.c"));
    }

    #[test]
    fn sanitize_strips_data_for_schema_less_sections() {
        // allowed_types includes "quote" but no schema is defined for it
        let field_def = make_field_def(vec!["text", "quote"], None, None, vec![text_schema()]);
        let parsed = serde_json::json!({
            "sections": [{
                "type": "quote",
                "weight": 5,
                "data": {
                    "injected": "malicious data",
                    "another": "value"
                }
            }]
        });
        let result = sanitize_compound_sections(&parsed, &field_def);
        let sections = result.get("sections").unwrap().as_array().unwrap();
        let data = sections[0].get("data").unwrap().as_object().unwrap();
        assert!(
            data.is_empty(),
            "Should strip all data for schema-less section types"
        );
    }

    #[test]
    fn sanitize_normalizes_weights() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        let parsed = serde_json::json!({
            "sections": [
                {"type": "text", "weight": 99, "data": {"body": "A"}},
                {"type": "text", "weight": -5, "data": {"body": "B"}},
                {"type": "text", "weight": 99, "data": {"body": "C"}}
            ]
        });
        let result = sanitize_compound_sections(&parsed, &field_def);
        let sections = result.get("sections").unwrap().as_array().unwrap();
        assert_eq!(sections[0].get("weight").unwrap().as_i64().unwrap(), 0);
        assert_eq!(sections[1].get("weight").unwrap().as_i64().unwrap(), 1);
        assert_eq!(sections[2].get("weight").unwrap().as_i64().unwrap(), 2);
    }

    #[test]
    fn process_compound_fields_parses_json_string() {
        let field_defs = vec![FieldDefinition {
            field_name: "body".to_string(),
            field_type: FieldType::Compound {
                allowed_types: vec!["text".to_string()],
                min_items: None,
                max_items: None,
            },
            label: "Body".to_string(),
            required: false,
            cardinality: 1,
            settings: serde_json::json!({
                "section_types": [text_schema()],
            }),
        }];
        let mut fields = serde_json::Map::new();
        fields.insert(
            "body".to_string(),
            serde_json::Value::String(
                r#"{"sections":[{"type":"text","weight":0,"data":{"body":{"value":"Hello","format":"plain_text"}}}]}"#.to_string(),
            ),
        );
        let errors = process_compound_fields(&mut fields, &field_defs);
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
        // The raw string should have been replaced with parsed JSON
        assert!(fields["body"].is_object(), "Should be parsed JSON object");
    }

    fn compound_field_def() -> FieldDefinition {
        FieldDefinition {
            field_name: "body".to_string(),
            field_type: FieldType::Compound {
                allowed_types: vec!["text".to_string()],
                min_items: None,
                max_items: None,
            },
            label: "Body".to_string(),
            required: false,
            cardinality: 1,
            settings: serde_json::json!({
                "section_types": [text_schema()],
            }),
        }
    }

    #[test]
    fn process_rejects_oversized_payload() {
        let field_defs = vec![compound_field_def()];
        // Create a string larger than MAX_COMPOUND_JSON_BYTES (512KB)
        let huge = "x".repeat(513 * 1024);
        let mut fields = serde_json::Map::new();
        fields.insert("body".to_string(), serde_json::Value::String(huge));
        let errors = process_compound_fields(&mut fields, &field_defs);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("exceeds maximum size"));
    }

    #[test]
    fn process_rejects_too_many_sections() {
        let field_defs = vec![compound_field_def()];
        // Build 101 sections (over MAX_COMPOUND_SECTIONS = 100)
        let sections: Vec<serde_json::Value> = (0..101)
            .map(|i| {
                serde_json::json!({
                    "type": "text",
                    "weight": i,
                    "data": {"body": {"value": "text", "format": "plain_text"}}
                })
            })
            .collect();
        let json_str = serde_json::to_string(&serde_json::json!({ "sections": sections })).unwrap();
        let mut fields = serde_json::Map::new();
        fields.insert("body".to_string(), serde_json::Value::String(json_str));
        let errors = process_compound_fields(&mut fields, &field_defs);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("too many sections"));
    }

    #[test]
    fn process_rejects_malformed_json() {
        let field_defs = vec![compound_field_def()];
        let mut fields = serde_json::Map::new();
        fields.insert(
            "body".to_string(),
            serde_json::Value::String("not valid json {{{".to_string()),
        );
        let errors = process_compound_fields(&mut fields, &field_defs);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("invalid compound field data"));
    }

    #[test]
    fn validate_rejects_unsupported_format_in_sub_field() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        let sections = vec![CompoundSection {
            section_type: "text".to_string(),
            weight: 0,
            data: serde_json::json!({"body": {"value": "Hello", "format": "full_html"}}),
        }];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("unsupported format"));
    }

    #[test]
    fn validate_accepts_safe_formats() {
        let field_def = make_field_def(vec!["text"], None, None, vec![text_schema()]);
        for fmt in &["plain_text", "filtered_html"] {
            let sections = vec![CompoundSection {
                section_type: "text".to_string(),
                weight: 0,
                data: serde_json::json!({"body": {"value": "Hello", "format": fmt}}),
            }];
            let errors = validate_compound_field("body", &sections, &field_def);
            assert!(
                errors.is_empty(),
                "Expected no errors for format '{fmt}', got: {errors:?}"
            );
        }
    }

    #[test]
    fn max_items_early_return_skips_per_section_validation() {
        // max_items=1 with 2 sections, second section has invalid data.
        // Should only get the max_items error, not per-section errors.
        let field_def = make_field_def(vec!["text"], None, Some(1), vec![text_schema()]);
        let sections = vec![
            CompoundSection {
                section_type: "text".to_string(),
                weight: 0,
                data: serde_json::json!({"body": {"value": "A", "format": "plain_text"}}),
            },
            CompoundSection {
                section_type: "text".to_string(),
                weight: 1,
                // Missing required "body" field — would trigger error if validated
                data: serde_json::json!({}),
            },
        ];
        let errors = validate_compound_field("body", &sections, &field_def);
        assert_eq!(
            errors.len(),
            1,
            "Expected only max_items error, got: {errors:?}"
        );
        assert!(errors[0].contains("at most 1"));
    }
}
