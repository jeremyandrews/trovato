//! Registry of the scopes plugins declare from `tap_assistant_scopes`.
//!
//! Built once at boot from the tap's JSON output, exactly like
//! [`crate::menu::MenuRegistry`], and for the same reason: a scope names a URL
//! the kernel has to route and a permission it has to check, so it must be known
//! before the first request rather than discovered during one.
//!
//! # Validation is a drop, never a failure
//!
//! One plugin's malformed scope must not stop a site booting. Every rejection is
//! recorded in [`AssistantRegistry::rejections`] with the plugin, the scope name
//! and the reason, logged once at startup, and shown on the assistant admin
//! page — so a bad declaration is loud without being fatal.
//!
//! The rules, all of them structural rather than stylistic:
//!
//! - `name` and every tool `name` match `[a-z0-9_]+`. The scope name is a path
//!   segment and the tool name is what a model emits, so both have to be free of
//!   anything that would need escaping in either place.
//! - A scope name is unique across **all** plugins. Two plugins claiming the
//!   same name would make `/ai/assistant/<name>` ambiguous; the second one loses
//!   and the warning names both.
//! - `parameters` is a JSON object with `"type": "object"`. Both provider
//!   protocols take a JSON Schema object here and nothing else.
//! - At most [`MAX_TOOLS_PER_SCOPE`] tools, [`MAX_SUGGESTIONS`] suggestions, and
//!   a prompt of at most [`MAX_PROMPT_BYTES`]. Tools and prompt go into every
//!   request the model gets, so an unbounded scope is an unbounded bill.
//!
//! # Why this is public API
//!
//! An external plugin workspace (netgrasp-trovato is the first) validates its own
//! `tap_assistant_scopes` output against this registry in its tests, so it finds
//! out at `cargo test` that a scope would be dropped rather than at boot on a
//! site. [`AssistantRegistry::from_tap_results`], [`AssistantRegistry::scopes`],
//! [`AssistantRegistry::get`] and [`AssistantRegistry::rejections`] exist for
//! that as much as for the kernel.

use std::collections::HashMap;

use tracing::warn;
use trovato_sdk::types::{AssistantIdKind, AssistantScope, AssistantToolKind};

/// Largest number of tools one scope may declare.
pub const MAX_TOOLS_PER_SCOPE: usize = 32;
/// Largest number of starter questions one scope may declare.
pub const MAX_SUGGESTIONS: usize = 6;
/// Largest stock domain prompt, in bytes.
pub const MAX_PROMPT_BYTES: usize = 8 * 1024;

/// A scope that survived validation, with the plugin that declared it.
#[derive(Debug, Clone)]
pub struct RegisteredScope {
    /// The plugin that declared this scope.
    pub plugin: String,
    /// The scope itself, as declared.
    pub scope: AssistantScope,
}

impl RegisteredScope {
    /// The tool by that name, if this scope declared one.
    ///
    /// The kernel dispatches only what this returns: a name the scope did not
    /// declare is reported to the model as an error and never reaches the plugin.
    pub fn tool(&self, name: &str) -> Option<&trovato_sdk::types::AssistantTool> {
        self.scope.tools.iter().find(|tool| tool.name == name)
    }

    /// Whether this scope applies to items of the given content type.
    pub fn applies_to_item_type(&self, item_type: &str) -> bool {
        self.scope.id_kind == AssistantIdKind::Item
            && self.scope.item_types.iter().any(|t| t == item_type)
    }

    /// How many write tools this scope declares.
    pub fn write_tool_count(&self) -> usize {
        self.scope
            .tools
            .iter()
            .filter(|tool| tool.kind == AssistantToolKind::Write)
            .count()
    }
}

/// One scope that was declared and dropped, and why.
#[derive(Debug, Clone)]
pub struct ScopeRejection {
    /// The plugin that declared it.
    pub plugin: String,
    /// The scope's declared name, or `"<unnamed>"` when it had none.
    pub scope: String,
    /// Why it was dropped, in a sentence an operator can act on.
    pub reason: String,
}

