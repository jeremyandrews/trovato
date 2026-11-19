//! B71 fixture: a second `tap_form_alter`, run after `test_form_taps`.
//!
//! `FormService::build` used to hand every plugin the same original form and
//! keep the last answer, so two plugins altering one form did not compose.
//! This one adds its element only when it can see the one `test_form_taps`
//! added, which it can only do if it received that plugin's output.

use trovato_sdk::prelude::*;

/// Add an element naming what this plugin saw of the first alteration.
#[plugin_tap]
pub fn tap_form_alter(form_json: String) -> String {
    let Ok(mut form) = serde_json::from_str::<serde_json::Value>(&form_json) else {
        return form_json;
    };
    let Some(elements) = form["elements"].as_object_mut() else {
        return form_json;
    };
    if !elements.contains_key("b71_probe_a") {
        return form_json;
    }
    elements.insert(
        "b71_probe_b".to_string(),
        serde_json::json!({
            "type": "markup",
            "value": "<p class=\"b71-probe\">test_form_taps_b saw test_form_taps</p>",
            "weight": 991,
        }),
    );
    serde_json::to_string(&form).unwrap_or(form_json)
}
