//! B71 fixture: a second `tap_form_alter`, run after `test_form_taps`.
//!
//! `FormService::build` used to hand every plugin the same original form and
//! keep the last answer, so two plugins altering one form did not compose.
//! This one adds its element only when it can see the one `test_form_taps`
//! added, which it can only do if it received that plugin's output.

use trovato_sdk::prelude::*;

/// Add an element naming what this plugin saw of the first alteration.
#[plugin_tap]
///
/// The form arrives and leaves as a JSON object: `#[plugin_tap]` deserializes
/// the input as the parameter's type, so a `String` parameter would expect a
/// JSON string and refuse the form.
pub fn tap_form_alter(mut form: serde_json::Value) -> serde_json::Value {
    let Some(elements) = form["elements"].as_object_mut() else {
        return form;
    };
    if !elements.contains_key("b71_probe_a") {
        return form;
    }
    elements.insert(
        "b71_probe_b".to_string(),
        serde_json::json!({
            "type": "markup",
            "value": "<p class=\"b71-probe\">test_form_taps_b saw test_form_taps</p>",
            "weight": 991,
        }),
    );
    form
}
