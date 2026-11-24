//! Validation of a saved page's component props against their definitions.
//!
//! Every check here is server side and runs on every write path that goes
//! through `ItemService`, so an editor that skips the browser's checks, or a
//! client that posts JSON directly, meets the same rules.

use serde::Serialize;

use super::schema::{FieldDefinition, FieldKind, ReferenceTarget};

/// One refused value, located well enough for an editor to point at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    /// Where the component is in the page: `content[2]`, or
    /// `content[1].zones.zone-0[0]` for a child; `root` for a page root field.
    pub location: String,
    /// The component's type name; empty for a page root field.
    pub component: String,
    /// The field, with list positions: `title`, `cards[1].link`.
    pub field: String,
    /// What is wrong, in a sentence.
    pub message: String,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A page root error names no component: `root: footer_mode: ...`.
        if self.component.is_empty() {
            let mut parts: Vec<&str> = Vec::new();
            for part in [&self.location, &self.field] {
                if !part.is_empty() {
                    parts.push(part);
                }
            }
            parts.push(&self.message);
            return write!(f, "{}", parts.join(": "));
        }
        if self.field.is_empty() {
            write!(
                f,
                "{} ({}): {}",
                self.location, self.component, self.message
            )
        } else {
            write!(
                f,
                "{} ({}): {}: {}",
                self.location, self.component, self.field, self.message
            )
        }
    }
}

/// The URL schemes a `url`, `link` or `image` value may use; anything else
/// with a scheme (`javascript:`, `data:`) is refused.
const URL_SCHEMES: &[&str] = &["http", "https", "mailto", "tel"];

/// Longest URL accepted.
const MAX_URL_LENGTH: usize = 2048;

/// Whether `url` is safe to put in an `href` or `src`: relative, or one of
/// `URL_SCHEMES`, with no whitespace or control characters.
pub fn is_safe_url(url: &str) -> bool {
    if url.len() > MAX_URL_LENGTH || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    // A scheme is whatever precedes the first ':' when no '/', '?' or '#'
    // comes before it; `/a:b` and `?x=a:b` are relative.
    let first_special = url.find(['/', '?', '#']).unwrap_or(url.len());
    match url[..first_special].find(':') {
        None => true,
        Some(colon) => {
            let scheme = url[..colon].to_ascii_lowercase();
            URL_SCHEMES.contains(&scheme.as_str())
        }
    }
}

/// A gather query id as the gather admin accepts it.
pub fn is_gather_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Accumulates errors for one component.
pub(super) struct Checker<'a> {
    /// Where the component is.
    pub location: &'a str,
    /// Its type name.
    pub component: &'a str,
    /// Errors so far, for the whole page.
    pub errors: &'a mut Vec<FieldError>,
}

