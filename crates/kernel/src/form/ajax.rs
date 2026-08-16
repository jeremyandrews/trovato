//! AJAX response building for form interactions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// AJAX response containing commands to execute on the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AjaxResponse {
    /// Commands to execute in order.
    pub commands: Vec<AjaxCommand>,
}

impl AjaxResponse {
    /// Create a new empty AJAX response.
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// Add a command to the response.
    pub fn command(mut self, command: AjaxCommand) -> Self {
        self.commands.push(command);
        self
    }

    /// Add a replace command.
    pub fn replace(self, selector: impl Into<String>, html: impl Into<String>) -> Self {
        self.command(AjaxCommand::Replace {
            selector: selector.into(),
            html: html.into(),
        })
    }

    /// Add an append command.
    pub fn append(self, selector: impl Into<String>, html: impl Into<String>) -> Self {
        self.command(AjaxCommand::Append {
            selector: selector.into(),
            html: html.into(),
        })
    }

    /// Add a prepend command.
    pub fn prepend(self, selector: impl Into<String>, html: impl Into<String>) -> Self {
        self.command(AjaxCommand::Prepend {
            selector: selector.into(),
            html: html.into(),
        })
    }

    /// Add a remove command.
    pub fn remove(self, selector: impl Into<String>) -> Self {
        self.command(AjaxCommand::Remove {
            selector: selector.into(),
        })
    }

    /// Add an invoke callback command.
    pub fn invoke(self, callback: impl Into<String>, args: Value) -> Self {
        self.command(AjaxCommand::InvokeCallback {
            callback: callback.into(),
            args,
        })
    }

    /// Add an alert message command.
    pub fn alert(self, message: impl Into<String>) -> Self {
        self.command(AjaxCommand::Alert {
            message: message.into(),
        })
    }

    /// Add a redirect command.
    pub fn redirect(self, url: impl Into<String>) -> Self {
        self.command(AjaxCommand::Redirect { url: url.into() })
    }

    /// Add a CSS add class command.
    pub fn add_class(self, selector: impl Into<String>, class: impl Into<String>) -> Self {
        self.command(AjaxCommand::AddClass {
            selector: selector.into(),
            class: class.into(),
        })
    }

    /// Add a CSS remove class command.
    pub fn remove_class(self, selector: impl Into<String>, class: impl Into<String>) -> Self {
        self.command(AjaxCommand::RemoveClass {
            selector: selector.into(),
            class: class.into(),
        })
    }

    /// Add a set attribute command.
    pub fn set_attr(
        self,
        selector: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.command(AjaxCommand::SetAttribute {
            selector: selector.into(),
            name: name.into(),
            value: value.into(),
        })
    }

    /// Check if the response is empty.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Default for AjaxResponse {
    fn default() -> Self {
        Self::new()
    }
}

/// Individual AJAX commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AjaxCommand {
    /// Replace element content.
    Replace { selector: String, html: String },

    /// Append content to element.
    Append { selector: String, html: String },

    /// Prepend content to element.
    Prepend { selector: String, html: String },

    /// Remove element.
    Remove { selector: String },

    /// Invoke a JavaScript callback.
    InvokeCallback { callback: String, args: Value },

    /// Show an alert message.
    Alert { message: String },

    /// Redirect to a URL.
    Redirect { url: String },

    /// Add a CSS class.
    AddClass { selector: String, class: String },

    /// Remove a CSS class.
    RemoveClass { selector: String, class: String },

    /// Set an HTML attribute.
    SetAttribute {
        selector: String,
        name: String,
        value: String,
    },

    /// Update form values.
    UpdateValues { selector: String, values: Value },

    /// Focus an element.
    Focus { selector: String },

    /// Scroll to an element.
    ScrollTo { selector: String },
}

impl AjaxCommand {
    /// Create a replace command.
    pub fn replace(selector: impl Into<String>, html: impl Into<String>) -> Self {
        Self::Replace {
            selector: selector.into(),
            html: html.into(),
        }
    }

    /// Create an append command.
    pub fn append(selector: impl Into<String>, html: impl Into<String>) -> Self {
        Self::Append {
            selector: selector.into(),
            html: html.into(),
        }
    }

    /// Create a remove command.
    pub fn remove(selector: impl Into<String>) -> Self {
        Self::Remove {
            selector: selector.into(),
        }
    }

    /// Create a redirect command.
    pub fn redirect(url: impl Into<String>) -> Self {
        Self::Redirect { url: url.into() }
    }
}

/// Request payload for AJAX form interactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AjaxRequest {
    /// The form build ID (identifies the form instance).
    pub form_build_id: String,

    /// The trigger element name (which element triggered the callback).
    pub trigger: String,

    /// Current form values.
    pub values: serde_json::Map<String, Value>,
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_ajax_response_builder() {
        let response = AjaxResponse::new()
            .replace("#container", "<div>New content</div>")
            .add_class("#element", "active");

        assert_eq!(response.commands.len(), 2);

        match &response.commands[0] {
            AjaxCommand::Replace { selector, html } => {
                assert_eq!(selector, "#container");
                assert_eq!(html, "<div>New content</div>");
            }
            _ => panic!("expected Replace command"),
        }
    }

    #[test]
    fn test_ajax_command_serialization() {
        let cmd = AjaxCommand::Replace {
            selector: "#test".to_string(),
            html: "<p>content</p>".to_string(),
        };

        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("replace"));
        assert!(json.contains("#test"));

        let parsed: AjaxCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, AjaxCommand::Replace { .. }));
    }

    #[test]
    fn test_ajax_response_empty() {
        let response = AjaxResponse::new();
        assert!(response.is_empty());

        let response = response.alert("test");
        assert!(!response.is_empty());
    }
}
