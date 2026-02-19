//! Form and form element types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A complete form definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Form {
    /// Unique form identifier (e.g., "content_type_add_form").
    pub form_id: String,

    /// Unique build ID for this form instance (for AJAX state tracking).
    pub form_build_id: String,

    /// Form action URL.
    pub action: String,

    /// HTTP method ("post" or "get").
    pub method: String,

    /// Form elements keyed by name.
    pub elements: BTreeMap<String, FormElement>,

    /// CSRF token for form submission.
    pub token: String,

    /// Optional form title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Optional form description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Additional form attributes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,

    /// Whether this form has been modified (for "unsaved changes" warnings).
    #[serde(default)]
    pub dirty: bool,
}

impl Form {
    /// Create a new form with the given ID.
    pub fn new(form_id: impl Into<String>) -> Self {
        Self {
            form_id: form_id.into(),
            form_build_id: uuid::Uuid::new_v4().to_string(),
            action: String::new(),
            method: "post".to_string(),
            elements: BTreeMap::new(),
            token: String::new(),
            title: None,
            description: None,
            attributes: None,
            dirty: false,
        }
    }

    /// Set the form action URL.
    pub fn action(mut self, action: impl Into<String>) -> Self {
        self.action = action.into();
        self
    }

    /// Set the form method.
    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    /// Set the form title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the form description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add an element to the form.
    pub fn element(mut self, name: impl Into<String>, element: FormElement) -> Self {
        self.elements.insert(name.into(), element);
        self
    }

    /// Add multiple elements.
    pub fn elements(mut self, elements: impl IntoIterator<Item = (String, FormElement)>) -> Self {
        self.elements.extend(elements);
        self
    }

    /// Get a mutable reference to an element.
    pub fn get_element_mut(&mut self, name: &str) -> Option<&mut FormElement> {
        self.elements.get_mut(name)
    }

    /// Get elements sorted by weight.
    pub fn sorted_elements(&self) -> Vec<(&String, &FormElement)> {
        let mut elements: Vec<_> = self.elements.iter().collect();
        elements.sort_by_key(|(_, el)| el.weight);
        elements
    }
}

/// A form element definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormElement {
    /// Element type with type-specific configuration.
    #[serde(flatten)]
    pub element_type: ElementType,

    /// Element title/label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Element description/help text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Default value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,

    /// Whether this field is required.
    #[serde(default)]
    pub required: bool,

    /// Sort weight (lower = appears first).
    #[serde(default)]
    pub weight: i32,

    /// Additional HTML attributes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,

    /// Child elements (for containers, fieldsets).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub children: BTreeMap<String, FormElement>,

    /// Whether this element is disabled.
    #[serde(default)]
    pub disabled: bool,

    /// Placeholder text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,

    /// Prefix markup (displayed before element).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,

    /// Suffix markup (displayed after element).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,

    /// AJAX callback configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ajax: Option<AjaxConfig>,
}

impl FormElement {
    /// Create a textfield element.
    pub fn textfield() -> Self {
        Self::new(ElementType::Textfield { max_length: None })
    }

    /// Create a textarea element.
    pub fn textarea(rows: u32) -> Self {
        Self::new(ElementType::Textarea { rows })
    }

    /// Create a select element.
    pub fn select(options: Vec<(String, String)>) -> Self {
        Self::new(ElementType::Select {
            options,
            multiple: false,
        })
    }

    /// Create a multi-select element.
    pub fn multi_select(options: Vec<(String, String)>) -> Self {
        Self::new(ElementType::Select {
            options,
            multiple: true,
        })
    }

    /// Create a checkbox element.
    pub fn checkbox() -> Self {
        Self::new(ElementType::Checkbox)
    }

    /// Create a checkboxes group.
    pub fn checkboxes(options: Vec<(String, String)>) -> Self {
        Self::new(ElementType::Checkboxes { options })
    }

    /// Create a radio button group.
    pub fn radio(options: Vec<(String, String)>) -> Self {
        Self::new(ElementType::Radio { options })
    }