impl Checker<'_> {
    fn push(&mut self, field: &str, message: impl Into<String>) {
        self.errors.push(FieldError {
            location: self.location.to_string(),
            component: self.component.to_string(),
            field: field.to_string(),
            message: message.into(),
        });
    }

    /// Check every declared field of `props` (an object, or `null`).
    ///
    /// Props the definition does not declare are left alone: the editor adds
    /// `id`, and a page saved by an older editor may carry keys no field
    /// declares any more. Neither reaches the template, which receives
    /// declared fields only.
    pub fn check_props(
        &mut self,
        fields: &[FieldDefinition],
        props: &serde_json::Value,
        prefix: &str,
    ) {
        for field in fields {
            let path = if prefix.is_empty() {
                field.name.clone()
            } else {
                format!("{prefix}.{}", field.name)
            };
            let value = props.get(&field.name).unwrap_or(&serde_json::Value::Null);
            self.check_value(field, value, &path);
        }
    }

    fn check_value(&mut self, field: &FieldDefinition, value: &serde_json::Value, path: &str) {
        use serde_json::Value;

        if is_empty(value) {
            if field.required {
                self.push(path, "is required");
            }
            return;
        }

        match &field.kind {
            FieldKind::Text {
                max_length,
                min_length,
                ..
            } => {
                let Some(text) = value.as_str() else {
                    return self.push(path, "must be text");
                };
                let len = text.chars().count();
                if let Some(max) = max_length
                    && len > *max
                {
                    self.push(
                        path,
                        format!("must be at most {max} characters (has {len})"),
                    );
                }
                if let Some(min) = min_length
                    && len < *min
                {
                    self.push(
                        path,
                        format!("must be at least {min} characters (has {len})"),
                    );
                }
            }
            FieldKind::RichText { max_length, .. } => {
                let Some(text) = value.as_str() else {
                    return self.push(path, "must be text");
                };
                let len = text.chars().count();
                if let Some(max) = max_length
                    && len > *max
                {
                    self.push(
                        path,
                        format!("must be at most {max} characters (has {len})"),
                    );
                }
            }
            FieldKind::Number { min, max, integer } => {
                let Some(n) = value.as_f64() else {
                    return self.push(path, "must be a number");
                };
                if *integer && n.fract() != 0.0 {
                    self.push(path, "must be a whole number");
                }
                if let Some(min) = min
                    && n < *min
                {
                    self.push(path, format!("must be at least {min}"));
                }
                if let Some(max) = max
                    && n > *max
                {
                    self.push(path, format!("must be at most {max}"));
                }
            }
            FieldKind::HeadingLevel => match value.as_u64() {
                Some(2..=6) => {}
                _ => self.push(
                    path,
                    "must be a heading level from 2 to 6 (the page title is the level 1 heading)",
                ),
            },
            FieldKind::Url => match value.as_str() {
                Some(url) if is_safe_url(url) => {}
                Some(_) => self.push(
                    path,
                    "must be a relative URL or an http, https, mailto or tel URL",
                ),
                None => self.push(path, "must be a URL"),
            },
            FieldKind::Image { alt_max_length } => {
                let Some(obj) = value.as_object() else {
                    return self.push(path, "must be an image: { src, alt, decorative }");
                };
                let src = obj.get("src").and_then(Value::as_str).unwrap_or("");
                let alt = obj.get("alt").and_then(Value::as_str).unwrap_or("");
                let decorative = obj
                    .get("decorative")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if src.is_empty() {
                    if field.required {
                        self.push(path, "needs an image");
                    }
                    return;
                }
                if !is_safe_url(src) {
                    self.push(
                        &format!("{path}.src"),
                        "must be a relative URL or an http or https URL",
                    );
                }
                if !decorative && alt.trim().is_empty() {
                    self.push(
                        &format!("{path}.alt"),
                        "an image needs alt text, or must be marked decorative (WCAG 1.1.1)",
                    );
                }
                let len = alt.chars().count();
                if len > *alt_max_length {
                    self.push(
                        &format!("{path}.alt"),
                        format!("must be at most {alt_max_length} characters (has {len})"),
                    );
                }
            }
            FieldKind::Link { label_max_length } => {
                let Some(obj) = value.as_object() else {
                    return self.push(path, "must be a link: { url, label, new_tab }");
                };
                let url = obj.get("url").and_then(Value::as_str).unwrap_or("");
                let label = obj.get("label").and_then(Value::as_str).unwrap_or("");
                if let Some(new_tab) = obj.get("new_tab")
                    && !new_tab.is_boolean()
                    && !new_tab.is_null()
                {
                    self.push(&format!("{path}.new_tab"), "must be true or false");
                }
                if url.is_empty() {
                    if field.required {
                        self.push(&format!("{path}.url"), "is required");
                    }
                    return;
                }
                if !is_safe_url(url) {
                    self.push(
                        &format!("{path}.url"),
                        "must be a relative URL or an http, https, mailto or tel URL",
                    );
                }
                if label.trim().is_empty() {
                    self.push(
                        &format!("{path}.label"),
                        "a link needs a label a screen reader can announce",
                    );
                }
                let len = label.chars().count();
                if len > *label_max_length {
                    self.push(
                        &format!("{path}.label"),
                        format!("must be at most {label_max_length} characters (has {len})"),
                    );
                }
            }
            FieldKind::Select { options } => {
                let Some(choice) = value.as_str() else {
                    return self.push(path, "must be one of the listed values");
                };
                if !options.iter().any(|o| o.value == choice) {
                    let allowed: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
                    self.push(
                        path,
                        format!("'{choice}' is not one of: {}", allowed.join(", ")),
                    );
                }
            }
            FieldKind::Boolean => {
                if !value.is_boolean() {
                    self.push(path, "must be true or false");
                }
            }
            FieldKind::List {
                fields,
                min_items,
                max_items,
                ..
            } => {
                let Some(items) = value.as_array() else {
                    return self.push(path, "must be a list");
                };
                if let Some(max) = max_items
                    && items.len() > *max
                {
                    self.push(
                        path,
                        format!("may hold at most {max} entries (has {})", items.len()),
                    );
                }
                if let Some(min) = min_items
                    && items.len() < *min
                {
                    self.push(
                        path,
                        format!("needs at least {min} entries (has {})", items.len()),
                    );
                }
                for (i, item) in items.iter().enumerate() {
                    if !item.is_object() {
                        self.push(&format!("{path}[{i}]"), "must be an object");
                        continue;
                    }
                    self.check_props(fields, item, &format!("{path}[{i}]"));
                }
            }
            FieldKind::Reference { target, .. } => {
                let Some(id) = value.as_str() else {
                    return self.push(path, "must be an id");
                };
                match target {
                    ReferenceTarget::Item => {
                        if uuid::Uuid::parse_str(id).is_err() {
                            self.push(path, "must be a content item id (a UUID)");
                        }
                    }
                    ReferenceTarget::Gather => {
                        if !is_gather_id(id) {
                            self.push(path, "must be a gather query id");
                        }
                    }
                }
            }
        }
    }
}

/// Absent, `null`, or an empty string: what an editor leaves in a field the
/// author did not fill in.
fn is_empty(value: &serde_json::Value) -> bool {
    value.is_null() || value.as_str().is_some_and(str::is_empty)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn safe_urls() {
        for ok in [
            "/about",
            "about",
            "#top",
            "?q=a:b",
            "/a:b",
            "https://example.com/x",
            "HTTP://example.com",
            "mailto:a@example.com",
            "tel:+15551234",
        ] {
            assert!(is_safe_url(ok), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,x",
            "vbscript:x",
            "/a b",
            "java\tscript:x",
        ] {
            assert!(!is_safe_url(bad), "{bad}");
        }
    }

    #[test]
    fn gather_ids() {
        assert!(is_gather_id("recent_posts"));
        assert!(!is_gather_id("Recent"));
        assert!(!is_gather_id("a/b"));
        assert!(!is_gather_id(""));
    }
}
