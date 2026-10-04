//! Auto-generated admin forms.
//!
//! Generates HTML forms from content type field definitions.
//! This is a temporary solution until the full Form API is built in Epic 9.

use crate::models::Item;
use crate::routes::helpers::html_escape;
use trovato_sdk::types::{ContentTypeDefinition, FieldDefinition, FieldType};

/// How one field is rendered on an item form.
///
/// A form used to render every field of the type with its current value, which
/// made the edit form a read of every field the viewer might not be allowed to
/// see: field-level access was enforced on the view paths and nowhere near the
/// form. The mode is set from the two field-access decisions the route asks
/// for, `"view"` and `"edit"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FieldMode {
    /// Rendered as an input the browser submits. The default, so a type whose
    /// fields nothing governs renders exactly as it did.
    #[default]
    Editable,
    /// Rendered with its value and the `disabled` attribute. A browser does not
    /// submit a disabled control, so the value arrives absent and
    /// `ItemService::gate_field_writes` copies the stored one back.
    ReadOnly,
    /// Not rendered at all — no label, no input, no value. A field the viewer
    /// may not see must not reach the page even as a disabled control.
    Hidden,
}

impl FieldMode {
    /// The mode for one field, from its two decisions.
    ///
    /// Denied for view ⇒ [`Hidden`](Self::Hidden), whether or not it may be
    /// edited: the form cannot ask someone to edit what it must not show them,
    /// and leaving the field out keeps the stored value through the write
    /// gate's copy-back. Visible but not editable ⇒ [`ReadOnly`](Self::ReadOnly)
    /// on an edit form, and [`Hidden`](Self::Hidden) on an add form, where
    /// there is no stored value to show and a disabled empty box says nothing.
    pub fn from_decisions(may_view: bool, may_edit: bool, creating: bool) -> Self {
        match (may_view, may_edit) {
            (false, _) => Self::Hidden,
            (true, false) if creating => Self::Hidden,
            (true, false) => Self::ReadOnly,
            (true, true) => Self::Editable,
        }
    }

    /// The template-facing name of this mode.
    ///
    /// The admin forms are Tera templates rather than `FormBuilder` output, so
    /// they read the mode as a string from the render context.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::ReadOnly => "readonly",
            Self::Hidden => "hidden",
        }
    }

    /// The attribute this mode adds to an input: `disabled`, or nothing.
    fn disabled_attr(self) -> &'static str {
        match self {
            Self::ReadOnly => "disabled",
            _ => "",
        }
    }
}

/// Builder for auto-generated forms.
pub struct FormBuilder {
    content_type: ContentTypeDefinition,
    /// Text formats the current user is permitted to use.
    /// When empty, all formats are shown (backwards compat).
    permitted_formats: Vec<String>,
    /// CSRF token emitted as a hidden `_csrf` input.
    ///
    /// An HTML form cannot set the `X-CSRF-Token` header the JSON API path
    /// reads, so without this the page the kernel renders could never be
    /// submitted successfully (**G-ITEM-FORM-MISMATCH**). Empty means no token
    /// input is emitted, which keeps the builder usable in tests.
    csrf_token: String,
    /// Extra form controls injected just before the submit button, so a caller
    /// can add a field (the URL-alias input) *inside* the `<form>` rather than
    /// concatenating it after the closing tag, where a browser would never
    /// submit it.
    extra_html: String,
    /// Display titles for `RecordReference` targets, keyed by target uuid.
    ///
    /// The stored value is a bare uuid (that is what the widget's JavaScript
    /// writes), so there is no title alongside it to render — the autocomplete
    /// box would come back blank on every edit even once the value itself
    /// survives, and the editor could not see what the field points at. The
    /// route resolves the titles and passes them here.
    reference_titles: std::collections::HashMap<String, String>,
    /// Per-field render mode, keyed by field name. A field with no entry is
    /// [`FieldMode::Editable`].
    field_modes: std::collections::HashMap<String, FieldMode>,
    /// Whether this user may decide the item's published state.
    ///
    /// Defaults to **false**, so a route that does not say renders no status
    /// control at all. The control used to be rendered unconditionally, and a
    /// missing checkbox reads as "unpublished" on submit, so the honest default
    /// is to omit it and let the submission say nothing about status.
    publish_allowed: bool,
}