    /// Create a hidden field.
    pub fn hidden() -> Self {
        Self::new(ElementType::Hidden)
    }

    /// Create a password field.
    pub fn password() -> Self {
        Self::new(ElementType::Password)
    }

    /// Create a file upload field.
    pub fn file() -> Self {
        Self::new(ElementType::File)
    }

    /// Create a submit button.
    pub fn submit(value: impl Into<String>) -> Self {
        Self::new(ElementType::Submit {
            value: value.into(),
        })
    }

    /// Create a fieldset.
    pub fn fieldset() -> Self {
        Self::new(ElementType::Fieldset {
            collapsible: false,
            collapsed: false,
        })
    }

    /// Create a collapsible fieldset.
    pub fn fieldset_collapsible(collapsed: bool) -> Self {
        Self::new(ElementType::Fieldset {
            collapsible: true,
            collapsed,
        })
    }

    /// Create a markup element (display-only HTML).
    pub fn markup(value: impl Into<String>) -> Self {
        Self::new(ElementType::Markup {
            value: value.into(),
        })
    }

    /// Create a container element.
    pub fn container() -> Self {
        Self::new(ElementType::Container)
    }

    /// Create a new element with the given type.
    fn new(element_type: ElementType) -> Self {
        Self {
            element_type,
            title: None,
            description: None,
            default_value: None,
            required: false,
            weight: 0,
            attributes: None,
            children: BTreeMap::new(),
            disabled: false,
            placeholder: None,
            prefix: None,
            suffix: None,
            ajax: None,
        }
    }

    /// Set the element title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the element description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the default value.
    pub fn default_value(mut self, value: impl Into<Value>) -> Self {
        self.default_value = Some(value.into());
        self
    }

    /// Mark as required.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Set the weight.
    pub fn weight(mut self, weight: i32) -> Self {
        self.weight = weight;
        self
    }

    /// Set placeholder text.
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// Set max length for textfield.
    pub fn max_length(mut self, max: usize) -> Self {
        if let ElementType::Textfield { ref mut max_length } = self.element_type {
            *max_length = Some(max);
        }
        self
    }

    /// Add a child element.
    pub fn child(mut self, name: impl Into<String>, element: FormElement) -> Self {
        self.children.insert(name.into(), element);
        self
    }

    /// Mark as disabled.
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    /// Add an AJAX callback.
    pub fn ajax(mut self, config: AjaxConfig) -> Self {
        self.ajax = Some(config);
        self
    }

    /// Set prefix markup.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Set suffix markup.
    pub fn suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = Some(suffix.into());
        self
    }
}

/// Element type variants with type-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ElementType {
    /// Single-line text input.
    Textfield {
        #[serde(skip_serializing_if = "Option::is_none")]
        max_length: Option<usize>,
    },

    /// Multi-line text input.
    Textarea { rows: u32 },

    /// Dropdown select.
    Select {
        options: Vec<(String, String)>,
        #[serde(default)]
        multiple: bool,
    },

    /// Single checkbox.
    Checkbox,

    /// Multiple checkboxes.
    Checkboxes { options: Vec<(String, String)> },

    /// Radio button group.
    Radio { options: Vec<(String, String)> },

    /// Hidden field.
    Hidden,

    /// Password field.
    Password,

    /// File upload.
    File,

    /// Submit button.
    Submit { value: String },

    /// Fieldset/group.
    Fieldset {
        #[serde(default)]
        collapsible: bool,
        #[serde(default)]
        collapsed: bool,
    },

    /// Display-only markup.
    Markup { value: String },

    /// Generic container for AJAX targets.
    Container,
}

impl ElementType {
    /// Get the type name as a string.
    pub fn type_name(&self) -> &'static str {
        match self {
            ElementType::Textfield { .. } => "textfield",
            ElementType::Textarea { .. } => "textarea",
            ElementType::Select { .. } => "select",
            ElementType::Checkbox => "checkbox",
            ElementType::Checkboxes { .. } => "checkboxes",
            ElementType::Radio { .. } => "radio",
            ElementType::Hidden => "hidden",
            ElementType::Password => "password",
            ElementType::File => "file",
            ElementType::Submit { .. } => "submit",
            ElementType::Fieldset { .. } => "fieldset",
            ElementType::Markup { .. } => "markup",
            ElementType::Container => "container",
        }
    }
}

