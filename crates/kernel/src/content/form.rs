//! Auto-generated admin forms.
//!
//! Generates HTML forms from content type field definitions.
//! This is a temporary solution until the full Form API is built in Epic 9.

use crate::models::Item;
use crate::routes::helpers::html_escape;
use trovato_sdk::types::{ContentTypeDefinition, FieldDefinition, FieldType};

/// Builder for auto-generated forms.
pub struct FormBuilder {
    content_type: ContentTypeDefinition,
    /// Text formats the current user is permitted to use.
    /// When empty, all formats are shown (backwards compat).
    permitted_formats: Vec<String>,
}

impl FormBuilder {
    /// Create a new form builder for a content type.
    pub fn new(content_type: ContentTypeDefinition) -> Self {
        Self {
            content_type,
            permitted_formats: Vec::new(),
        }
    }

    /// Set the permitted text formats for the current user.
    ///
    /// Only formats in this list will appear in format selectors.
    /// `plain_text` is always allowed. If the list is empty, all formats are shown.
    pub fn with_permitted_formats(mut self, formats: Vec<String>) -> Self {
        self.permitted_formats = formats;
        self
    }

    /// Check whether a format should be shown in the selector.
    fn is_format_permitted(&self, format: &str) -> bool {
        if self.permitted_formats.is_empty() {
            return true;
        }
        // plain_text is always allowed
        if format == "plain_text" {
            return true;
        }
        self.permitted_formats.contains(&format.to_string())
    }

    /// Generate an add form for creating new items.
    pub fn build_add_form(&self, action: &str) -> String {
        let mut html = String::new();

        html.push_str(&format!(
            r#"<form method="post" action="{}" class="item-form item-form-add">"#,
            html_escape(action)
        ));

        // Title field (always present)
        html.push_str(
            r#"
            <div class="form-group">
                <label for="title">Title</label>
                <input type="text" id="title" name="title" required class="form-control">
            </div>
        "#,
        );

        // Dynamic fields
        for field in &self.content_type.fields {
            html.push_str(&self.render_field(field, None));
        }

        // Status field
        html.push_str(
            r#"
            <div class="form-group">
                <label>
                    <input type="checkbox" name="status" value="1" checked>
                    Published
                </label>
            </div>
        "#,
        );

        // Submit button
        html.push_str(
            r#"
            <div class="form-actions">
                <button type="submit" class="btn btn-primary">Save</button>
            </div>
        </form>
        "#,
        );

        html
    }