/// Every scope the site knows about, keyed by name.
#[derive(Debug, Default)]
pub struct AssistantRegistry {
    scopes: HashMap<String, RegisteredScope>,
    /// Insertion order, so listings are stable rather than hash-ordered.
    order: Vec<String>,
    rejections: Vec<ScopeRejection>,
}

impl AssistantRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from `tap_assistant_scopes` output.
    ///
    /// Each element is a `(plugin_name, json_array)` pair, in the order the taps
    /// were dispatched. A plugin whose output does not parse contributes one
    /// rejection and no scopes.
    pub fn from_tap_results(results: Vec<(String, String)>) -> Self {
        let mut registry = Self::new();

        for (plugin, json) in results {
            // `#[plugin_tap]` serializes the return value, so an array arrives as
            // a JSON array — but a String-returning tap would arrive as a JSON
            // string wrapping one, the same double encoding the view and api
            // paths already accept (G-VIEW-OUTPUT-JSON-ENCODED).
            let parsed = serde_json::from_str::<serde_json::Value>(&json)
                .map(|value| match value {
                    serde_json::Value::String(ref inner) => {
                        serde_json::from_str::<serde_json::Value>(inner).unwrap_or(value)
                    }
                    other => other,
                })
                .and_then(serde_json::from_value::<Vec<AssistantScope>>);

            match parsed {
                Ok(scopes) => {
                    for scope in scopes {
                        registry.register(&plugin, scope);
                    }
                }
                Err(e) => {
                    registry.reject(&plugin, "<unparsed>", format!("output did not parse: {e}"));
                }
            }
        }

        for rejection in &registry.rejections {
            warn!(
                plugin = %rejection.plugin,
                scope = %rejection.scope,
                reason = %rejection.reason,
                "dropping an invalid assistant scope"
            );
        }

        registry
    }

    /// Validate and add one scope, or record why it was dropped.
    pub fn register(&mut self, plugin: &str, scope: AssistantScope) {
        let name = scope.name.clone();
        let display = if name.is_empty() {
            "<unnamed>".to_string()
        } else {
            name.clone()
        };

        if let Err(reason) = validate(&scope) {
            self.reject(plugin, &display, reason);
            return;
        }

        if let Some(existing) = self.scopes.get(&name) {
            self.reject(
                plugin,
                &display,
                format!(
                    "scope name '{name}' is already declared by plugin '{}'; \
                     a scope name must be unique across every plugin",
                    existing.plugin
                ),
            );
            return;
        }

        self.order.push(name.clone());
        self.scopes.insert(
            name,
            RegisteredScope {
                plugin: plugin.to_string(),
                scope,
            },
        );
    }

    fn reject(&mut self, plugin: &str, scope: &str, reason: impl Into<String>) {
        self.rejections.push(ScopeRejection {
            plugin: plugin.to_string(),
            scope: scope.to_string(),
            reason: reason.into(),
        });
    }

    /// Every registered scope, in declaration order.
    pub fn scopes(&self) -> impl Iterator<Item = &RegisteredScope> {
        self.order
            .iter()
            .filter_map(|name| self.scopes.get(name.as_str()))
    }

    /// The scope with this name, if any.
    pub fn get(&self, name: &str) -> Option<&RegisteredScope> {
        self.scopes.get(name)
    }

    /// Every scope that was declared and dropped, with the reason.
    pub fn rejections(&self) -> &[ScopeRejection] {
        &self.rejections
    }

    /// How many scopes are registered.
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    /// Whether no scope is registered.
    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    /// Every scope that applies to items of this content type, in declaration
    /// order. This is what puts the launcher on an item's page.
    pub fn scopes_for_item_type(&self, item_type: &str) -> Vec<&RegisteredScope> {
        self.scopes()
            .filter(|registered| registered.applies_to_item_type(item_type))
            .collect()
    }
}