impl FormBuilder {
    /// Create a new form builder for a content type.
    pub fn new(content_type: ContentTypeDefinition) -> Self {
        Self {
            content_type,
            permitted_formats: Vec::new(),
            csrf_token: String::new(),
            extra_html: String::new(),
            reference_titles: std::collections::HashMap::new(),
            field_modes: std::collections::HashMap::new(),
            publish_allowed: false,
        }
    }

    /// Set the per-field render modes, from the route's field-access decisions.
    pub fn with_field_modes(mut self, modes: std::collections::HashMap<String, FieldMode>) -> Self {
        self.field_modes = modes;
        self
    }

    /// Say whether this user may set the item's published state.
    ///
    /// When they may not, the status checkbox and its `_status_rendered`
    /// marker are both omitted, and the submission then says nothing about
    /// status rather than saying "unpublished".
    pub fn with_publish_allowed(mut self, allowed: bool) -> Self {
        self.publish_allowed = allowed;
        self
    }

    /// The mode for one field: what [`with_field_modes`](Self::with_field_modes)
    /// was told, or editable.
    fn mode_of(&self, field_name: &str) -> FieldMode {
        self.field_modes
            .get(field_name)
            .copied()
            .unwrap_or_default()
    }

    /// The published checkbox plus the hidden input that says it was rendered,
    /// or nothing at all when this user may not publish.
    ///
    /// `_status_rendered` is what lets the submission distinguish "the user
    /// unchecked the box" from "there was no box": an unchecked checkbox posts
    /// nothing, so without the marker the two are the same request.
    fn status_control(&self, checked: bool) -> String {
        if !self.publish_allowed {
            return String::new();
        }
        let checked = if checked { "checked" } else { "" };
        format!(
            r#"
            <div class="form-group">
                <input type="hidden" name="_status_rendered" value="1">
                <label>
                    <input type="checkbox" name="status" value="1" {checked}>
                    Published
                </label>
            </div>
            "#
        )
    }

    /// Supply display titles for `RecordReference` targets, keyed by uuid.
    pub fn with_reference_titles(
        mut self,
        titles: std::collections::HashMap<String, String>,
    ) -> Self {
        self.reference_titles = titles;
        self
    }

    /// Set the CSRF token this form carries in its body.
    pub fn with_csrf_token(mut self, token: impl Into<String>) -> Self {
        self.csrf_token = token.into();
        self
    }

    /// Append extra controls immediately before the submit button.
    pub fn with_extra_html(mut self, html: impl Into<String>) -> Self {
        self.extra_html = html.into();
        self
    }

