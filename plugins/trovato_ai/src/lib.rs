//! AI content enrichment plugin for Trovato.
//!
//! Provides field rules (auto-fill content via AI on save), form assist
//! buttons (rewrite/expand/shorten/translate inline), and chat action
//! registration for plugin-extensible chatbot capabilities.
//!
//! The kernel provides `ai_request()` as a host function — this plugin
//! uses it to bring AI capabilities into the content editing workflow.
//! API keys, rate limits, and token budgets are managed by the kernel;
//! this plugin focuses on content-level intelligence.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use trovato_sdk::host;
use trovato_sdk::prelude::*;

// ---- Field Rule Types ----

/// Configuration for an AI field rule that fires on item save.
///
/// Rules are stored in `site_config` under key `trovato_ai.field_rules`
/// as a JSON array. The admin UI writes them; this plugin reads them at
/// presave time.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FieldRule {
    /// Content type this rule applies to (e.g., "conference").
    item_type: String,
    /// Source field whose value feeds the prompt (e.g., "field_description").
    source_field: String,
    /// Target field that receives the AI output (e.g., "field_summary").
    target_field: String,
    /// When to fire: "on_change" or "always".
    #[serde(default = "default_trigger")]
    trigger: String,
    /// AI operation type — currently only "chat" is supported.
    #[serde(default = "default_operation")]
    operation: String,
    /// Prompt template with `{field_name}` placeholders for field values.
    prompt: String,
    /// How to apply: "fill_if_empty", "always_update", or "suggest".
    #[serde(default = "default_behavior")]
    behavior: String,
    /// Execution order — lower weight runs first.
    #[serde(default)]
    weight: i32,
}

fn default_trigger() -> String {
    "on_change".to_string()
}

fn default_operation() -> String {
    "chat".to_string()
}

fn default_behavior() -> String {
    "fill_if_empty".to_string()
}

/// Wrapper for the site_config JSON value containing rules.
#[derive(Debug, Deserialize)]
struct FieldRulesConfig {
    #[serde(default)]
    rules: Vec<FieldRule>,
}

/// Row returned from `site_config` query.
#[derive(Debug, Deserialize)]
struct SiteConfigRow {
    value: serde_json::Value,
}

/// Presave input from the kernel.
///
/// The kernel serializes `{item_type, title, fields, status}` — not a
/// full Item — so we use a dedicated struct with serde defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresaveInput {
    item_type: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    fields: HashMap<String, serde_json::Value>,
    #[serde(default)]
    status: i32,
}

// ---- Permissions ----

/// Register AI content permissions.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("use ai", "Use AI features in content editing"),
        PermissionDefinition::new("use ai chat", "Use AI chat and content assist features"),
        PermissionDefinition::new(
            "use ai embeddings",
            "Generate embeddings for semantic search",
        ),
        PermissionDefinition::new("use ai image generation", "Generate images via AI"),
        PermissionDefinition::new(
            "configure ai",
            "Configure AI providers, field rules, and chat settings",
        ),
        PermissionDefinition::new("view ai usage", "View AI usage dashboard and reports"),
    ]
}

// ---- Menu routes ----

/// Register AI admin routes.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![MenuDefinition::new("/admin/config/ai", "AI Configuration")]
}

// ---- Field Rules (tap_item_presave) ----

/// Load field rules from site_config.
fn load_field_rules() -> Vec<FieldRule> {
    let rows_json = match host::query_raw(
        "SELECT value FROM site_config WHERE name = $1",
        &[serde_json::json!("trovato_ai.field_rules")],
    ) {
        Ok(json) => json,
        Err(code) => {
            host::log(
                "debug",
                "trovato_ai",
                &format!("field rules query returned error code {code}"),
            );
            return Vec::new();
        }
    };

    let rows: Vec<SiteConfigRow> = match serde_json::from_str(&rows_json) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let Some(row) = rows.first() else {
        return Vec::new();
    };

    // Try parsing as {rules: [...]} first, then as a plain array
    if let Ok(config) = serde_json::from_value::<FieldRulesConfig>(row.value.clone()) {
        return config.rules;
    }
    if let Ok(rules) = serde_json::from_value::<Vec<FieldRule>>(row.value.clone()) {
        return rules;
    }

    host::log(
        "warn",
        "trovato_ai",
        "failed to parse field rules from site_config",
    );
    Vec::new()
}

/// Resolve `{field_name}` placeholders in a prompt template.
fn resolve_prompt(template: &str, fields: &HashMap<String, serde_json::Value>) -> String {
    let mut result = template.to_string();
    for (key, value) in fields {
        let placeholder = format!("{{{key}}}");
        if result.contains(&placeholder) {
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &text);
        }
    }
    result
}

/// Check if a field has a non-empty value.
fn field_has_value(fields: &HashMap<String, serde_json::Value>, name: &str) -> bool {
    fields.get(name).is_some_and(|v| match v {
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Null => false,
        _ => true,
    })
}

