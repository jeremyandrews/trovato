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

use trovato_sdk::prelude::*;

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

/// Process AI field rules before an item is saved.
///
/// Reads field rule configuration from site config, evaluates which
/// rules apply to the current item type and changed fields, and
/// calls `ai_request()` to generate enriched content.
///
/// Rules are configured as:
/// ```json
/// {
///   "item_type": "conference",
///   "source_field": "field_description",
///   "target_field": "field_summary",
///   "trigger": "on_change",
///   "operation": "chat",
///   "prompt": "Summarize this conference description in 2 sentences: {field_description}",
///   "behavior": "fill_if_empty",
///   "weight": 0
/// }
/// ```
///
/// For now, field rules are configured via site config (database).
/// A visual admin UI for rule management is a future enhancement.
#[plugin_tap]
pub fn tap_item_presave(item: Item) -> String {
    // Field rules are read from site config at runtime.
    // In this initial implementation, we return the item unchanged.
    // When field rules are configured, this tap will:
    // 1. Load rules for this item_type from site config
    // 2. Check which rules' trigger conditions are met
    // 3. For each matching rule, build a prompt from the template
    // 4. Call ai_request() with the prompt
    // 5. Apply the result to the target field based on behavior

    // Return the item JSON unchanged (no rules configured yet)
    serde_json::to_string(&item).unwrap_or_default()
}

// ---- Form Assist (tap_form_alter) ----

/// Inject AI Assist buttons into content editing forms.
///
/// Adds "AI Assist" buttons next to text fields for users with the
/// `use ai chat` permission. Operations: rewrite, expand, shorten,
/// translate, adjust tone.
#[plugin_tap]
pub fn tap_form_alter(form_json: String) -> String {
    // In the initial implementation, we pass through the form unchanged.
    // When form assist is enabled, this tap will:
    // 1. Check if the current user has "use ai chat" permission
    // 2. For each text field in the form, inject an AI Assist button
    // 3. The button triggers a client-side popover with operation choices
    // 4. Selected operations call /api/v1/ai/assist with the field value

    // Return the form JSON unchanged
    form_json
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
}
