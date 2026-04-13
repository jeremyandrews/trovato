//! HubSpot CRM integration plugin for Trovato.
//!
//! On contact form submission, sends form data to HubSpot's Contacts API
//! to create or update a CRM contact. The HubSpot access token is read
//! from the site variable `hubspot_access_token`.

use std::collections::HashMap;

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// HubSpot Contacts API endpoint.
const HUBSPOT_CONTACTS_URL: &str = "https://api.hubapi.com/crm/v3/objects/contacts";

/// Form fields that map to HubSpot contact properties.
const HUBSPOT_FIELD_MAP: &[(&str, &str)] = &[
    ("firstname", "firstname"),
    ("lastname", "lastname"),
    ("email", "email"),
    ("company", "company"),
    ("phone", "phone"),
    ("message", "message"),
];

/// Register the contact form submission permission.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "submit contact form",
        "Submit contact forms that sync data to HubSpot CRM",
    )]
}

/// On contact form submission, send data to HubSpot CRM.
///
/// Reads form values, maps known fields to HubSpot contact properties,
/// and sends a POST request to create a contact. The form must have
/// `form_id` starting with `"contact"` to trigger this tap.
#[plugin_tap]
pub fn tap_form_submit(input_json: String) -> String {
    let input: serde_json::Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(_) => return r#"{"status":"skipped"}"#.to_string(),
    };

    // Only process contact forms
    let form_id = input.get("form_id").and_then(|v| v.as_str()).unwrap_or("");
    if !form_id.starts_with("contact") {
        return r#"{"status":"skipped","reason":"not a contact form"}"#.to_string();
    }

    let Some(values) = input.get("values") else {
        return r#"{"status":"skipped","reason":"no form values"}"#.to_string();
    };

    // Read the HubSpot access token
    let access_token = match host::variables_get("hubspot_access_token", "") {
        Ok(token) if !token.is_empty() => token,
        _ => {
            host::log(
                "warn",
                "tag1_hubspot",
                "hubspot_access_token not configured, skipping CRM sync",
            );
            return r#"{"status":"skipped","reason":"no access token"}"#.to_string();
        }
    };

    // Build HubSpot contact properties from form values
    let mut properties = HashMap::new();
    for (form_field, hubspot_field) in HUBSPOT_FIELD_MAP {
        if let Some(value) = values.get(*form_field).and_then(|v| v.as_str())
            && !value.is_empty()
        {
            properties.insert(*hubspot_field, value);
        }
    }

    if properties.is_empty() {
        host::log(
            "debug",
            "tag1_hubspot",
            "No mappable fields found in form submission",
        );
        return r#"{"status":"skipped","reason":"no mappable fields"}"#.to_string();
    }

    // Build the HubSpot API request payload
    let payload = serde_json::json!({
        "properties": properties
    });

    let Ok(body) = serde_json::to_string(&payload) else {
        host::log(
            "error",
            "tag1_hubspot",
            "Failed to serialize HubSpot payload",
        );
        return r#"{"status":"error","reason":"serialization failed"}"#.to_string();
    };

    let request = HttpRequest::post(HUBSPOT_CONTACTS_URL, body)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {access_token}"))
        .timeout(15_000);

    match host::http_request(&request) {
        Ok(response) => {
            if response.status >= 200 && response.status < 300 {
                host::log(
                    "info",
                    "tag1_hubspot",
                    &format!(
                        "Contact created/updated in HubSpot for form {form_id} (HTTP {})",
                        response.status
                    ),
                );
                r#"{"status":"ok"}"#.to_string()
            } else {
                host::log(
                    "error",
                    "tag1_hubspot",
                    &format!(
                        "HubSpot API error (HTTP {}): {}",
                        response.status, response.body
                    ),
                );
                format!(
                    r#"{{"status":"error","reason":"HubSpot API returned {}"}}"#,
                    response.status
                )
            }
        }
        Err(code) => {
            host::log(
                "error",
                "tag1_hubspot",
                &format!("HubSpot HTTP request failed with code {code}"),
            );
            format!(r#"{{"status":"error","reason":"HTTP request failed ({code})"}}"#)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_contact_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "submit contact form");
    }

    #[test]
    fn submit_skips_non_contact_form() {
        let input = serde_json::json!({
            "form_id": "login_form",
            "values": {"email": "test@example.com"}
        });
        let result = __inner_tap_form_submit(serde_json::to_string(&input).unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "skipped");
        assert_eq!(parsed["reason"], "not a contact form");
    }

    #[test]
    fn submit_skips_without_values() {
        let input = serde_json::json!({"form_id": "contact_form"});
        let result = __inner_tap_form_submit(serde_json::to_string(&input).unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "skipped");
    }

    #[test]
    fn submit_skips_without_access_token() {
        // Stub variables_get returns default (""), so token is empty
        let input = serde_json::json!({
            "form_id": "contact_form",
            "values": {
                "firstname": "Alice",
                "lastname": "Smith",
                "email": "alice@example.com"
            }
        });
        let result = __inner_tap_form_submit(serde_json::to_string(&input).unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "skipped");
        assert_eq!(parsed["reason"], "no access token");
    }

    #[test]
    fn submit_handles_invalid_json() {
        let result = __inner_tap_form_submit("not json".to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "skipped");
    }

    #[test]
    fn field_map_covers_expected_fields() {
        let fields: Vec<&str> = HUBSPOT_FIELD_MAP.iter().map(|(f, _)| *f).collect();
        assert!(fields.contains(&"firstname"));
        assert!(fields.contains(&"lastname"));
        assert!(fields.contains(&"email"));
        assert!(fields.contains(&"company"));
        assert!(fields.contains(&"phone"));
        assert!(fields.contains(&"message"));
    }
}