/// Process AI field rules before an item is saved.
///
/// Reads field rule configuration from site config, evaluates which
/// rules apply to the current item type, and calls `ai_request()` to
/// generate enriched content for target fields.
///
/// Rules are evaluated in weight order (lower first). A rule's target
/// can be a subsequent rule's source, enabling chaining.
#[plugin_tap]
pub fn tap_item_presave(input_json: String) -> String {
    if !host::current_user_has_permission("use ai") {
        return input_json;
    }

    let mut input: PresaveInput = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(_) => return input_json,
    };

    let mut rules = load_field_rules();
    if rules.is_empty() {
        return input_json;
    }

    // Filter and sort rules for this item type
    rules.retain(|r| r.item_type == input.item_type);
    rules.sort_by_key(|r| r.weight);

    if rules.is_empty() {
        return input_json;
    }

    for rule in &rules {
        // fill_if_empty: skip if target already has a value
        if rule.behavior == "fill_if_empty" && field_has_value(&input.fields, &rule.target_field) {
            host::log(
                "debug",
                "trovato_ai",
                &format!(
                    "skip rule {}.{}: target has value",
                    rule.item_type, rule.target_field
                ),
            );
            continue;
        }

        // suggest: store metadata only, don't modify field (future UI feature)
        if rule.behavior == "suggest" {
            continue;
        }

        // Source field must have content to enrich from
        if !field_has_value(&input.fields, &rule.source_field) {
            continue;
        }

        let prompt = resolve_prompt(&rule.prompt, &input.fields);
        if prompt.is_empty() {
            continue;
        }

        let request = AiRequest {
            operation: AiOperationType::Chat,
            provider_id: None,
            model: None,
            messages: vec![
                AiMessage::system(
                    "You are a content enrichment assistant. Respond with only the \
                     requested content — no explanations, no markdown formatting, \
                     no quotes around the text.",
                ),
                AiMessage::user(&prompt),
            ],
            input: None,
            options: AiRequestOptions {
                max_tokens: Some(500),
                ..AiRequestOptions::default()
            },
        };

        match host::ai_request(&request) {
            Ok(response) => {
                let content = response.content.trim().to_string();
                if !content.is_empty() {
                    input.fields.insert(
                        rule.target_field.clone(),
                        serde_json::Value::String(content),
                    );
                    host::log(
                        "info",
                        "trovato_ai",
                        &format!(
                            "enriched {}.{} via {} ({}ms, {} tokens)",
                            rule.item_type,
                            rule.target_field,
                            response.model,
                            response.latency_ms,
                            response.usage.total_tokens,
                        ),
                    );
                }
            }
            Err(code) => {
                host::log(
                    "warn",
                    "trovato_ai",
                    &format!(
                        "ai_request failed for {}.{}: error {}",
                        rule.item_type, rule.target_field, code
                    ),
                );
            }
        }
    }

    serde_json::to_string(&input).unwrap_or(input_json)
}

// ---- Form Assist (tap_form_alter) ----

