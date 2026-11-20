//! Item forms, built as typed [`Form`]s from content type field definitions.

use crate::form::{Form, FormElement};
use crate::models::Item;
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
}

/// Builder for item forms: a content type's fields as a typed
/// [`crate::form::Form`], never HTML.
///
/// This is the one place a [`FieldType`] becomes a widget. The route adds its
/// own elements (URL alias, stage), `FormService::build` runs
/// `tap_form_alter`, and `ThemeEngine::render_form_with_errors` renders the
/// result, the same way on the admin form and on `/item/add` and
/// `/item/{id}/edit`.
pub struct FormBuilder {
    content_type: ContentTypeDefinition,
    /// Text formats the current user is permitted to use.
    /// When empty, all formats are shown (backwards compat).
    permitted_formats: Vec<String>,
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

/// Weight of the title element: first.
const TITLE_WEIGHT: i32 = -100;
/// Weight of the publishing controls, after every field.
pub const STATUS_WEIGHT: i32 = 900;
/// Weight of the revision log message.
pub const LOG_WEIGHT: i32 = 950;
/// Weight of the submit button: last.
pub const SUBMIT_WEIGHT: i32 = 1000;

impl FormBuilder {
    /// Create a new form builder for a content type.
    pub fn new(content_type: ContentTypeDefinition) -> Self {
        Self {
            content_type,
            permitted_formats: Vec::new(),
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

    /// Supply display titles for `RecordReference` targets, keyed by uuid.
    pub fn with_reference_titles(
        mut self,
        titles: std::collections::HashMap<String, String>,
    ) -> Self {
        self.reference_titles = titles;
        self
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

    /// The add form, `item_add_{type}`, posting to `action`.
    pub fn build_add_form(&self, action: &str) -> Form {
        self.build(
            format!("item_add_{}", self.content_type.machine_name),
            action,
            None,
        )
    }

    /// The edit form for `item`, `item_edit_{type}`, posting to `action`.
    pub fn build_edit_form(&self, item: &Item, action: &str) -> Form {
        self.build(
            format!("item_edit_{}", self.content_type.machine_name),
            action,
            Some(item),
        )
    }

    /// The form both public builders share.
    fn build(&self, form_id: String, action: &str, item: Option<&Item>) -> Form {
        let title_label = self
            .content_type
            .title_label
            .clone()
            .unwrap_or_else(|| "Title".to_string());
        let mut title = FormElement::textfield()
            .title(title_label)
            .required()
            .weight(TITLE_WEIGHT);
        if let Some(item) = item {
            title = title.default_value(item.title.clone());
        }
        let mut form = Form::new(form_id).action(action).element("title", title);

        // A field this viewer may not see is not rendered, value included.
        let stored = item.and_then(|i| i.fields.as_object());
        for (index, field) in self.content_type.fields.iter().enumerate() {
            let mode = self.mode_of(&field.field_name);
            if mode == FieldMode::Hidden {
                continue;
            }
            let value = item.and_then(|i| i.fields.get(&field.field_name));
            let weight = i32::try_from(index).unwrap_or(i32::MAX - 1000);
            let elements = self.field_elements(field, value, mode, weight);
            form = match field.visible_when_condition() {
                None => form.elements(elements),
                Some(when) => {
                    let shown = crate::content::submission::is_visible(
                        field,
                        &self.content_type,
                        stored,
                        &[],
                        None,
                    );
                    form.element(
                        format!("{}--when", field.field_name),
                        visible_when_wrapper(&when, shown, weight, elements),
                    )
                }
            };
        }

        // The published checkbox and the marker that says it was rendered:
        // an unchecked box posts nothing, so without the marker "unchecked"
        // and "never offered" are the same request. Only for a user who may
        // publish.
        if self.publish_allowed {
            let published = item.is_none_or(Item::is_published);
            form = form
                .element(
                    "status",
                    FormElement::checkbox()
                        .title("Published")
                        .default_value(published)
                        .weight(STATUS_WEIGHT),
                )
                .element(
                    "_status_rendered",
                    FormElement::hidden()
                        .default_value("1")
                        .weight(STATUS_WEIGHT),
                );
        }

        if item.is_some() {
            form = form.element(
                "log",
                FormElement::textfield()
                    .title("Revision log message")
                    .placeholder("Describe your changes...")
                    .weight(LOG_WEIGHT),
            );
        }

        form.element("op", FormElement::submit("Save").weight(SUBMIT_WEIGHT))
    }

    /// The element(s) one field renders as, under its own name.
    ///
    /// A field whose `cardinality` is not 1 is a container holding one widget
    /// per stored value, `name[0]`, `name[1]`, …, plus one empty slot while
    /// the cardinality allows another value; the submission builder reads the
    /// deltas back in order.
    fn field_elements(
        &self,
        field: &FieldDefinition,
        value: Option<&serde_json::Value>,
        mode: FieldMode,
        weight: i32,
    ) -> Vec<(String, FormElement)> {
        let name = &field.field_name;
        if !crate::content::field_values::stores_a_list(field) {
            let mut elements = vec![(name.clone(), self.widget(field, value, mode))];
            if matches!(field.field_type, FieldType::TextLong) {
                elements.push((format!("{name}_format"), self.format_select(value, mode)));
            }
            return elements
                .into_iter()
                .map(|(n, e)| (n, e.weight(weight)))
                .collect();
        }

        let stored: Vec<&serde_json::Value> = match value {
            Some(serde_json::Value::Array(values)) => values.iter().collect(),
            Some(serde_json::Value::Null) | None => Vec::new(),
            Some(single) => vec![single],
        };
        let room = field.cardinality < 0 || stored.len() < field.cardinality as usize;
        let mut slots: Vec<Option<&serde_json::Value>> = stored.into_iter().map(Some).collect();
        if room && mode == FieldMode::Editable {
            slots.push(None);
        }

        let mut container = FormElement::container()
            .title(field.label.clone())
            .weight(weight)
            .attribute("data-cardinality", field.cardinality);
        if field.cardinality > 0 {
            container = container.attribute("data-max-items", field.cardinality);
        }
        match mode {
            FieldMode::ReadOnly => container = container.disabled(),
            // "Add another" asks the server for the next widget (see
            // `routes::item_form::add_another`); hidden once the field is full.
            _ => {
                container = container.attribute(crate::form::ADD_ANOTHER, format!("add_{name}"));
                if !room {
                    container = container.attribute("data-full", "true");
                }
            }
        }
        for (delta, slot) in slots.into_iter().enumerate() {
            let mut widget = self.widget(field, slot, mode);
            widget.title = None;
            widget.required = false;
            let delta_weight = i32::try_from(delta).unwrap_or(i32::MAX);
            container = container.child(format!("{name}[{delta}]"), widget.weight(delta_weight));
        }
        vec![(name.clone(), container)]
    }

    /// The single value widget for `field`.
    ///
    /// `mode` is never [`FieldMode::Hidden`] here — the callers skip those
    /// fields entirely rather than render them in any form. A `ReadOnly` field
    /// is `disabled`, which is what keeps a browser from submitting it.
    pub fn widget(
        &self,
        field: &FieldDefinition,
        value: Option<&serde_json::Value>,
        mode: FieldMode,
    ) -> FormElement {
        let mut element =
            match &field.field_type {
                FieldType::Text { max_length } => {
                    let mut e = FormElement::textfield();
                    if let Some(max) = max_length {
                        e = e.max_length(*max);
                    }
                    e.default_value(raw_field_text(value).unwrap_or_default())
                }
                FieldType::TextLong => FormElement::textarea(10)
                    .default_value(raw_field_text(value).unwrap_or_default()),
                FieldType::Integer => FormElement::input("number").default_value(
                    scalar_of(value)
                        .and_then(|v| v.as_i64().map(|n| n.to_string()))
                        .unwrap_or_default(),
                ),
                FieldType::Float => FormElement::input("number")
                    .attribute("step", "any")
                    .default_value(
                        scalar_of(value)
                            .and_then(|v| v.as_f64().map(|n| n.to_string()))
                            .unwrap_or_default(),
                    ),
                FieldType::Boolean => FormElement::checkbox().default_value(
                    scalar_of(value)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                ),
                FieldType::Date => FormElement::input("date")
                    .default_value(raw_field_text(value).unwrap_or_default()),
                FieldType::Email => FormElement::input("email")
                    .default_value(raw_field_text(value).unwrap_or_default()),
                FieldType::RecordReference(target_type) => {
                    let id = extract_reference_id(value);
                    // Display title for the current value: an inline `title` key if
                    // the writer supplied one, else the route-resolved title for
                    // this uuid. Without the latter the box renders empty on every
                    // edit, because what the widget stores is a bare uuid.
                    let display = value
                        .and_then(|v| v.get("title"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| self.reference_titles.get(&id).cloned())
                        .unwrap_or_default();
                    FormElement::widget("record_reference")
                        .attribute("data-target-type", target_type.clone())
                        .attribute("data-autocomplete-url", "/api/v1/items/autocomplete?type=")
                        .attribute("data-display", display)
                        .default_value(id)
                }
                // The tag picker: the same widget and script, asking the tags
                // endpoint for this category. One slot holds one tag uuid; the
                // field is always the repeating container (`stores_a_list`), so
                // what it posts back is an array. The box shows the tag's label.
                FieldType::CategoryReference(category) => {
                    let id = extract_reference_id(value);
                    let display = self.reference_titles.get(&id).cloned().unwrap_or_default();
                    FormElement::widget("record_reference")
                        .attribute("data-target-type", category.clone())
                        .attribute(
                            "data-autocomplete-url",
                            "/api/v1/tags/autocomplete?category=",
                        )
                        .attribute("data-display", display)
                        .default_value(id)
                }
                FieldType::File => FormElement::widget("file")
                    .default_value(raw_field_text(value).unwrap_or_default()),
                FieldType::Blocks => {
                    FormElement::widget("blocks").default_value(json_input(value, "[]"))
                }
                FieldType::PageBuilder => FormElement::widget("page_builder")
                    .default_value(json_input(value, r#"{"root":{"title":""},"content":[]}"#)),
                FieldType::Compound {
                    allowed_types,
                    min_items,
                    max_items,
                } => {
                    let config = serde_json::json!({
                        "allowed_types": allowed_types,
                        "min_items": min_items,
                        "max_items": max_items,
                    });
                    let section_types =
                        crate::content::compound::parse_section_schemas(&field.settings);
                    FormElement::widget("compound")
                        .attribute(
                            "data-config",
                            serde_json::to_string(&config).unwrap_or_default(),
                        )
                        .attribute(
                            "data-section-types",
                            serde_json::to_string(&section_types).unwrap_or_default(),
                        )
                        .default_value(json_input(value, r#"{"sections":[]}"#))
                }
            };
        element = element.title(field.label.clone());
        // A checkbox that must be ticked is not what "required" means for a
        // Boolean; the other widgets carry it to the browser.
        if field.required && !matches!(field.field_type, FieldType::Boolean) {
            element = element.required();
        }
        if mode == FieldMode::ReadOnly {
            element = element.disabled();
        }
        element
    }

    /// The text format selector beside a long text field, offering the
    /// formats this user may use.
    fn format_select(&self, value: Option<&serde_json::Value>, mode: FieldMode) -> FormElement {
        let current = extract_format_value(value);
        let options: Vec<(String, String)> = [
            ("filtered_html", "Filtered HTML"),
            ("full_html", "Full HTML"),
            ("plain_text", "Plain Text"),
        ]
        .into_iter()
        .filter(|(format, _)| self.is_format_permitted(format))
        .map(|(format, label)| (format.to_string(), label.to_string()))
        .collect();
        let mut element = FormElement::select(options)
            .title("Text format")
            .default_value(current)
            .required();
        if mode == FieldMode::ReadOnly {
            element = element.disabled();
        }
        element
    }
}

/// A stored JSON value as the text a widget's hidden input carries, or
/// `empty` when there is none.
fn json_input(value: Option<&serde_json::Value>, empty: &str) -> String {
    match value {
        Some(serde_json::Value::Null) | None => empty.to_string(),
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| empty.to_string()),
    }
}

/// The scalar inside a stored field value: the `{"value": …}` wrapper's payload
/// when present, otherwise the value itself. See [`raw_field_text`] for why
/// both shapes have to be read.
fn scalar_of(value: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    value.map(|v| v.get("value").unwrap_or(v))
}

/// The wrapper a field with `visible_when` renders in: a container holding the
/// field's elements, carrying the condition for `form-states.js`, and
/// `hidden` when the stored values make the condition false. Without
/// JavaScript the condition is not re-evaluated as the editor types; the
/// server applies it on save either way.
///
/// The inputs inside lose the HTML `required` attribute: a browser refuses to
/// submit a form whose hidden control is required and empty, and the
/// submission builder checks `required` itself, only while the field shows.
fn visible_when_wrapper(
    when: &trovato_sdk::types::VisibleWhen,
    shown: bool,
    weight: i32,
    elements: Vec<(String, FormElement)>,
) -> FormElement {
    let mut wrapper = FormElement::container()
        .weight(weight)
        .attribute("data-visible-when-field", when.field.clone())
        .attribute("data-visible-when-equals", when.equals.clone());
    if !shown {
        wrapper = wrapper.attribute("hidden", "hidden");
    }
    for (name, mut element) in elements {
        element.required = false;
        for child in element.children.values_mut() {
            child.required = false;
        }
        wrapper = wrapper.child(name, element);
    }
    wrapper
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
    use crate::form::ElementType;

    fn test_content_type() -> ContentTypeDefinition {
        ContentTypeDefinition {
            machine_name: "blog".to_string(),
            label: "Blog Post".to_string(),
            description: "A blog article".to_string(),
            title_label: None,
            fields: vec![
                FieldDefinition::new("body", FieldType::TextLong)
                    .label("Body")
                    .required(),
                FieldDefinition::new(
                    "summary",
                    FieldType::Text {
                        max_length: Some(255),
                    },
                )
                .label("Summary"),
                FieldDefinition::new("tags", FieldType::Text { max_length: None })
                    .label("Tags")
                    .cardinality(-1),
            ],
        }
    }

    fn item(fields: serde_json::Value) -> Item {
        Item {
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
            fields,
            stage_id: crate::models::stage::LIVE_STAGE_ID,
            language: "en".to_string(),
            item_group_id: uuid::Uuid::now_v7(),
            retention_days: None,
        }
    }

    #[test]
    fn the_add_form_is_typed_and_named_for_its_type() {
        let form = FormBuilder::new(test_content_type()).build_add_form("/item/add/blog");
        assert_eq!(form.form_id, "item_add_blog");
        assert_eq!(form.action, "/item/add/blog");
        assert!(form.elements["title"].required);
        assert!(matches!(
            form.elements["body"].element_type,
            ElementType::Textarea { .. }
        ));
        assert!(form.elements.contains_key("body_format"));
        assert!(matches!(
            form.elements["op"].element_type,
            ElementType::Submit { .. }
        ));
        assert!(!form.elements.contains_key("log"), "no log on an add form");
    }

    #[test]
    fn a_multi_value_field_renders_every_value_and_one_empty_slot() {
        let form = FormBuilder::new(test_content_type())
            .build_edit_form(&item(serde_json::json!({"tags": ["a", "b"]})), "/x");
        let tags = &form.elements["tags"];
        let names: Vec<&String> = tags.children.keys().collect();
        assert_eq!(names, vec!["tags[0]", "tags[1]", "tags[2]"]);
        assert_eq!(
            tags.children["tags[1]"].default_value,
            Some(serde_json::json!("b"))
        );
        assert_eq!(
            tags.children["tags[2]"].default_value,
            Some(serde_json::json!(""))
        );
        assert!(form.rendered_inputs().contains(&"tags".to_string()));
    }

    #[test]
    fn modes_hide_and_disable() {
        let modes = [
            ("summary".to_string(), FieldMode::Hidden),
            ("body".to_string(), FieldMode::ReadOnly),
        ]
        .into_iter()
        .collect();
        let form = FormBuilder::new(test_content_type())
            .with_field_modes(modes)
            .build_edit_form(&item(serde_json::json!({"summary": "secret"})), "/x");
        assert!(!form.elements.contains_key("summary"));
        assert!(form.elements["body"].disabled);
        let rendered = form.rendered_inputs();
        assert!(!rendered.contains(&"summary".to_string()));
        assert!(!rendered.contains(&"body".to_string()));
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
    fn compound_field_carries_its_config() {
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
                    "section_types": [{"machine_name": "text", "label": "Text", "fields": []}]
                }),
                personal_data: false,
            }],
        };
        let form = FormBuilder::new(ct).build_add_form("/item/add/page");
        let sections = &form.elements["sections"];
        assert!(matches!(
            &sections.element_type,
            ElementType::Widget { widget } if widget == "compound"
        ));
        let attrs = sections.attributes.as_ref().unwrap();
        assert!(attrs.get("data-section-types").is_some());
        assert_eq!(
            sections.default_value,
            Some(serde_json::json!(r#"{"sections":[]}"#))
        );
    }

    /// The real templates, so these check the markup a browser gets.
    fn render(form: &Form) -> String {
        let templates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
        crate::theme::ThemeEngine::new(&[templates], None)
            .unwrap()
            .render_form(form)
            .unwrap()
    }

    #[test]
    fn a_multi_value_field_offers_add_another_until_full() {
        let ct = ContentTypeDefinition {
            machine_name: "probe".to_string(),
            label: "Probe".to_string(),
            description: String::new(),
            title_label: None,
            fields: vec![
                FieldDefinition::new("field_tags", FieldType::Text { max_length: None })
                    .cardinality(-1),
                FieldDefinition::new("field_pair", FieldType::Text { max_length: None })
                    .cardinality(2),
            ],
        };
        let item = item(serde_json::json!({"field_tags": ["a"], "field_pair": ["x", "y"]}));
        let form = FormBuilder::new(ct).build_edit_form(&item, "/x");
        assert!(form.uses_ajax());
        let html = render(&form);
        assert!(html.contains(r#"id="field_tags-items""#), "{html}");
        assert!(html.contains(r#"data-ajax-trigger="add_field_tags""#));
        // The full field keeps its button for later, hidden.
        let pair = html.find(r#"data-ajax-trigger="add_field_pair""#).unwrap();
        let tag_end = pair + html[pair..].find('>').unwrap();
        assert!(
            html[pair..tag_end].contains("hidden"),
            "{}",
            &html[pair..tag_end]
        );
    }

    #[test]
    fn a_field_with_visible_when_is_wrapped_and_hidden_while_false() {
        let ct = ContentTypeDefinition {
            machine_name: "probe".to_string(),
            label: "Probe".to_string(),
            description: String::new(),
            title_label: None,
            fields: vec![
                FieldDefinition::new("field_online", FieldType::Boolean),
                FieldDefinition::new("field_url", FieldType::Text { max_length: None })
                    .visible_when("field_online", "1"),
            ],
        };
        let mut item = item(serde_json::json!({"field_online": false, "field_url": "kept"}));
        let form = FormBuilder::new(ct.clone()).build_edit_form(&item, "/x");
        assert_eq!(
            form.rendered_inputs(),
            vec!["field_online", "field_url", "log", "title"]
        );
        let html = render(&form);
        let at = html
            .find(r#"data-visible-when-field="field_online""#)
            .expect(&html);
        let open = html[..at].rfind('<').unwrap();
        let tag = &html[open..at + html[at..].find('>').unwrap()];
        assert!(tag.contains(r#"hidden="hidden""#), "{tag}");
        assert!(tag.contains(r#"data-visible-when-equals="1""#), "{tag}");
        assert!(
            html.contains("form-states.js"),
            "the script that keeps it current"
        );

        item.fields = serde_json::json!({"field_online": true});
        let html = render(&FormBuilder::new(ct).build_edit_form(&item, "/x"));
        assert!(!html.contains(r#"hidden="hidden""#));
    }
}
