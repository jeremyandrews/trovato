//! K1 e2e fixture: a plugin that **serves HTTP requests** (G-NO-PLUGIN-HTTP).
//!
//! Before `KERNEL_API_VERSION (0,99)` no such plugin could exist: `tap_menu`'s
//! `callback` was dropped on deserialize, the form taps were never dispatched,
//! and `tap_form_ajax` was admin-only and service-less. This fixture is the
//! smallest thing that proves the surface is real — it registers two `api`
//! routes, gates them on a permission, and **writes a plugin-owned table** from
//! an authenticated reader's request, which is the thing that was impossible.
//!
//! Deliberately minimal: no business logic, one table, and one callback per
//! surface it exercises.

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::{ApiRequest, ApiResponse, MailAttachment, MenuRoute};

/// Permission required to write a note. Anonymous callers do not hold it.
const PERM_WRITE: &str = "write test notes";

/// Declare the permission the menu entries gate on.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        PERM_WRITE,
        "Write a note through the test plugin API",
    )]
}

/// Register the plugin-served routes.
///
/// `handler_type = "api"` plus a `callback` is what makes the kernel route a
/// request here. The `permission` field is the gate; an entry with an empty
/// permission is public, which the read route uses on purpose so the test can
/// prove the two cases differ.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("POST", "/tpa/note/:slug", "write_note")
            .title("Write a note")
            .permission(PERM_WRITE),
        // Public on purpose: the kernel must gate on what the entry says, not
        // on whether an entry exists.
        MenuRoute::api("GET", "/tpa/notes", "list_notes").title("List notes"),
        // A form that works with JavaScript switched off, which needs both
        // halves of the 0.101 surface: a token the plugin can render, and a
        // `_token` field the kernel will accept in place of a header. Public
        // and anonymous on purpose — that is the case a contact form is.
        MenuRoute::api("GET", "/tpa/form", "show_form").title("A form"),
        MenuRoute::api("POST", "/tpa/form", "submit_form").title("Submit the form"),
        // Sends to the site's contact address through the `mail` host interface.
        // Public, because the shape being exercised is a contact form: a visitor
        // with no account reaching the site owner.
        MenuRoute::api("POST", "/tpa/mail", "send_mail").title("Send mail"),
        // Asks to be rendered into the site's page template (0.101). Paired with
        // `/tpa/form`, which does not ask: the two together are what proves
        // theming is opt-in rather than the new default.
        MenuRoute::api("GET", "/tpa/themed", "show_themed").title("A themed page"),
        // A page entry, to prove the kernel routes only `api` entries here.
        MenuRoute::page("/tpa/page", "Not an API"),
    ]
}

/// Serve one request.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "write_note" => write_note(&request),
        "list_notes" => list_notes(&request),
        "show_form" => show_form(&request),
        "submit_form" => submit_form(&request),
        "send_mail" => send_mail(&request),
        "show_themed" => show_themed(),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}

/// Serve an HTML form carrying the kernel-minted token in a hidden `_token`.
///
/// No JavaScript, no header: a `<form method="post">` cannot set one, which is
/// the reason [`ApiRequest::csrf_token`] exists.
fn show_form(request: &ApiRequest) -> ApiResponse {
    let body = format!(
        r#"<!DOCTYPE html>
<html><body>
<form method="post" action="/tpa/form">
<input type="hidden" name="_token" value="{token}">
<textarea name="message"></textarea>
<button type="submit">Send</button>
</form>
</body></html>"#,
        token = escape_html(&request.csrf_token),
    );
    ApiResponse::with_status(200, body).content_type("text/html; charset=utf-8")
}

/// Accept the form, having been let through by the kernel's CSRF check.
///
/// Reaching this function at all is the assertion: a state-changing
/// plugin-served request with no `X-CSRF-Token` header is refused with 403
/// unless the `_token` field verified.
fn submit_form(request: &ApiRequest) -> ApiResponse {
    let message = form_field(&request.body, "message").unwrap_or_default();
    let body = format!(
        r#"<!DOCTYPE html>
<html><body><p>received: {message}</p></body></html>"#,
        message = escape_html(&message),
    );
    ApiResponse::with_status(200, body).content_type("text/html; charset=utf-8")
}