/// Inject AI Assist buttons into content editing forms.
///
/// Adds "AI Assist" button markup as a suffix on text-type form elements
/// for users with the `use ai chat` permission. The buttons trigger
/// client-side JS (`static/js/ai-assist.js`) that calls
/// `POST /api/v1/ai/assist` with the field value and selected operation.
#[plugin_tap]
pub fn tap_form_alter(form_json: String) -> String {
    if !host::current_user_has_permission("use ai chat") {
        return form_json;
    }

    let mut form: serde_json::Value = match serde_json::from_str(&form_json) {
        Ok(v) => v,
        Err(_) => return form_json,
    };

    // Only alter content editing forms (item add/edit)
    let form_id = form.get("form_id").and_then(|v| v.as_str()).unwrap_or("");
    if !form_id.starts_with("item_") {
        return form_json;
    }

    // Inject AI Assist buttons on text fields
    let Some(elements) = form.get_mut("elements").and_then(|v| v.as_object_mut()) else {
        return form_json;
    };

    let text_field_names: Vec<String> = elements
        .iter()
        .filter_map(|(name, el)| {
            let el_type = el.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if el_type == "textfield" || el_type == "textarea" {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    for name in &text_field_names {
        if let Some(element) = elements.get_mut(name) {
            let btn_html = format!(
                r#"<button type="button" class="ai-assist-btn" data-field="{name}" title="AI Assist">AI Assist</button>"#
            );
            element
                .as_object_mut()
                .map(|obj| obj.insert("suffix".to_string(), serde_json::json!(btn_html)));
        }
    }

    serde_json::to_string(&form).unwrap_or(form_json)
}

// ---- Chat Actions (tap_chat_actions) ----

/// Register chatbot actions that this plugin provides.
///
/// Actions are described to the LLM via function-calling format so
/// it can invoke them during conversation. The chatbot kernel handles
/// the LLM interaction; this plugin adds domain-specific actions.
#[plugin_tap]
pub fn tap_chat_actions() -> String {
    serde_json::json!({
        "actions": [
            {
                "name": "search_conferences",
                "description": "Search for conferences by keyword, topic, or location",
                "parameters": {
                    "query": {"type": "string", "description": "Search query"},
                    "topic": {"type": "string", "description": "Topic slug (optional)"},
                    "country": {"type": "string", "description": "Country name (optional)"}
                }
            },
            {
                "name": "get_conference_details",
                "description": "Get full details of a specific conference by ID",
                "parameters": {
                    "id": {"type": "string", "description": "Conference UUID"}
                }
            },
            {
                "name": "list_upcoming_cfps",
                "description": "List conferences with open Calls for Papers",
                "parameters": {}
            }
        ]
    })
    .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn permissions_include_core_ai_perms() {
        let perms = __inner_tap_perm();
        assert!(perms.len() >= 6);

        let names: Vec<&str> = perms.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"use ai"));
        assert!(names.contains(&"use ai chat"));
        assert!(names.contains(&"configure ai"));
        assert!(names.contains(&"view ai usage"));
    }

    #[test]
    fn menu_registers_ai_config_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/ai");
    }

    #[test]
    fn chat_actions_returns_valid_json() {
        let json_str = __inner_tap_chat_actions();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let actions = parsed.get("actions").unwrap().as_array().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(
            actions[0].get("name").unwrap().as_str().unwrap(),
            "search_conferences"
        );
    }

    #[test]
    fn resolve_prompt_substitutes_fields() {
        let mut fields = HashMap::new();
        fields.insert(
            "field_description".to_string(),
            serde_json::json!("A great Rust conference in Berlin"),
        );
        fields.insert("field_name".to_string(), serde_json::json!("RustFest"));
        let result = resolve_prompt(
            "Summarize: {field_description} (name: {field_name})",
            &fields,
        );
        assert_eq!(
            result,
            "Summarize: A great Rust conference in Berlin (name: RustFest)"
        );
    }

    #[test]
    fn resolve_prompt_leaves_unknown_placeholders() {
        let mut fields = HashMap::new();
        fields.insert("field_a".to_string(), serde_json::json!("hello"));
        let result = resolve_prompt("{field_a} and {field_b}", &fields);
        assert_eq!(result, "hello and {field_b}");
    }

    #[test]
    fn field_rule_deserializes_with_defaults() {
        let json = r#"{
            "item_type": "conference",
            "source_field": "field_description",
            "target_field": "field_summary",
            "prompt": "Summarize: {field_description}"
        }"#;
        let rule: FieldRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.trigger, "on_change");
        assert_eq!(rule.operation, "chat");
        assert_eq!(rule.behavior, "fill_if_empty");
        assert_eq!(rule.weight, 0);
    }

    #[test]
    fn field_has_value_checks_empty() {
        let mut fields = HashMap::new();
        fields.insert("a".to_string(), serde_json::json!(""));
        fields.insert("b".to_string(), serde_json::Value::Null);
        fields.insert("c".to_string(), serde_json::json!("hello"));
        fields.insert("d".to_string(), serde_json::json!(42));

        assert!(!field_has_value(&fields, "a"));
        assert!(!field_has_value(&fields, "b"));
        assert!(field_has_value(&fields, "c"));
        assert!(field_has_value(&fields, "d"));
        assert!(!field_has_value(&fields, "missing"));
    }

    #[test]
    fn presave_returns_unchanged_without_rules() {
        // Native stub query_raw returns "[]", so no rules will load
        let input = serde_json::json!({
            "item_type": "conference",
            "title": "Test",
            "fields": {"field_description": "A test conference"},
            "status": 1
        });
        let input_str = serde_json::to_string(&input).unwrap();
        let result = __inner_tap_item_presave(input_str);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["fields"]["field_description"], "A test conference");
    }

    #[test]
    fn form_alter_skips_non_item_forms() {
        let form = serde_json::json!({
            "form_id": "login_form",
            "elements": {}
        });
        let json = serde_json::to_string(&form).unwrap();
        let result = __inner_tap_form_alter(json);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["form_id"], "login_form");
    }

    #[test]
    fn form_alter_injects_buttons_on_text_fields() {
        let form = serde_json::json!({
            "form_id": "item_conference_edit",
            "elements": {
                "field_description": {
                    "type": "textarea",
                    "rows": 5,
                    "title": "Description"
                },
                "field_name": {
                    "type": "textfield",
                    "title": "Name"
                },
                "field_status": {
                    "type": "select",
                    "title": "Status",
                    "options": []
                }
            }
        });
        let json = serde_json::to_string(&form).unwrap();
        let result = __inner_tap_form_alter(json);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

        // Text fields should have AI assist button suffix
        let desc = &parsed["elements"]["field_description"];
        assert!(desc["suffix"].as_str().unwrap().contains("ai-assist-btn"));

        let name = &parsed["elements"]["field_name"];
        assert!(name["suffix"].as_str().unwrap().contains("ai-assist-btn"));

        // Select field should NOT have suffix
        assert!(parsed["elements"]["field_status"]["suffix"].is_null());
    }
}
