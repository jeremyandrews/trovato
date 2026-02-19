//! Render tree consumer - converts RenderElement JSON to HTML via Tera.

use anyhow::{Context, Result};
use serde_json::Value;
use tera::{Context as TeraContext, Tera};

use crate::content::FilterPipeline;
use crate::routes::helpers::html_escape;
use trovato_sdk::render::RenderElement;

/// Consumer that converts RenderElement trees to HTML.
pub struct RenderTreeConsumer {
    _private: (),
}

impl RenderTreeConsumer {
    /// Create a new render tree consumer.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Render a RenderElement tree to HTML.
    pub fn render(
        &self,
        tera: &Tera,
        element: &RenderElement,
        context: &mut TeraContext,
    ) -> Result<String> {
        self.render_element(tera, element, context)
    }

    /// Render a single element and its children.
    fn render_element(
        &self,
        tera: &Tera,
        element: &RenderElement,
        context: &mut TeraContext,
    ) -> Result<String> {
        // Sort and render children first
        let children_html = self.render_children(tera, element, context)?;

        // Build element context
        let mut el_context = context.clone();
        el_context.insert("element", element);
        el_context.insert("children", &children_html);

        // Add processed value if present
        if let Some(value) = &element.value {
            let processed = self.process_value(value, element.format.as_deref());
            el_context.insert("value", &processed);
        }

        // Add attributes as individual values for easy access
        if let Some(attrs) = &element.attributes {
            el_context.insert("attributes", attrs);

            // Extract class list for convenience
            if let Some(classes) = attrs.get("class") {
                let class_str = self.classes_to_string(classes);
                el_context.insert("class", &class_str);
            }
        }

        // Determine template based on element type
        let template_name = self.template_for_type(&element.element_type);

        // Try to render with template, fall back to inline rendering
        if tera.get_template(&template_name).is_ok() {
            tera.render(&template_name, &el_context)
                .with_context(|| format!("failed to render element type: {}", element.element_type))
        } else {
            // Inline fallback rendering
            self.render_inline(element, &children_html)
        }
    }

    /// Render element children, sorted by weight.
    fn render_children(
        &self,
        tera: &Tera,
        element: &RenderElement,
        context: &mut TeraContext,
    ) -> Result<String> {
        use std::fmt::Write;

        if element.children.is_empty() {
            return Ok(String::new());
        }

        // Collect and sort children by weight
        let mut children: Vec<_> = element.children.iter().collect();
        children.sort_by_key(|(_, child)| child.weight.unwrap_or(0));

        let mut html = String::new();
        for (_key, child) in children {
            let child_html = self.render_element(tera, child, context)?;
            write!(html, "{}", child_html).unwrap();
        }

        Ok(html)
    }

    /// Process a value through the appropriate filter pipeline.
    fn process_value(&self, value: &str, format: Option<&str>) -> String {
        let format_name = format.unwrap_or("plain_text");
        let pipeline = FilterPipeline::for_format(format_name);
        pipeline.process(value)
    }

    /// Convert a classes value (array or string) to a space-separated string.
    fn classes_to_string(&self, classes: &Value) -> String {
        match classes {
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            Value::String(s) => s.clone(),
            _ => String::new(),
        }
    }

    /// Get the template name for an element type.
    fn template_for_type(&self, element_type: &str) -> String {
        format!("elements/{}.html", element_type)
    }