    /// The hidden CSRF input, or nothing when no token was set.
    fn csrf_input(&self) -> String {
        if self.csrf_token.is_empty() {
            return String::new();
        }
        format!(
            r#"<input type="hidden" name="_csrf" value="{}">"#,
            html_escape(&self.csrf_token)
        )
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
            r#"<form method="post" action="{}" class="item-form item-form-add">{}"#,
            html_escape(action),
            self.csrf_input()
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

        // Dynamic fields. A hidden field is not rendered at all.
        for field in &self.content_type.fields {
            let mode = self.mode_of(&field.field_name);
            if mode == FieldMode::Hidden {
                continue;
            }
            html.push_str(&self.render_field(field, None, mode));
        }

        // Status field, only when this user may publish.
        html.push_str(&self.status_control(true));

        // Caller-supplied controls, inside the form so they are submitted.
        html.push_str(&self.extra_html);

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
            r#"<form method="post" action="{}" class="item-form item-form-edit">{}"#,
            html_escape(action),
            self.csrf_input()
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

        // Dynamic fields with existing values. A field this viewer may not see
        // is not rendered, value included.
        for field in &self.content_type.fields {
            let mode = self.mode_of(&field.field_name);
            if mode == FieldMode::Hidden {
                continue;
            }
            let value = item.fields.get(&field.field_name);
            html.push_str(&self.render_field(field, value, mode));
        }

        // Status field, only when this user may publish.
        html.push_str(&self.status_control(item.is_published()));

        // Revision log
        html.push_str(r#"
            <div class="form-group">
                <label for="log">Revision log message</label>
                <input type="text" id="log" name="log" class="form-control" placeholder="Describe your changes...">
            </div>
        "#);

        // Caller-supplied controls, inside the form so they are submitted.
        html.push_str(&self.extra_html);

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
    ///
    /// `mode` is never [`FieldMode::Hidden`] here — the callers skip those
    /// fields entirely rather than render them in any form. A `ReadOnly` field
    /// carries `disabled` on every control it renders, which is what keeps a
    /// browser from submitting it.
    fn render_field(
        &self,
        field: &FieldDefinition,
        value: Option<&serde_json::Value>,
        mode: FieldMode,
    ) -> String {
        let field_name = &field.field_name;
        let label = &field.label;
        let required = if field.required { "required" } else { "" };
        let required_star = if field.required { " *" } else { "" };
        let disabled = mode.disabled_attr();

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
                        <input type="text" id="{field_name}" name="{field_name}" value="{val}" {required} {disabled} {max} class="form-control">
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
                        <textarea id="{field_name}" name="{field_name}" rows="10" {required} {disabled} class="form-control">{val}</textarea>
                        <div class="form-help">
                            <select name="{field_name}_format" {disabled} class="form-control-sm">
                                {format_options}
                            </select>
                        </div>
                    </div>
                    "#
                )
            }

            FieldType::Integer => {
                let val = scalar_of(value)
                    .and_then(|v| v.as_i64().map(|n| n.to_string()))
                    .unwrap_or_default();
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="number" id="{field_name}" name="{field_name}" value="{val}" {required} {disabled} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::Float => {
                let val = scalar_of(value)
                    .and_then(|v| v.as_f64().map(|n| n.to_string()))
                    .unwrap_or_default();
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="number" id="{field_name}" name="{field_name}" value="{val}" step="any" {required} {disabled} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::Boolean => {
                let checked = scalar_of(value)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let checked_attr = if checked { "checked" } else { "" };
                format!(
                    r#"
                    <div class="form-group">
                        <label>
                            <input type="checkbox" id="{field_name}" name="{field_name}" value="1" {checked_attr} {disabled}>
                            {label}
                        </label>
                    </div>
                    "#
                )
            }

            FieldType::Date => {
                let val = extract_text_value(value);
                format!(
                    r#"
                    <div class="form-group">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="date" id="{field_name}" name="{field_name}" value="{val}" {required} {disabled} class="form-control">
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
                        <input type="email" id="{field_name}" name="{field_name}" value="{val}" {required} {disabled} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::RecordReference(target_type) => {
                let val = extract_reference_id(value);
                // Display title for the current value: an inline `title` key if
                // the writer supplied one, else the route-resolved title for
                // this uuid. Without the latter the box renders empty on every
                // edit, because what the widget stores is a bare uuid.
                let display = value
                    .and_then(|v| v.get("title"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| {
                        self.reference_titles
                            .get(&val)
                            .map(String::as_str)
                            .unwrap_or_default()
                    });
                let escaped_type = crate::routes::helpers::html_escape(target_type);
                let escaped_val = crate::routes::helpers::html_escape(&val);
                let escaped_display = crate::routes::helpers::html_escape(display);
                format!(
                    r#"
                    <div class="form-group record-reference-field">
                        <label for="{field_name}">{label}{required_star}</label>
                        <input type="hidden" id="{field_name}" name="{field_name}" value="{escaped_val}" {disabled}>
                        <input type="text" id="{field_name}_autocomplete"
                               class="form-control record-ref-autocomplete"
                               data-target-type="{escaped_type}"
                               data-hidden-field="{field_name}"
                               value="{escaped_display}"
                               placeholder="Search {escaped_type}..."
                               autocomplete="off"
                               {required} {disabled}>
                        <div class="record-ref-results" id="{field_name}_results"></div>
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
                        <input type="file" id="{field_name}" name="{field_name}" {required} {disabled} class="form-control">
                    </div>
                    "#
                )
            }

            FieldType::Blocks => {
                // Block editor: hidden input + container div for Editor.js
                let existing_json = value
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .unwrap_or_else(|| "[]".to_string());
                let existing_escaped = html_escape(&existing_json);

                format!(
                    r#"
                    <div class="form-group">
                        <label>{label}{required_star}</label>
                        <input type="hidden" id="{field_name}" name="{field_name}" value="{existing_escaped}" {disabled}>
                        <div data-block-editor data-block-editor-input="{field_name}"></div>
                    </div>
                    "#
                )
            }

            FieldType::PageBuilder => {
                // Page builder: hidden input + container div for Puck editor
                let existing_json = value
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .unwrap_or_else(|| r#"{"root":{"title":""},"content":[]}"#.to_string());
                let existing_escaped = html_escape(&existing_json);

                format!(
                    r#"
                    <div class="form-group">
                        <label>{label}{required_star}</label>
                        <input type="hidden" id="{field_name}" name="{field_name}" value="{existing_escaped}" {disabled}>
                        <div data-page-builder data-page-builder-input="{field_name}"
                             data-components-url="/api/v1/page-builder/components"></div>
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
                            <input type="hidden" name="{field_name}" class="compound-field__value" value="{existing_escaped}" {disabled}>
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

/// The scalar inside a stored field value: the `{"value": …}` wrapper's payload
/// when present, otherwise the value itself. See [`raw_field_text`] for why
/// both shapes have to be read.
fn scalar_of(value: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    value.map(|v| v.get("value").unwrap_or(v))
}

/// Read a stored field value as raw text, accepting **both** shapes the kernel
/// writes.
///
/// Two item-form stacks disagreed about storage: the admin stack
/// (`admin_content.rs`) writes **flat** scalars and reads them back flat, so it
/// round-trips; `FormBuilder` read only the `{"value": …}` wrapper, which
/// nothing on its own path ever wrote — so every saved value rendered back
/// empty (**G-ITEM-FORM-MISMATCH**, Argus M3).
///
/// Rather than pick a winner and migrate every stored item, this reads either:
/// the wrapper when present, the bare scalar otherwise. Both shapes are live in
/// the tree — plugin-written items use the wrapper (`argus_story`) and
/// admin-written items are flat (`argus_feed`) — so tolerance here is not
/// laxity, it is the only reading that is correct for the data that exists.
/// Numbers and booleans are rendered too; before, a stored `5` produced an
/// empty input.
fn raw_field_text(value: Option<&serde_json::Value>) -> Option<String> {
    match scalar_of(value)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Extract text value from field JSON, HTML-escaped for an attribute or body.
fn extract_text_value(value: Option<&serde_json::Value>) -> String {
    raw_field_text(value)
        .map(|s| html_escape(&s))
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

/// Read a `RecordReference` field's target id, accepting every shape that
/// reaches it.
///
/// The widget's JavaScript sets the hidden input to a **bare uuid**
/// (`static/js/record-ref.js`), the form read `value.target_id`, and the two
/// never met — so a reference silently emptied itself on every edit, which is
/// why Argus M3 modelled a feed's topic as a plain `Text` uuid instead of a
/// reference (deviation 3). Accepts `{"target_id": …}`, `{"value": …}`, a bare
/// string, and the first element of a list (the multi-value shape the reverse
/// -reference resolver in `routes/item.rs` already handles).
pub fn extract_reference_id(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let candidate = value
        .get("target_id")
        .or_else(|| value.get("value"))
        .unwrap_or(value);
    let id = match candidate {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(items) => items
            .first()
            .and_then(|v| v.get("target_id").unwrap_or(v).as_str())
            .unwrap_or_default(),
        _ => "",
    };
    id.to_string()
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
            title_label: None,
            fields: vec![
                FieldDefinition {
                    field_name: "body".to_string(),
                    field_type: FieldType::TextLong,
                    label: "Body".to_string(),
                    required: true,
                    cardinality: 1,
                    settings: serde_json::json!({}),
                    personal_data: false,
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
                    personal_data: false,
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
        // The control is rendered only for a user who may publish, so this test
        // says so. Its absence by default is the new behaviour, pinned in
        // `tests/item_test.rs`.
        let builder = FormBuilder::new(test_content_type()).with_publish_allowed(true);
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
            title_label: None,
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
                personal_data: false,
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
            stage_id: crate::models::stage::LIVE_STAGE_ID,
            language: "en".to_string(),
            item_group_id: uuid::Uuid::now_v7(),
            retention_days: None,
        };
        let form = builder.build_edit_form(&item, "/item/123/edit");
        assert!(form.contains(r#"name="log""#));
        assert!(form.contains("Revision log"));
    }
}