    /// Generate an edit form for updating existing items.
    pub fn build_edit_form(&self, item: &Item, action: &str) -> String {
        let mut html = String::new();

        html.push_str(&format!(
            r#"<form method="post" action="{}" class="item-form item-form-edit">"#,
            html_escape(action)
        ));

        // Title field
        html.push_str(&format!(
            r#"
            <div class="form-group">
                <label for="title">Title</label>
                <input type="text" id="title" name="title" value="{}" required class="form-control">
            </div>
            "#,
            html_escape(&item.title)
        ));

        // Dynamic fields with existing values
        for field in &self.content_type.fields {
            let value = item.fields.get(&field.field_name);
            html.push_str(&self.render_field(field, value));
        }

        // Status field
        let checked = if item.is_published() { "checked" } else { "" };
        html.push_str(&format!(
            r#"
            <div class="form-group">
                <label>
                    <input type="checkbox" name="status" value="1" {checked}>
                    Published
                </label>
            </div>
            "#
        ));

        // Revision log
        html.push_str(r#"
            <div class="form-group">
                <label for="log">Revision log message</label>
                <input type="text" id="log" name="log" class="form-control" placeholder="Describe your changes...">
            </div>
        "#);

        // Submit button
        html.push_str(
            r#"
            <div class="form-actions">
                <button type="submit" class="btn btn-primary">Save</button>
            </div>
        </form>
        "#,
        );

        html
    }

    /// Render a single field based on its type.
    fn render_field(&self, field: &FieldDefinition, value: Option<&serde_json::Value>) -> String {
        let field_name = &field.field_name;
        let label = &field.label;
        let required = if field.required { "required" } else { "" };
        let required_star = if field.required { " *" } else { "" };

        match &field.field_type {
            FieldType::Text { max_length } => {
                let max = max_length
                    .map(|m| format!(r#"maxlength="{m}""#))
                    .unwrap_or_default();
                let val = extract_text_value(value);
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="text" id="{field_name}" name="{field_name}" value="{val}" {required} {max} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::TextLong => {
                let val = extract_text_value(value);
                let format = extract_format_value(value);

                // Build format options based on permissions
                let mut format_options = String::new();
                if self.is_format_permitted("filtered_html") {
                    let sel = if format == "filtered_html" {
                        "selected"
                    } else {
                        ""
                    };
                    format_options.push_str(&std::format!(
                        r#"<option value="filtered_html" {sel}>Filtered HTML</option>"#
                    ));
                }
                if self.is_format_permitted("full_html") {
                    let sel = if format == "full_html" {
                        "selected"
                    } else {
                        ""
                    };
                    format_options.push_str(&std::format!(
                        r#"<option value="full_html" {sel}>Full HTML</option>"#
                    ));
                }
                {
                    let sel = if format == "plain_text" {
                        "selected"
                    } else {
                        ""
                    };
                    format_options.push_str(&std::format!(
                        r#"<option value="plain_text" {sel}>Plain Text</option>"#
                    ));
                }

                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <textarea id="{field_name}" name="{field_name}" rows="10" {required} class="form-control">{val}</textarea>
                        <div class="form-help">
                            <select name="{field_name}_format" class="form-control-sm">
                                {format_options}
                            </select>
                        </div>
                    </div>
                    "#
                )
            }

            FieldType::Integer => {
                let val = value
                    .and_then(|v| v.get("value"))
                    .and_then(|v| v.as_i64())
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="number" id="{field_name}" name="{field_name}" value="{val}" {required} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::Float => {
                let val = value
                    .and_then(|v| v.get("value"))
                    .and_then(|v| v.as_f64())
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="number" id="{field_name}" name="{field_name}" value="{val}" step="any" {required} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::Boolean => {
                let checked = value
                    .and_then(|v| v.get("value"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let checked_attr = if checked { "checked" } else { "" };
                format!(
                    r#"
                    <div class="form-group">
                        <label>
                            <input type="checkbox" id="{field_name}" name="{field_name}" value="1" {checked_attr}>
                            {label}
                        </label>
                    </div>
                    "#
                )
            }

            FieldType::Date => {
                let val = value
                    .and_then(|v| v.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="date" id="{field_name}" name="{field_name}" value="{val}" {required} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::Email => {
                let val = extract_text_value(value);
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="email" id="{field_name}" name="{field_name}" value="{val}" {required} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::RecordReference(_target_type) => {
                // For now, just a text input for UUID
                // TODO: Implement autocomplete/select when query API is ready
                let val = value
                    .and_then(|v| v.get("target_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="text" id="{field_name}" name="{field_name}" value="{val}" {required} class="form-control" placeholder="UUID">
                        <div class="form-help">Enter the UUID of the referenced item.</div>
                    </div>
                    "#
                )
            }

            FieldType::File => {
                // File upload - simplified for MVP
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="file" id="{field_name}" name="{field_name}" {required} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::Compound {
                allowed_types,
                min_items,
                max_items,
            } => {
                // Build config JSON (Compound type constraints only)
                let config = serde_json::json!({
                    "allowed_types": allowed_types,
                    "min_items": min_items,
                    "max_items": max_items,
                });
                let config_json = html_escape(&serde_json::to_string(&config).unwrap_or_default());

                // Build section type schemas JSON separately
                let section_types =
                    crate::content::compound::parse_section_schemas(&field.settings);
                let section_types_json =
                    html_escape(&serde_json::to_string(&section_types).unwrap_or_default());

                // Serialize existing value for hidden input
                let existing_json = value
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .unwrap_or_else(|| r#"{"sections":[]}"#.to_string());
                let existing_escaped = html_escape(&existing_json);

                format!(
                    r#"
                    <div class="form-group">
                        <label>{label}{required_star}</label>
                        <div class="compound-field" id="compound-{field_name}" data-field="{field_name}" data-config="{config_json}" data-section-types="{section_types_json}">
                            <div class="compound-field__sections"></div>
                            <input type="hidden" name="{field_name}" class="compound-field__value" value="{existing_escaped}">
                            <div class="compound-field__actions">
                                <button type="button" class="button compound-field__add">Add section</button>
                            </div>
                        </div>
                    </div>
                    "#
                )
            }
        }
    }
}

/// Extract text value from field JSON.
fn extract_text_value(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_str())
        .map(html_escape)
        .unwrap_or_default()
}

/// Extract format from field JSON.
fn extract_format_value(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| v.get("format"))
        .and_then(|v| v.as_str())
        .unwrap_or("filtered_html")
        .to_string()
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn test_content_type() -> ContentTypeDefinition {
        ContentTypeDefinition {
            machine_name: "blog".to_string(),
            label: "Blog Post".to_string(),
            description: "A blog article".to_string(),
            fields: vec![
                FieldDefinition {
                    field_name: "body".to_string(),
                    field_type: FieldType::TextLong,
                    label: "Body".to_string(),
                    required: true,
                    cardinality: 1,
                    settings: serde_json::json!({}),
                },
                FieldDefinition {
                    field_name: "summary".to_string(),
                    field_type: FieldType::Text {
                        max_length: Some(255),
                    },
                    label: "Summary".to_string(),
                    required: false,
                    cardinality: 1,
                    settings: serde_json::json!({}),
                },
            ],
        }
    }

    #[test]
    fn build_add_form_includes_title() {
        let builder = FormBuilder::new(test_content_type());
        let form = builder.build_add_form("/item/add/blog");
        assert!(form.contains(r#"name="title""#));
        assert!(form.contains(r#"action="/item/add/blog""#));
    }

    #[test]
    fn build_add_form_includes_fields() {
        let builder = FormBuilder::new(test_content_type());
        let form = builder.build_add_form("/item/add/blog");
        assert!(form.contains(r#"name="body""#));
        assert!(form.contains(r#"name="summary""#));
        assert!(form.contains("textarea")); // TextLong
    }

    #[test]
    fn html_escape_works() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape(r#"a="b""#), "a=&quot;b&quot;");
    }

    #[test]
    fn html_escape_single_quote() {
        assert_eq!(html_escape("it's"), "it&#x27;s");
    }

    #[test]
    fn html_escape_ampersand() {
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn extract_text_value_some() {
        let val = serde_json::json!({"value": "Hello"});
        assert_eq!(extract_text_value(Some(&val)), "Hello");
    }

    #[test]
    fn extract_text_value_none() {
        assert_eq!(extract_text_value(None), "");
    }

    #[test]
    fn extract_text_value_escapes() {
        let val = serde_json::json!({"value": "<b>bold</b>"});
        let result = extract_text_value(Some(&val));
        assert!(result.contains("&lt;b&gt;"));
    }

    #[test]
    fn extract_format_default() {
        assert_eq!(extract_format_value(None), "filtered_html");
    }

    #[test]
    fn extract_format_specified() {
        let val = serde_json::json!({"format": "plain_text"});
        assert_eq!(extract_format_value(Some(&val)), "plain_text");
    }

    #[test]
    fn build_add_form_has_submit_button() {
        let builder = FormBuilder::new(test_content_type());
        let form = builder.build_add_form("/item/add/blog");
        assert!(form.contains(r#"type="submit""#));
        assert!(form.contains("Save"));
    }

    #[test]
    fn build_add_form_has_status_checkbox() {
        let builder = FormBuilder::new(test_content_type());
        let form = builder.build_add_form("/item/add/blog");
        assert!(form.contains(r#"name="status""#));
        assert!(form.contains("Published"));
    }

    #[test]
    fn compound_field_renders_container_and_hidden_input() {
        let ct = ContentTypeDefinition {
            machine_name: "page".to_string(),
            label: "Page".to_string(),
            description: "A page".to_string(),
            fields: vec![FieldDefinition {
                field_name: "sections".to_string(),
                field_type: FieldType::Compound {
                    allowed_types: vec!["text".to_string()],
                    min_items: None,
                    max_items: None,
                },
                label: "Sections".to_string(),
                required: false,
                cardinality: 1,
                settings: serde_json::json!({
                    "section_types": [{
                        "machine_name": "text",
                        "label": "Text",
                        "fields": []
                    }]
                }),
            }],
        };
        let builder = FormBuilder::new(ct);
        let form = builder.build_add_form("/item/add/page");
        assert!(
            form.contains("compound-field"),
            "should contain compound-field class"
        );
        assert!(
            form.contains(r#"data-field="sections""#),
            "should have data-field"
        );
        assert!(
            form.contains("compound-field__value"),
            "should have hidden input"
        );
        assert!(
            form.contains("compound-field__add"),
            "should have add button"
        );
        assert!(
            form.contains("data-section-types="),
            "should have data-section-types attribute"
        );
    }

    #[test]
    fn build_edit_form_has_log_field() {
        let builder = FormBuilder::new(test_content_type());
        let item = Item {
            id: uuid::Uuid::now_v7(),
            current_revision_id: None,
            item_type: "blog".to_string(),
            title: "Test".to_string(),
            author_id: uuid::Uuid::nil(),
            status: 1,
            created: 0,
            changed: 0,
            promote: 0,
            sticky: 0,
            fields: serde_json::json!({}),
            stage_id: "live".to_string(),
            language: "en".to_string(),
        };
        let form = builder.build_edit_form(&item, "/item/123/edit");
        assert!(form.contains(r#"name="log""#));
        assert!(form.contains("Revision log"));
    }
}