/// A page that asks to be wrapped in the site's theme.
///
/// The body is page *content*: no `<html>`, no `<head>`, nothing the theme
/// already provides. What comes back should carry the site's header and
/// navigation around this paragraph.
fn show_themed() -> ApiResponse {
    ApiResponse::themed(
        "A Themed Plugin Page",
        "<p id=\"plugin-content\">content from the plugin</p>",
    )
}

/// Send the submitted message to the site's contact address.
///
/// The plugin names no recipient: `mail_send_to_site_contacts` sends to the
/// address the site configured, which is what stops this being a relay. On
/// refusal the host error code is reported rather than swallowed, so the test can
/// tell "not configured" from "sent".
fn send_mail(request: &ApiRequest) -> ApiResponse {
    let subject = form_field(&request.body, "subject").unwrap_or_default();
    let body = form_field(&request.body, "body").unwrap_or_default();
    let attachments = if form_field(&request.body, "attach").as_deref() == Some("1") {
        vec![MailAttachment::text("message.txt", body.clone())]
    } else {
        Vec::new()
    };

    match host::mail_send_to_site_contacts(&subject, &body, &attachments) {
        Ok(()) => ApiResponse::with_status(200, r#"{"sent":true}"#),
        Err(code) => ApiResponse::json(&serde_json::json!({"sent": false, "code": code}))
            .unwrap_or_else(|_| ApiResponse::error(500, "serialize failed")),
    }
}

/// Read one field out of a URL-encoded body.
///
/// Hand-rolled because a plugin is a `no_std`-adjacent wasm crate with a
/// deliberately thin dependency list, and this fixture needs one field.
fn form_field(body: &str, field: &str) -> Option<String> {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == field)
        .map(|(_, value)| percent_decode(&value.replace('+', " ")))
}