/// AJAX callback configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AjaxConfig {
    /// Callback name (e.g., "add_field").
    pub callback: String,

    /// Event that triggers the callback (e.g., "click", "change").
    #[serde(default = "default_event")]
    pub event: String,

    /// CSS selector for element to update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<String>,

    /// Whether to show a progress indicator.
    #[serde(default = "default_true")]
    pub progress: bool,
}

fn default_event() -> String {
    "click".to_string()
}

fn default_true() -> bool {
    true
}

impl AjaxConfig {
    /// Create a new AJAX configuration.
    pub fn new(callback: impl Into<String>) -> Self {
        Self {
            callback: callback.into(),
            event: "click".to_string(),
            wrapper: None,
            progress: true,
        }
    }

    /// Set the triggering event.
    pub fn event(mut self, event: impl Into<String>) -> Self {
        self.event = event.into();
        self
    }

    /// Set the wrapper element selector.
    pub fn wrapper(mut self, wrapper: impl Into<String>) -> Self {
        self.wrapper = Some(wrapper.into());
        self
    }

    /// Disable progress indicator.
    pub fn no_progress(mut self) -> Self {
        self.progress = false;
        self
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_form_builder() {
        let form = Form::new("test_form")
            .title("Test Form")
            .action("/submit")
            .element("name", FormElement::textfield().title("Name").required())
            .element("submit", FormElement::submit("Save").weight(100));

        assert_eq!(form.form_id, "test_form");
        assert_eq!(form.action, "/submit");
        assert_eq!(form.elements.len(), 2);
        assert!(form.elements.get("name").unwrap().required);
    }

    #[test]
    fn test_form_element_types() {
        let textfield = FormElement::textfield().max_length(100);
        assert!(matches!(
            textfield.element_type,
            ElementType::Textfield {
                max_length: Some(100)
            }
        ));

        let textarea = FormElement::textarea(5);
        assert!(matches!(
            textarea.element_type,
            ElementType::Textarea { rows: 5 }
        ));

        let select = FormElement::select(vec![
            ("a".to_string(), "Option A".to_string()),
            ("b".to_string(), "Option B".to_string()),
        ]);
        assert!(matches!(
            select.element_type,
            ElementType::Select {
                multiple: false,
                ..
            }
        ));
    }

    #[test]
    fn test_form_sorted_elements() {
        let form = Form::new("test")
            .element("c", FormElement::textfield().weight(30))
            .element("a", FormElement::textfield().weight(10))
            .element("b", FormElement::textfield().weight(20));

        let sorted = form.sorted_elements();
        assert_eq!(sorted[0].0, "a");
        assert_eq!(sorted[1].0, "b");
        assert_eq!(sorted[2].0, "c");
    }

    #[test]
    fn test_ajax_config() {
        let config = AjaxConfig::new("add_field")
            .event("click")
            .wrapper("#field-container");

        assert_eq!(config.callback, "add_field");
        assert_eq!(config.event, "click");
        assert_eq!(config.wrapper, Some("#field-container".to_string()));
    }

    #[test]
    fn test_element_type_name() {
        assert_eq!(
            ElementType::Textfield { max_length: None }.type_name(),
            "textfield"
        );
        assert_eq!(ElementType::Checkbox.type_name(), "checkbox");
        assert_eq!(
            ElementType::Submit {
                value: "Save".to_string()
            }
            .type_name(),
            "submit"
        );
    }

    #[test]
    fn test_form_serialization() {
        let form = Form::new("test").element("name", FormElement::textfield().title("Name"));

        let json = serde_json::to_string(&form).unwrap();
        assert!(json.contains("test"));
        assert!(json.contains("textfield"));

        let parsed: Form = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.form_id, "test");
    }
}
