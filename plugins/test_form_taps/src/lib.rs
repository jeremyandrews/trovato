//! B71 fixture: a plugin that alters, validates and hears the submission of
//! kernel forms.
//!
//! Before B71 no route called `FormService::build` or `process`, so none of
//! these taps ever ran for a content or profile form. This fixture touches only
//! forms for its own probe content type (`b71_alter_probe`) and the profile
//! form of a user holding its permission, so the rest of the suite never sees
//! it.

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// The content type whose forms this fixture alters.
const PROBE_TYPE: &str = "b71_alter_probe";

/// Held by a user whose profile form this fixture alters.
const PERM_PROFILE: &str = "b71 probe profile";

/// Declare the permission the profile alteration gates on.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        PERM_PROFILE,
        "Have the B71 fixture alter your profile form",
    )]
}

/// Whether this fixture acts on the form `form_id`.
fn is_probe_form(form_id: &str) -> bool {
    form_id == format!("item_add_{PROBE_TYPE}")
        || form_id == format!("item_edit_{PROBE_TYPE}")
        || (form_id == "user_profile_form" && host::current_user_has_permission(PERM_PROFILE))
}

/// Add a markup element saying this plugin altered the form.
#[plugin_tap]
pub fn tap_form_alter(form_json: String) -> String {
    let Ok(mut form) = serde_json::from_str::<serde_json::Value>(&form_json) else {
        return form_json;
    };
    let form_id = form["form_id"].as_str().unwrap_or_default().to_string();
    if !is_probe_form(&form_id) {
        return form_json;
    }
    if let Some(elements) = form["elements"].as_object_mut() {
        elements.insert(
            "b71_probe_a".to_string(),
            serde_json::json!({
                "type": "markup",
                "value": "<p class=\"b71-probe\">Altered by test_form_taps</p>",
                "weight": 990,
            }),
        );
    }
    serde_json::to_string(&form).unwrap_or(form_json)
}

/// Refuse a probe item whose note says so, with an error on that field.
#[plugin_tap]
pub fn tap_form_validate(input_json: String) -> String {
    let input: serde_json::Value = serde_json::from_str(&input_json).unwrap_or_default();
    if !is_probe_form(input["form_id"].as_str().unwrap_or_default()) {
        return "{}".to_string();
    }
    if input["values"]["field_note"].as_str() == Some("refuse me") {
        return serde_json::json!({
            "errors": [{"field": "field_note", "message": "The probe refuses this note."}]
        })
        .to_string();
    }
    "{}".to_string()
}

/// Record that a probe form was submitted.
#[plugin_tap]
pub fn tap_form_submit(input_json: String) -> String {
    let input: serde_json::Value = serde_json::from_str(&input_json).unwrap_or_default();
    let form_id = input["form_id"].as_str().unwrap_or_default();
    if !is_probe_form(form_id) {
        return "{}".to_string();
    }
    let item_id = input["item_id"].as_str().unwrap_or_default();
    if let Err(code) = host::execute_raw(
        "INSERT INTO b71_form_submits (form_id, item_id) VALUES ($1, $2)",
        &[serde_json::json!(form_id), serde_json::json!(item_id)],
    ) {
        host::log("error", "test_form_taps", &format!("insert failed: {code}"));
    }
    "{}".to_string()
}