    /// Render an element inline when no template is available.
    fn render_inline(&self, element: &RenderElement, children: &str) -> Result<String> {
        match element.element_type.as_str() {
            "container" => self.render_container(element, children),
            "markup" => self.render_markup(element),
            _ => {
                // Unknown type - wrap in a div
                let class = self.get_class_string(element);
                Ok(format!(
                    "<div class=\"element element--{}{}\">{}</div>",
                    element.element_type,
                    if class.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", class)
                    },
                    children
                ))
            }
        }
    }

    /// Render a container element.
    fn render_container(&self, element: &RenderElement, children: &str) -> Result<String> {
        let class = self.get_class_string(element);
        let attrs = self.get_extra_attrs(element);

        Ok(format!(
            "<div class=\"container{}\"{}>{}</div>",
            if class.is_empty() {
                String::new()
            } else {
                format!(" {}", class)
            },
            attrs,
            children
        ))
    }

    /// Render a markup element.
    fn render_markup(&self, element: &RenderElement) -> Result<String> {
        let tag = element.tag.as_deref().unwrap_or("span");
        let value = element
            .value
            .as_ref()
            .map(|v| self.process_value(v, element.format.as_deref()))
            .unwrap_or_default();

        let class = self.get_class_string(element);
        let attrs = self.get_extra_attrs(element);

        // Void elements (no closing tag)
        let void_elements = ["br", "hr", "img", "input", "meta", "link"];
        if void_elements.contains(&tag) {
            return Ok(format!(
                "<{}{}{} />",
                tag,
                if class.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"{}\"", class)
                },
                attrs
            ));
        }

        Ok(format!(
            "<{}{}{}>{}</{}>",
            tag,
            if class.is_empty() {
                String::new()
            } else {
                format!(" class=\"{}\"", class)
            },
            attrs,
            value,
            tag
        ))
    }

    /// Get class string from element attributes.
    fn get_class_string(&self, element: &RenderElement) -> String {
        element
            .attributes
            .as_ref()
            .and_then(|attrs| attrs.get("class"))
            .map(|classes| self.classes_to_string(classes))
            .unwrap_or_default()
    }

    /// Get extra attributes (excluding class) as a string.
    fn get_extra_attrs(&self, element: &RenderElement) -> String {
        let Some(attrs) = &element.attributes else {
            return String::new();
        };

        let Value::Object(obj) = attrs else {
            return String::new();
        };

        obj.iter()
            .filter(|(k, _)| *k != "class")
            .map(|(k, v)| {
                let value = match v {
                    Value::String(s) => html_escape(s),
                    Value::Bool(b) => {
                        if *b {
                            return format!(" {}", k);
                        } else {
                            return String::new();
                        }
                    }
                    _ => v.to_string(),
                };
                format!(" {}=\"{}\"", k, value)
            })
            .collect()
    }
}

impl Default for RenderTreeConsumer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_render_markup_basic() {
        let consumer = RenderTreeConsumer::new();

        let element = RenderElement {
            element_type: "markup".to_string(),
            weight: None,
            tag: Some("p".to_string()),
            value: Some("Hello world".to_string()),
            format: Some("plain_text".to_string()),
            attributes: None,
            children: BTreeMap::new(),
        };

        let result = consumer.render_markup(&element).unwrap();
        assert_eq!(result, "<p>Hello world</p>");
    }

    #[test]
    fn test_render_markup_with_class() {
        let consumer = RenderTreeConsumer::new();

        let mut attrs = serde_json::Map::new();
        attrs.insert(
            "class".to_string(),
            Value::Array(vec![Value::String("text".to_string())]),
        );

        let element = RenderElement {
            element_type: "markup".to_string(),
            weight: None,
            tag: Some("span".to_string()),
            value: Some("Test".to_string()),
            format: None,
            attributes: Some(Value::Object(attrs)),
            children: BTreeMap::new(),
        };

        let result = consumer.render_markup(&element).unwrap();
        assert!(result.contains("class=\"text\""));
    }

    #[test]
    fn test_render_container() {
        let consumer = RenderTreeConsumer::new();

        let element = RenderElement {
            element_type: "container".to_string(),
            weight: None,
            tag: None,
            value: None,
            format: None,
            attributes: None,
            children: BTreeMap::new(),
        };

        let result = consumer.render_container(&element, "<p>Child</p>").unwrap();
        assert!(result.contains("container"));
        assert!(result.contains("<p>Child</p>"));
    }

    #[test]
    fn test_process_value_plain_text() {
        let consumer = RenderTreeConsumer::new();
        let result = consumer.process_value("<script>alert('xss')</script>", Some("plain_text"));
        assert!(!result.contains("<script>"));
        assert!(result.contains("&lt;script&gt;"));
    }

    #[test]
    fn test_classes_to_string_array() {
        let consumer = RenderTreeConsumer::new();
        let classes = Value::Array(vec![
            Value::String("foo".to_string()),
            Value::String("bar".to_string()),
        ]);
        assert_eq!(consumer.classes_to_string(&classes), "foo bar");
    }

    #[test]
    fn test_classes_to_string_string() {
        let consumer = RenderTreeConsumer::new();
        let classes = Value::String("foo bar".to_string());
        assert_eq!(consumer.classes_to_string(&classes), "foo bar");
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#x27;");
    }
}
