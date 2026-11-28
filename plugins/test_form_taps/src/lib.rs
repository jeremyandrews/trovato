//! B71 fixture: a plugin that alters, validates and hears the submission of
//! kernel forms, and answers one AJAX trigger from its own table.
//!
//! Before B71 no route called `FormService::build` or `process`, so none of
//! these taps ever ran for a content or profile form. This fixture touches only
//! forms for its own probe content type (`b71_alter_probe`) and the profile
//! form of a user holding its permission, so the rest of the suite never sees
//! it.
//!
//! B83b adds the kernel's account and comment forms and one plugin form, and
//! B83c two admin forms ([`B83B_FORMS`]): each is altered with a probe line, refused with a form
//! error while its id is listed in this fixture's `b83b_refuse_forms` table,
//! and its submissions are recorded in `b83b_form_submits`.
//!
//! Each tap takes and returns a `serde_json::Value`: `#[plugin_tap]`
//! deserializes the input as the parameter's type, so a `String` parameter
//! would expect a JSON string and refuse the object the kernel sends.

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

/// The kernel's account and comment forms this fixture alters and, on
/// request, refuses (B83b).
const B83B_FORMS: &[&str] = &[
    "user_login",
    "user_register",
    "user_password_change",
    "user_mail_resubscribe",
    "user_mail_unsubscribe",
    "user_delete_confirm",
    "user_delete_final",
    "comment_form",
    "comment_edit",
    "comment_delete",
    "user_password_reset_request",
    "user_password_reset",
    "user_recover",
    "user_api_token_create",
    // A plugin's own form, served through `ApiResponse::form` (B83b PR 3).
    "test_plugin_api_note",
    // Admin forms built and checked through `routes::admin_form` (B83c).
    "access_rule_form",
    "url_alias_form",
];

/// Whether `form_id` is one of [`B83B_FORMS`].
fn is_b83b_form(form_id: &str) -> bool {
    B83B_FORMS.contains(&form_id)
}

/// Whether a test asked for `form_id` to be refused: its row in
/// `b83b_refuse_forms`.
fn refuses(form_id: &str) -> bool {
    host::query_raw(
        "SELECT count(*) AS n FROM b83b_refuse_forms WHERE form_id = $1",
        &[serde_json::json!(form_id)],
    )
    .ok()
    .and_then(|rows| serde_json::from_str::<serde_json::Value>(&rows).ok())
    .and_then(|rows| rows[0]["n"].as_i64())
    .is_some_and(|n| n > 0)
}

/// Whether this fixture acts on the form `form_id`.
///
/// `json_item_{PROBE_TYPE}` (B326) is the synthetic id `FormService::validate_fields`
/// uses for the JSON item API, so this fixture's one `tap_form_validate` can be
/// proven to run on both paths.
fn is_probe_form(form_id: &str) -> bool {
    form_id == format!("item_add_{PROBE_TYPE}")
        || form_id == format!("item_edit_{PROBE_TYPE}")
        || form_id == format!("json_item_{PROBE_TYPE}")
        || (form_id == "user_profile_form" && host::current_user_has_permission(PERM_PROFILE))
}

