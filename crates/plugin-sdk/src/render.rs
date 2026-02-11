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
