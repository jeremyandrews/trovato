//! Render element builder API.
//!
//! Plugins return structured JSON render elements (never raw HTML).
//! The Kernel sanitizes and renders these via Tera templates.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A render element in the JSON render tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderElement {
    #[serde(rename = "#type")]
    pub element_type: String,
    #[serde(rename = "#weight", skip_serializing_if = "Option::is_none")]
    pub weight: Option<i32>,
    #[serde(rename = "#tag", skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(rename = "#value", skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(rename = "#format", skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(rename = "#attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
    #[serde(flatten)]
    pub children: BTreeMap<String, RenderElement>,
}

impl RenderElement {
    pub fn set_child(&mut self, key: &str, element: RenderElement) {
        self.children.insert(key.into(), element);
    }
}

/// Builder for constructing render elements.
pub struct ElementBuilder {
    element_type: String,
    weight: Option<i32>,
    tag: Option<String>,
    value: Option<String>,
    format: Option<String>,
    classes: Vec<String>,
    attrs: serde_json::Map<String, Value>,
    children: BTreeMap<String, RenderElement>,
}

impl ElementBuilder {
    fn new(element_type: &str) -> Self {
        Self {
            element_type: element_type.into(),
            weight: None,
            tag: None,
            value: None,
            format: None,
            classes: Vec::new(),
            attrs: serde_json::Map::new(),
            children: BTreeMap::new(),
        }
    }

    pub fn weight(mut self, w: i32) -> Self {
        self.weight = Some(w);
        self
    }

    pub fn class(mut self, class: &str) -> Self {
        self.classes.push(class.into());
        self
    }

    pub fn attr(mut self, key: &str, value: &str) -> Self {
        self.attrs.insert(key.into(), Value::String(value.into()));
        self
    }

    pub fn child(mut self, key: &str, element: RenderElement) -> Self {
        self.children.insert(key.into(), element);
        self
    }

    // -- ARIA accessibility helpers --
    // Each method maps to an HTML attribute for screen reader and
    // keyboard accessibility support.

    /// Set `aria-label` — a label string for screen readers.
    ///
    /// Use when the element has no visible text label (e.g., icon buttons).
    pub fn aria_label(self, label: &str) -> Self {
        self.attr("aria-label", label)
    }

    /// Set `aria-describedby` — ID of the element that describes this one.
    ///
    /// Use to associate error messages or help text with an input.
    pub fn aria_describedby(self, id: &str) -> Self {
        self.attr("aria-describedby", id)
    }

    /// Set `aria-hidden` — hides the element from the accessibility tree.
    ///
    /// Use for decorative elements that screen readers should skip.
    pub fn aria_hidden(self, hidden: bool) -> Self {
        self.attr("aria-hidden", if hidden { "true" } else { "false" })
    }

    /// Set `aria-current` — indicates the current item in a set.
    ///
    /// Common values: `"page"`, `"step"`, `"true"`.
    pub fn aria_current(self, value: &str) -> Self {
        self.attr("aria-current", value)
    }

    /// Set `aria-live` — defines a live region for dynamic content updates.
    ///
    /// Common values: `"polite"` (wait for idle), `"assertive"` (interrupt).
    pub fn aria_live(self, value: &str) -> Self {
        self.attr("aria-live", value)
    }

    /// Set `role` — the WAI-ARIA role of the element.
    ///
    /// Common values: `"alert"`, `"navigation"`, `"search"`, `"tablist"`.
    pub fn role(self, role: &str) -> Self {
        self.attr("role", role)
    }

    /// Set `aria-expanded` — whether an expandable element is open.
    pub fn aria_expanded(self, expanded: bool) -> Self {
        self.attr("aria-expanded", if expanded { "true" } else { "false" })
    }

    /// Set `aria-controls` — ID of the element this one controls.
    pub fn aria_controls(self, id: &str) -> Self {
        self.attr("aria-controls", id)
    }

    pub fn build(self) -> RenderElement {
        let attributes = if self.classes.is_empty() && self.attrs.is_empty() {
            None
        } else {
            let mut map = self.attrs;
            if !self.classes.is_empty() {
                map.insert(
                    "class".into(),
                    Value::Array(self.classes.into_iter().map(Value::String).collect()),
                );
            }
            Some(Value::Object(map))
        };

        RenderElement {
            element_type: self.element_type,
            weight: self.weight,
            tag: self.tag,
            value: self.value,
            format: self.format,
            attributes,
            children: self.children,
        }
    }
}

/// Create a container element (groups children).
pub fn container() -> ElementBuilder {
    ElementBuilder::new("container")
}

/// Create a markup element with an HTML tag and text value.
pub fn markup(tag: &str, value: &str) -> ElementBuilder {
    let mut b = ElementBuilder::new("markup");
    b.tag = Some(tag.into());
    b.value = Some(value.into());
    b
}

/// Create a markup element with a text format (for filtered HTML, etc.).
pub fn filtered_markup(value: &str, format: &str) -> ElementBuilder {
    let mut b = ElementBuilder::new("markup");
    b.value = Some(value.into());
    b.format = Some(format.into());
    b
}

/// Create a link element.
pub fn link(href: &str, text: &str) -> ElementBuilder {
    let mut b = ElementBuilder::new("markup");
    b.tag = Some("a".into());
    b.value = Some(text.into());
    b.attrs.insert("href".into(), Value::String(href.into()));
    b
}