/// Percent-decode a form value, leaving an invalid escape as written.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escape text for an HTML body or a double-quoted attribute.
///
/// The kernel does not sanitize a plugin's response body, which is the contract
/// every view tap has, so the plugin does it.
fn escape_html(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Write a note owned by the authenticated caller — the write that had no
/// surface before this existed.
fn write_note(request: &ApiRequest) -> ApiResponse {
    if !request.authenticated {
        // Belt and braces: the kernel's permission gate already refused an
        // anonymous caller, and a plugin should still not trust that.
        return ApiResponse::error(401, "authentication required");
    }

    let Some(slug) = request.params.get("slug") else {
        return ApiResponse::error(400, "missing slug");
    };

    let body: serde_json::Value = match request.json() {
        Ok(v) => v,
        Err(e) => return ApiResponse::error(400, &format!("body is not JSON: {e}")),
    };
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // Idempotent on (user, slug), so a re-delivered request updates rather than
    // duplicating.
    let written = host::execute_raw(
        "INSERT INTO tpa_notes (user_id, slug, text, method) VALUES ($1::uuid, $2, $3, $4) \
         ON CONFLICT (user_id, slug) DO UPDATE SET text = EXCLUDED.text",
        &[
            serde_json::json!(request.user_id),
            serde_json::json!(slug),
            serde_json::json!(text),
            serde_json::json!(request.method),
        ],
    );

    match written {
        Ok(_) => ApiResponse::json(&serde_json::json!({
            "written": true,
            "slug": slug,
            "user_id": request.user_id,
            // Echoed so the test can prove the request context crossed intact.
            "query": request.query,
        }))
        .unwrap_or_else(|_| ApiResponse::error(500, "serialize failed")),
        Err(code) => ApiResponse::error(500, &format!("write failed with code {code}")),
    }
}

/// Read the caller's notes back.
fn list_notes(request: &ApiRequest) -> ApiResponse {
    let rows = host::query_raw(
        "SELECT slug, text, method FROM tpa_notes WHERE user_id = $1::uuid ORDER BY slug",
        &[serde_json::json!(request.user_id)],
    );

    match rows {
        Ok(json) => ApiResponse::with_status(200, json),
        Err(code) => ApiResponse::error(500, &format!("read failed with code {code}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_declares_six_api_routes_with_callbacks() {
        let menus = __inner_tap_menu();
        let api: Vec<&MenuRoute> = menus.iter().filter(|m| m.handler_type == "api").collect();
        assert_eq!(api.len(), 6);
        assert!(api.iter().all(|m| !m.callback.is_empty()));
        assert_eq!(
            api.iter()
                .find(|m| m.callback == "write_note")
                .unwrap()
                .permission,
            PERM_WRITE
        );
    }

    #[test]
    fn an_unknown_callback_is_a_404_not_a_trap() {
        let request = ApiRequest::new("nope", "GET", "/tpa/x", "", false);
        assert_eq!(__inner_tap_api(request).status, 404);
    }

    #[test]
    fn an_anonymous_write_is_refused_by_the_plugin_too() {
        let request = ApiRequest::new("write_note", "POST", "/tpa/note/x", "", false);
        assert_eq!(__inner_tap_api(request).status, 401);
    }

    #[test]
    fn a_write_with_no_slug_is_a_400() {
        let request = ApiRequest::new(
            "write_note",
            "POST",
            "/tpa/note/",
            "00000000-0000-0000-0000-000000000001",
            true,
        );
        assert_eq!(__inner_tap_api(request).status, 400);
    }

    #[test]
    fn the_form_carries_the_kernel_minted_token_in_a_token_field() {
        let mut request = ApiRequest::new("show_form", "GET", "/tpa/form", "", false);
        request.csrf_token = "deadbeef".to_string();

        let response = __inner_tap_api(request);

        assert_eq!(response.status, 200);
        assert!(response.content_type.starts_with("text/html"));
        assert!(
            response
                .body
                .contains(r#"<input type="hidden" name="_token" value="deadbeef">"#),
            "the form must carry the token the kernel minted: {}",
            response.body
        );
    }

    #[test]
    fn a_token_carrying_markup_is_escaped_into_the_attribute() {
        let mut request = ApiRequest::new("show_form", "GET", "/tpa/form", "", false);
        request.csrf_token = r#"a"><script>"#.to_string();

        let body = __inner_tap_api(request).body;

        assert!(!body.contains("<script>"), "unescaped markup: {body}");
        assert!(body.contains("&quot;&gt;&lt;script&gt;"), "{body}");
    }

    #[test]
    fn the_submission_reads_its_field_out_of_a_form_encoded_body() {
        let mut request = ApiRequest::new("submit_form", "POST", "/tpa/form", "", false);
        request.body = "_token=abc&message=hello+there%21".to_string();

        let response = __inner_tap_api(request);

        assert_eq!(response.status, 200);
        assert!(
            response.body.contains("received: hello there!"),
            "the body must decode `+` and `%xx`: {}",
            response.body
        );
    }

    #[test]
    fn a_submitted_message_carrying_markup_is_escaped() {
        let mut request = ApiRequest::new("submit_form", "POST", "/tpa/form", "", false);
        request.body = "message=%3Cimg+onerror%3Dx%3E".to_string();

        let body = __inner_tap_api(request).body;

        assert!(!body.contains("<img"), "unescaped markup: {body}");
        assert!(body.contains("&lt;img onerror=x&gt;"), "{body}");
    }

    #[test]
    fn the_themed_page_asks_for_the_theme_and_names_itself() {
        let response = __inner_tap_api(ApiRequest::new(
            "show_themed",
            "GET",
            "/tpa/themed",
            "",
            false,
        ));

        assert_eq!(response.status, 200);
        assert!(response.theme, "the page must ask for the site theme");
        assert_eq!(response.title, "A Themed Plugin Page");
        assert!(response.body.contains("plugin-content"));
        // Page content, not a document: the theme supplies the rest.
        assert!(!response.body.contains("<html"), "{}", response.body);
    }

    #[test]
    fn a_missing_field_is_none_rather_than_a_panic() {
        assert_eq!(form_field("message=hi", "_token"), None);
        assert_eq!(form_field("", "message"), None);
        // A pair with no `=` is skipped rather than read as an empty value.
        assert_eq!(form_field("message", "message"), None);
    }
}