/// Add a markup element saying this plugin altered the form.
#[plugin_tap]
pub fn tap_form_alter(mut form: serde_json::Value) -> serde_json::Value {
    let form_id = form["form_id"].as_str().unwrap_or_default().to_string();
    if is_b83b_form(&form_id) {
        if let Some(elements) = form["elements"].as_object_mut() {
            elements.insert(
                "b83b_probe".to_string(),
                serde_json::json!({
                    "type": "markup",
                    "value": format!("<p class=\"b83b-probe\">Altered: {form_id}</p>"),
                    "weight": 99,
                }),
            );
        }
        return form;
    }
    if !is_probe_form(&form_id) {
        return form;
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
    form
}

/// Refuse a probe item whose note says so, with an error on that field, and
/// a B83b form a test listed, with an error on the whole form.
#[plugin_tap]
pub fn tap_form_validate(input: serde_json::Value) -> serde_json::Value {
    let form_id = input["form_id"].as_str().unwrap_or_default();
    if is_b83b_form(form_id) {
        if refuses(form_id) {
            return serde_json::json!({
                "errors": [format!("test_form_taps refuses {form_id}.")]
            });
        }
        return serde_json::json!({});
    }
    if !is_probe_form(form_id) {
        return serde_json::json!({});
    }
    if input["values"]["field_note"].as_str() == Some("refuse me") {
        return serde_json::json!({
            "errors": [{"field": "field_note", "message": "The probe refuses this note."}]
        });
    }
    // B326: `tap_form_validate`'s input now carries `item_id`, so a plugin
    // validating a field it owns can read the item's current stored state.
    // This names what it saw, rather than just whether it saw one, so a
    // kernel test can tell the real id came through rather than a stray
    // truthy value.
    if input["values"]["field_note"].as_str() == Some("name the item id") {
        let message = match input["item_id"].as_str() {
            Some(id) => format!("item_id was {id}"),
            None => "no item_id".to_string(),
        };
        return serde_json::json!({
            "errors": [{"field": "field_note", "message": message}]
        });
    }
    serde_json::json!({})
}

/// Record that a probe form was submitted.
#[plugin_tap]
pub fn tap_form_submit(input: serde_json::Value) -> serde_json::Value {
    let form_id = input["form_id"].as_str().unwrap_or_default();
    if is_b83b_form(form_id) {
        if let Err(code) = host::execute_raw(
            "INSERT INTO b83b_form_submits (form_id) VALUES ($1)",
            &[serde_json::json!(form_id)],
        ) {
            host::log("error", "test_form_taps", &format!("insert failed: {code}"));
        }
        return serde_json::json!({});
    }
    if !is_probe_form(form_id) {
        return serde_json::json!({});
    }
    let item_id = input["item_id"].as_str().unwrap_or_default();
    if let Err(code) = host::execute_raw(
        "INSERT INTO b71_form_submits (form_id, item_id) VALUES ($1, $2)",
        &[serde_json::json!(form_id), serde_json::json!(item_id)],
    ) {
        host::log("error", "test_form_taps", &format!("insert failed: {code}"));
    }
    serde_json::json!({})
}

/// Answer the `b71_count` trigger with how many submissions the table holds.
///
/// `/system/ajax` used to dispatch with no services, so a handler could not
/// read anything; this one needs the database to answer at all.
#[plugin_tap]
pub fn tap_form_ajax(input: serde_json::Value) -> serde_json::Value {
    if input["trigger"] != "b71_count"
        || !is_probe_form(input["form_id"].as_str().unwrap_or_default())
    {
        return serde_json::json!({});
    }
    let count = host::query_raw("SELECT count(*) AS n FROM b71_form_submits", &[])
        .ok()
        .and_then(|rows| serde_json::from_str::<serde_json::Value>(&rows).ok())
        .and_then(|rows| rows[0]["n"].as_i64());
    let html = match count {
        Some(n) => format!("<span class=\"b71-count\">{n} submissions</span>"),
        None => "<span class=\"b71-count\">no database</span>".to_string(),
    };
    serde_json::json!({
        "commands": [{"command": "replace", "selector": "#b71-count", "html": html}]
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn validate_json_item_form_id_is_recognized_as_a_probe_form() {
        assert!(is_probe_form(&format!("json_item_{PROBE_TYPE}")));
    }

    #[test]
    fn validate_names_the_item_id_it_was_given() {
        let with_id = __inner_tap_form_validate(serde_json::json!({
            "form_id": format!("item_edit_{PROBE_TYPE}"),
            "values": {"field_note": "name the item id"},
            "item_id": "11111111-1111-1111-1111-111111111111",
        }));
        assert_eq!(
            with_id["errors"][0]["message"],
            "item_id was 11111111-1111-1111-1111-111111111111"
        );

        let without_id = __inner_tap_form_validate(serde_json::json!({
            "form_id": format!("item_add_{PROBE_TYPE}"),
            "values": {"field_note": "name the item id"},
            "item_id": null,
        }));
        assert_eq!(without_id["errors"][0]["message"], "no item_id");
    }

    #[test]
    fn validate_refuses_the_json_item_path_the_same_as_the_form() {
        let result = __inner_tap_form_validate(serde_json::json!({
            "form_id": format!("json_item_{PROBE_TYPE}"),
            "values": {"field_note": "refuse me"},
            "item_id": null,
        }));
        assert_eq!(
            result["errors"][0]["message"],
            "The probe refuses this note."
        );
    }
}