/// Whether a machine name is `[a-z0-9_]+`.
fn is_machine_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Check one scope, returning the reason it cannot be registered.
fn validate(scope: &AssistantScope) -> Result<(), String> {
    if !is_machine_name(&scope.name) {
        return Err(format!(
            "scope name '{}' must be one or more of [a-z0-9_]",
            scope.name
        ));
    }
    if scope.label.trim().is_empty() {
        return Err("scope has no label".to_string());
    }
    if scope.prompt.len() > MAX_PROMPT_BYTES {
        return Err(format!(
            "prompt is {} bytes, over the {MAX_PROMPT_BYTES}-byte limit",
            scope.prompt.len()
        ));
    }
    if scope.suggestions.len() > MAX_SUGGESTIONS {
        return Err(format!(
            "{} suggestions, over the limit of {MAX_SUGGESTIONS}",
            scope.suggestions.len()
        ));
    }
    if scope.tools.len() > MAX_TOOLS_PER_SCOPE {
        return Err(format!(
            "{} tools, over the limit of {MAX_TOOLS_PER_SCOPE}",
            scope.tools.len()
        ));
    }
    if scope.id_kind == AssistantIdKind::Item && scope.item_types.is_empty() {
        return Err("an item scope must name at least one content type".to_string());
    }

    let mut seen: Vec<&str> = Vec::with_capacity(scope.tools.len());
    for tool in &scope.tools {
        if !is_machine_name(&tool.name) {
            return Err(format!(
                "tool name '{}' must be one or more of [a-z0-9_]",
                tool.name
            ));
        }
        if seen.contains(&tool.name.as_str()) {
            return Err(format!("tool '{}' is declared twice", tool.name));
        }
        seen.push(&tool.name);

        match tool.parameters.as_object() {
            Some(object) => {
                if object.get("type").and_then(serde_json::Value::as_str) != Some("object") {
                    return Err(format!(
                        "tool '{}' parameters must be a JSON Schema object with \"type\": \"object\"",
                        tool.name
                    ));
                }
            }
            None => {
                return Err(format!(
                    "tool '{}' parameters must be a JSON object",
                    tool.name
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use trovato_sdk::types::{AssistantRisk, AssistantTool};

    fn scope(name: &str) -> AssistantScope {
        AssistantScope::new(name, "A scope", "administer site", AssistantIdKind::None)
            .prompt("Do the thing.")
    }

    fn as_results(plugin: &str, scopes: &[AssistantScope]) -> Vec<(String, String)> {
        vec![(plugin.to_string(), serde_json::to_string(scopes).unwrap())]
    }

    #[test]
    fn a_valid_scope_registers_and_is_retrievable() {
        let registry = AssistantRegistry::from_tap_results(as_results(
            "widgets",
            &[scope("test_widget").tool(AssistantTool::read("read_widget", "Read it"))],
        ));

        assert_eq!(registry.len(), 1);
        let registered = registry.get("test_widget").expect("registered");
        assert_eq!(registered.plugin, "widgets");
        assert!(registered.tool("read_widget").is_some());
        assert!(registered.tool("nope").is_none());
        assert!(registry.rejections().is_empty());
    }

    #[test]
    fn a_tool_name_with_a_space_drops_the_scope_and_names_the_reason() {
        let registry = AssistantRegistry::from_tap_results(as_results(
            "widgets",
            &[
                scope("good_scope"),
                scope("bad_scope").tool(AssistantTool::read("read widget", "Read it")),
            ],
        ));

        assert!(registry.get("good_scope").is_some());
        assert!(registry.get("bad_scope").is_none());
        let rejection = registry
            .rejections()
            .iter()
            .find(|r| r.scope == "bad_scope")
            .expect("the invalid scope is recorded");
        assert_eq!(rejection.plugin, "widgets");
        assert!(rejection.reason.contains("read widget"), "{rejection:?}");
    }

    #[test]
    fn a_duplicate_scope_name_loses_and_the_warning_names_both_plugins() {
        let mut results = as_results("first", &[scope("shared")]);
        results.extend(as_results("second", &[scope("shared")]));
        let registry = AssistantRegistry::from_tap_results(results);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get("shared").unwrap().plugin, "first");
        let rejection = &registry.rejections()[0];
        assert_eq!(rejection.plugin, "second");
        assert!(rejection.reason.contains("'first'"), "{rejection:?}");
    }

    #[test]
    fn caps_on_tools_suggestions_and_prompt_are_enforced() {
        let too_many_tools = (0..MAX_TOOLS_PER_SCOPE + 1)
            .map(|i| AssistantTool::read(format!("t{i}"), "x"))
            .collect::<Vec<_>>();
        let cases = vec![
            ("many_tools", scope("many_tools").tools(too_many_tools)),
            (
                "many_suggestions",
                scope("many_suggestions")
                    .suggestions((0..MAX_SUGGESTIONS + 1).map(|i| format!("q{i}"))),
            ),
            (
                "long_prompt",
                scope("long_prompt").prompt("x".repeat(MAX_PROMPT_BYTES + 1)),
            ),
        ];

        for (name, declared) in cases {
            let registry = AssistantRegistry::from_tap_results(as_results("p", &[declared]));
            assert!(registry.get(name).is_none(), "{name} should be dropped");
            assert_eq!(registry.rejections().len(), 1, "{name}");
        }
    }

    #[test]
    fn tool_parameters_must_be_a_json_schema_object() {
        for bad in [
            serde_json::json!("not an object"),
            serde_json::json!({"type": "string"}),
            serde_json::json!([]),
        ] {
            let registry = AssistantRegistry::from_tap_results(as_results(
                "p",
                &[scope("s").tool(
                    AssistantTool::write("w", "Write", AssistantRisk::Normal).parameters(bad),
                )],
            ));
            assert!(registry.is_empty());
            assert!(registry.rejections()[0].reason.contains("parameters"));
        }
    }

    #[test]
    fn an_item_scope_must_name_a_content_type() {
        let without = AssistantScope::new("s", "S", "p", AssistantIdKind::Item);
        let registry = AssistantRegistry::from_tap_results(as_results("p", &[without]));
        assert!(registry.is_empty());

        let with =
            AssistantScope::new("s", "S", "p", AssistantIdKind::Item).item_types(["ng_device"]);
        let registry = AssistantRegistry::from_tap_results(as_results("p", &[with]));
        assert_eq!(registry.scopes_for_item_type("ng_device").len(), 1);
        assert!(registry.scopes_for_item_type("ng_person").is_empty());
    }

    #[test]
    fn unparseable_tap_output_is_a_rejection_not_a_panic() {
        let registry =
            AssistantRegistry::from_tap_results(vec![("p".to_string(), "not json".to_string())]);
        assert!(registry.is_empty());
        assert_eq!(registry.rejections()[0].scope, "<unparsed>");
    }

    #[test]
    fn a_double_encoded_json_string_is_accepted_like_the_other_tap_paths() {
        let scopes = vec![scope("wrapped")];
        let inner = serde_json::to_string(&scopes).unwrap();
        let wrapped = serde_json::to_string(&inner).unwrap();
        let registry = AssistantRegistry::from_tap_results(vec![("p".to_string(), wrapped)]);
        assert!(registry.get("wrapped").is_some());
    }

    #[test]
    fn scopes_are_listed_in_declaration_order() {
        let registry = AssistantRegistry::from_tap_results(as_results(
            "p",
            &[scope("b_scope"), scope("a_scope"), scope("c_scope")],
        ));
        let names: Vec<&str> = registry.scopes().map(|s| s.scope.name.as_str()).collect();
        assert_eq!(names, ["b_scope", "a_scope", "c_scope"]);
    }

    #[test]
    fn write_tools_are_counted_separately_from_reads() {
        let registered = RegisteredScope {
            plugin: "p".to_string(),
            scope: scope("s")
                .tool(AssistantTool::read("r", "Read"))
                .tool(AssistantTool::write("w", "Write", AssistantRisk::High)),
        };
        assert_eq!(registered.write_tool_count(), 1);
    }
}
