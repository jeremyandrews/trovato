//! Cloudflare Turnstile CAPTCHA validation plugin for Trovato.
//!
//! Validates `cf-turnstile-response` tokens on form submissions by calling
//! the Turnstile siteverify API. The secret key is read from the site
//! variable `turnstile_secret_key`.

use serde::Deserialize;
use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// Turnstile siteverify API endpoint.
const TURNSTILE_VERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

/// Response from the Turnstile siteverify API.
#[derive(Debug, Deserialize)]
struct TurnstileResponse {
    success: bool,
    #[serde(default)]
    #[serde(rename = "error-codes")]
    error_codes: Vec<String>,
}

/// Register the CAPTCHA admin permission.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "administer captcha",
        "Configure CAPTCHA settings and bypass CAPTCHA on forms",
    )]
}

/// Validate the Turnstile CAPTCHA token on form submission.
///
/// Reads the `cf-turnstile-response` field from form values and verifies
/// it against Cloudflare's siteverify endpoint. Returns a JSON object with
/// an `errors` array (empty on success, populated on failure).
#[plugin_tap]
pub fn tap_form_validate(input_json: String) -> String {
    let input: serde_json::Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(_) => return r#"{"errors":[]}"#.to_string(),
    };

    let Some(values) = input.get("values") else {
        return r#"{"errors":[]}"#.to_string();
    };

    // Extract the Turnstile token from form values
    let token = match values.get("cf-turnstile-response").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => {
            // No token present — skip validation (form may not have CAPTCHA enabled)
            return r#"{"errors":[]}"#.to_string();
        }
    };

    // Read the secret key from site variables
    let secret_key = match host::variables_get("turnstile_secret_key", "") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            host::log(
                "warn",
                "trovato_captcha",
                "turnstile_secret_key not configured, skipping validation",
            );
            return r#"{"errors":[]}"#.to_string();
        }
    };

    // Build the verification request
    let body = format!("secret={secret_key}&response={token}");
    let request = HttpRequest::post(TURNSTILE_VERIFY_URL, body)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .timeout(10_000);

    match host::http_request(&request) {
        Ok(response) => interpret_siteverify(Some(&response.body)),
        Err(code) => {
            host::log(
                "error",
                "trovato_captcha",
                &format!("Turnstile HTTP request failed with code {code}"),
            );
            interpret_siteverify(None)
        }
    }
}

/// Interpret a Turnstile siteverify outcome into the tap's `{"errors":[…]}` JSON.
///
/// Extracted from [`tap_form_validate`] so the accept/**reject** decision — the
/// actual anti-spam behavior — is unit-testable without a live Cloudflare call
/// (closes test-quality gap TQ-F3, the "injectable-verifier seam"). `body` is
/// the siteverify response body, or `None` when the HTTP request itself failed.
fn interpret_siteverify(body: Option<&str>) -> String {
    let Some(body) = body else {
        return r#"{"errors":["CAPTCHA service unavailable. Please try again later."]}"#
            .to_string();
    };
    match serde_json::from_str::<TurnstileResponse>(body) {
        Ok(result) if result.success => r#"{"errors":[]}"#.to_string(),
        Ok(result) => {
            host::log(
                "warn",
                "trovato_captcha",
                &format!("Turnstile verification failed: {:?}", result.error_codes),
            );
            r#"{"errors":["CAPTCHA verification failed. Please try again."]}"#.to_string()
        }
        Err(_) => {
            host::log(
                "error",
                "trovato_captcha",
                &format!("Failed to parse Turnstile response: {body}"),
            );
            r#"{"errors":["CAPTCHA verification error. Please try again."]}"#.to_string()
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_captcha_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "administer captcha");
    }

    #[test]
    fn validate_skips_when_no_token() {
        let input = serde_json::json!({
            "form_id": "contact_form",
            "values": {"name": "Alice"}
        });
        let result = __inner_tap_form_validate(serde_json::to_string(&input).unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn validate_skips_when_no_values() {
        let input = serde_json::json!({"form_id": "contact_form"});
        let result = __inner_tap_form_validate(serde_json::to_string(&input).unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn validate_skips_empty_token() {
        let input = serde_json::json!({
            "form_id": "contact_form",
            "values": {"cf-turnstile-response": ""}
        });
        let result = __inner_tap_form_validate(serde_json::to_string(&input).unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn validate_with_token_but_no_secret_key() {
        // Stub variables_get returns the default (""), so secret key is empty
        let input = serde_json::json!({
            "form_id": "contact_form",
            "values": {"cf-turnstile-response": "some-token"}
        });
        let result = __inner_tap_form_validate(serde_json::to_string(&input).unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        // No secret key configured, so validation is skipped
        assert!(parsed["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn validate_handles_invalid_json() {
        let result = __inner_tap_form_validate("not json".to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["errors"].as_array().unwrap().is_empty());
    }

    // TQ-F3: the CAPTCHA *reject* decision (previously untested — all prior
    // tests exercised only the skip path). `interpret_siteverify` is the seam.

    #[test]
    fn interpret_accepts_successful_verification() {
        let result = interpret_siteverify(Some(r#"{"success":true}"#));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn interpret_rejects_unsuccessful_verification() {
        let body = r#"{"success":false,"error-codes":["invalid-input-response"]}"#;
        let result = interpret_siteverify(Some(body));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let errors = parsed["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1, "an invalid token must be rejected");
        assert!(
            errors[0].as_str().unwrap().contains("verification failed"),
            "reject message expected, got {errors:?}"
        );
    }

    #[test]
    fn interpret_rejects_unparseable_response() {
        let result = interpret_siteverify(Some("<html>gateway error</html>"));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let errors = parsed["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].as_str().unwrap().contains("verification error"));
    }

    #[test]
    fn interpret_reports_service_unavailable_on_request_failure() {
        let result = interpret_siteverify(None);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(
            !parsed["errors"].as_array().unwrap().is_empty(),
            "a failed siteverify request must not silently pass"
        );
    }
}
