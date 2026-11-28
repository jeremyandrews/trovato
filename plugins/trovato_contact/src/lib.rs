//! A contact form: a visitor reaches the site owner without an account.
//!
//! Drupal 6 shipped contact in core. Kernel minimality puts it in a plugin ("if
//! it is a feature, it is a plugin"), and until 0.101 a plugin could not build
//! one: it could not send mail, could not serve a form that worked without
//! JavaScript, and could not render a page into the site's theme. This plugin is
//! what those three surfaces were for, and it uses all three.
//!
//! # What it does
//!
//! - `GET /contact` describes its form in [`ApiResponse::form`]. The kernel
//!   builds it (token, `tap_form_alter`), renders it with the site's form
//!   templates into the site's page template, and checks it when it comes
//!   back. It has no JavaScript in it.
//! - `POST /contact` receives a submission the kernel has already checked:
//!   the token, the required fields, and this plugin's own rules in
//!   [`tap_form_validate`]. It sends the message to the site's configured
//!   contact address through the `mail` host interface and renders a themed
//!   confirmation. A submission that fails a check never reaches it: the
//!   kernel shows the form again with the errors and what the visitor typed.
//!
//! # What it deliberately does not do
//!
//! - **It stores nothing.** A contact message is delivered, not kept. Keeping it
//!   would mean a moderation queue, a retention policy and a data-export
//!   obligation, and a contact form should not have opinions about any of those.
//!   The plugin therefore owns no table and declares no `db` capability.
//! - **It cannot set `Reply-To`.** The `mail` interface takes a subject, a body
//!   and attachments, and no headers — headers are how a relay would be smuggled
//!   back in. The visitor's address goes in the body instead, where the owner can
//!   read it and reply by hand.
//! - **It does not tell the visitor whether the site can send mail.** A refusal
//!   from the host says why in the log; the visitor is told the message could not
//!   be sent and to try again, because "SMTP is not configured on this site" is
//!   the site owner's problem and not a stranger's business.

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::render::escape_html;
use trovato_sdk::types::{ApiRequest, ApiResponse, MailAttachment, MenuRoute};

/// Where the form lives. One path, two methods, so the form posts to the URL it
/// was served from.
const PATH: &str = "/contact";

/// The form's id: the kernel requires a plugin's form ids to start with the
/// plugin's name.
const FORM_ID: &str = "trovato_contact_message";

/// Longest name accepted.
const MAX_NAME: usize = 100;

/// Longest subject accepted. The kernel caps the mail subject at 512 bytes and
/// this leaves room for the prefix below.
const MAX_SUBJECT: usize = 200;

/// Longest message accepted. Generous, and bounded: an unbounded textarea is a
/// way to fill somebody's mailbox one request at a time.
const MAX_MESSAGE: usize = 10_000;

/// Prefix on the mail subject, so the owner can filter contact mail from the rest.
const SUBJECT_PREFIX: &str = "[contact]";

/// The permission both routes ask, with Drupal's name, so a site can take the
/// form away from some roles or all of them (jibnotes denied it to everyone).
///
/// Granted to the anonymous and authenticated roles when the plugin's
/// migrations run (on enable, and on upgrade for a site that already has it
/// enabled; `migrations/001_grant_contact_permission.sql`), so a site that does
/// nothing keeps a form anybody can use, as before.
const PERMISSION: &str = "access site-wide contact form";

/// Declare [`PERMISSION`], so it is listed on the permissions page.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        PERMISSION,
        "Use the site-wide contact form",
    )]
}

/// Register the two routes, both behind [`PERMISSION`]: a role without it gets
/// 403 (401 when not signed in, the kernel's answer for every plugin route).
/// `visible` is left on so the page appears in navigation like any other.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("GET", PATH, "show")
            .title("Contact")
            .permission(PERMISSION),
        MenuRoute::api("POST", PATH, "submit")
            .title("Contact")
            .permission(PERMISSION),
    ]
}

/// Serve one request.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "show" => show(),
        "submit" => submit(&request),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}

/// This plugin's own rules for its form, beyond the required fields the
/// kernel checks: lengths and the shape of the address.
///
/// Every problem at once rather than the first: a visitor who has typed a long
/// message should not discover the problems one round-trip at a time.
#[plugin_tap]
pub fn tap_form_validate(input: serde_json::Value) -> serde_json::Value {
    if input["form_id"].as_str() != Some(FORM_ID) {
        return serde_json::json!({});
    }
    let submission = Submission::from_values(input["values"].as_object());
    let errors: Vec<serde_json::Value> = submission
        .errors()
        .into_iter()
        .map(|(field, message)| serde_json::json!({"field": field, "message": message}))
        .collect();
    serde_json::json!({ "errors": errors })
}

/// What the visitor typed.
#[derive(Debug, Default, Clone)]
struct Submission {
    name: String,
    email: String,
    subject: String,
    message: String,
    attach: bool,
}

impl Submission {
    /// Read a submission out of the form's values, as the kernel passes them.
    fn from_values(values: Option<&serde_json::Map<String, serde_json::Value>>) -> Self {
        let text = |name: &str| {
            values
                .and_then(|v| v.get(name))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        Self {
            name: text("name"),
            email: text("email"),
            subject: text("subject"),
            message: text("message"),
            attach: text("attach") == "1",
        }
    }

    /// Everything wrong with it that the kernel does not already check, as
    /// `(field, message)`, in the order the fields appear. An empty required
    /// field is the kernel's to report, so it is not reported twice.
    fn errors(&self) -> Vec<(&'static str, String)> {
        let mut errors = Vec::new();
        if self.name.chars().count() > MAX_NAME {
            errors.push((
                "name",
                format!("Your name is longer than {MAX_NAME} characters."),
            ));
        }
        if !self.email.trim().is_empty() && !looks_like_email(self.email.trim()) {
            errors.push((
                "email",
                "That email address does not look right.".to_string(),
            ));
        }
        if self.subject.chars().count() > MAX_SUBJECT {
            errors.push((
                "subject",
                format!("Your subject is longer than {MAX_SUBJECT} characters."),
            ));
        }
        if self.message.chars().count() > MAX_MESSAGE {
            errors.push((
                "message",
                format!("Your message is longer than {MAX_MESSAGE} characters."),
            ));
        }
        errors
    }

    /// The mail subject: prefixed, single-line, and bounded.
    ///
    /// Control characters are stripped here rather than left to the kernel. The
    /// kernel refuses them, correctly — a newline in a header is header injection
    /// — but a visitor who pastes a subject with a line break has not done
    /// anything wrong, and should get their message delivered rather than an
    /// error they cannot act on.
    fn mail_subject(&self) -> String {
        let subject = single_line(self.subject.trim());
        if subject.is_empty() {
            format!("{SUBJECT_PREFIX} message from {}", single_line(&self.name))
        } else {
            format!("{SUBJECT_PREFIX} {subject}")
        }
    }

    /// The mail body. Carries the visitor's address, because the interface has no
    /// `Reply-To` to put it in.
    fn mail_body(&self) -> String {
        format!(
            "From: {name} <{email}>\n\n{message}\n",
            name = self.name.trim(),
            email = self.email.trim(),
            message = self.message.trim(),
        )
    }
}

/// Serve the form.
fn show() -> ApiResponse {
    ApiResponse::themed("Contact", "").with_form(form(&Submission::default()))
}

/// Send a submission the kernel has checked.
fn submit(request: &ApiRequest) -> ApiResponse {
    // A post that did not come from the form (no build id, so the kernel
    // checked only its token) gets the form.
    if request.form_values.is_none() {
        return show();
    }
    let submission = Submission::from_values(request.form_values.as_ref());

    let attachments = if submission.attach {
        vec![MailAttachment::text(
            "message.txt",
            submission.message.trim().to_string(),
        )]
    } else {
        Vec::new()
    };

    match host::mail_send_to_site_contacts(
        &submission.mail_subject(),
        &submission.mail_body(),
        &attachments,
    ) {
        Ok(()) => ApiResponse::themed("Message sent", sent_html(&submission)),
        Err(code) => {
            // The reason goes to the log, where the site owner can see it. The
            // visitor is told what happened and what to do, and not why.
            host::log(
                "error",
                "trovato_contact",
                &format!("failed to send a contact message: host error {code}"),
            );
            ApiResponse::themed_with_status(
                500,
                "Contact",
                "<p class=\"contact-error\">Your message could not be sent. \
                 Please try again later.</p>",
            )
            .with_form(form(&submission))
        }
    }
}

/// The form, holding `values`. The kernel escapes everything in it.
fn form(values: &Submission) -> serde_json::Value {
    serde_json::json!({
        "form_id": FORM_ID,
        "attributes": {"class": "contact-form"},
        "elements": {
            "name": {
                "type": "textfield", "max_length": MAX_NAME, "title": "Your name",
                "required": true, "default_value": values.name, "weight": 0
            },
            "email": {
                "type": "input", "input_type": "email", "title": "Your email",
                "required": true, "default_value": values.email, "weight": 1
            },
            "subject": {
                "type": "textfield", "max_length": MAX_SUBJECT, "title": "Subject",
                "default_value": values.subject, "weight": 2
            },
            "message": {
                "type": "textarea", "rows": 10, "title": "Message", "required": true,
                "default_value": values.message, "weight": 3,
                "attributes": {"maxlength": MAX_MESSAGE.to_string()}
            },
            "attach": {
                "type": "checkbox", "title": "Attach a copy of my message as a file",
                "default_value": values.attach, "weight": 4
            },
            "send": {"type": "submit", "value": "Send message", "weight": 10}
        }
    })
}

/// The confirmation.
fn sent_html(submission: &Submission) -> String {
    format!(
        "<p class=\"contact-sent\">Thank you, {name}. Your message has been sent.</p>",
        name = escape_html(submission.name.trim()),
    )
}

/// Collapse a string to one line, for a value that becomes a mail header.
fn single_line(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a string is shaped like an email address.
///
/// Deliberately loose: one `@`, something either side, a dot in the domain, no
/// whitespace. A stricter test rejects addresses that work, and the real check is
/// whether the owner can reply.
fn looks_like_email(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) || value.len() > 254 {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::json!(v)))
            .collect()
    }

    fn submit_request(pairs: &[(&str, &str)]) -> ApiRequest {
        let mut request = ApiRequest::new("submit", "POST", PATH, "", false);
        request.form_values = Some(values(pairs));
        request
    }

    fn validate(pairs: &[(&str, &str)]) -> serde_json::Value {
        __inner_tap_form_validate(serde_json::json!({
            "form_id": FORM_ID,
            "values": values(pairs),
        }))
    }

    #[test]
    fn it_declares_the_permission_its_routes_ask() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "access site-wide contact form");
    }

    #[test]
    fn the_menu_registers_one_path_for_two_methods() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert!(menus.iter().all(|m| m.path == PATH));
        assert!(menus.iter().all(|m| m.handler_type == "api"));
        assert!(
            menus.iter().all(|m| m.permission == PERMISSION),
            "both routes ask the one permission a site can take away"
        );
        let methods: Vec<&str> = menus.iter().map(|m| m.method.as_str()).collect();
        assert!(methods.contains(&"GET"), "{methods:?}");
        assert!(methods.contains(&"POST"), "{methods:?}");
    }

    #[test]
    fn the_page_is_themed_and_hands_the_kernel_its_form() {
        let response = __inner_tap_api(ApiRequest::new("show", "GET", PATH, "", false));

        assert_eq!(response.status, 200);
        assert!(response.theme, "a public page must be themed");
        assert_eq!(response.title, "Contact");
        let form = response.form.expect("the page carries its form");
        assert_eq!(form["form_id"], FORM_ID);
        for required in ["name", "email", "message"] {
            assert_eq!(form["elements"][required]["required"], true, "{required}");
        }
        assert_eq!(
            form["elements"]["subject"]["required"],
            serde_json::Value::Null
        );
        // The plugin writes no markup of its own for the form, and no token.
        assert!(!response.body.contains("<form"), "{}", response.body);
    }

    #[test]
    fn a_checked_submission_is_sent_and_confirmed() {
        let response = __inner_tap_api(submit_request(&[
            ("name", "Ada Lovelace"),
            ("email", "ada@example.test"),
            ("subject", "Hello"),
            ("message", "A message"),
        ]));

        // The native SDK stub accepts the send, so this asserts the plugin's own
        // path: the confirmation was rendered.
        assert_eq!(response.status, 200);
        assert!(response.theme);
        assert_eq!(response.title, "Message sent");
        assert!(response.form.is_none());
        assert!(
            response.body.contains("Thank you, Ada Lovelace"),
            "{}",
            response.body
        );
    }

    #[test]
    fn a_post_that_did_not_come_from_the_form_gets_the_form() {
        let mut request = ApiRequest::new("submit", "POST", PATH, "", false);
        request.body = "name=Ada&message=hi".to_string();

        let response = __inner_tap_api(request);

        assert_eq!(response.title, "Contact");
        assert!(
            response.form.is_some(),
            "nothing is sent without the kernel's checks"
        );
    }

    #[test]
    fn the_confirmation_escapes_the_name() {
        let response = __inner_tap_api(submit_request(&[
            ("name", "\"><script>alert(1)</script>"),
            ("email", "ada@example.test"),
            ("message", "hi"),
        ]));

        assert!(!response.body.contains("<script"), "{}", response.body);
        assert!(
            response.body.contains("&lt;script&gt;"),
            "{}",
            response.body
        );
    }

    #[test]
    fn validation_reports_each_problem_against_its_field() {
        let long = "x".repeat(MAX_MESSAGE + 1);
        let result = validate(&[
            ("name", "Ada"),
            ("email", "nope"),
            ("subject", ""),
            ("message", &long),
        ]);
        let errors = result["errors"].as_array().unwrap();
        let fields: Vec<&str> = errors
            .iter()
            .map(|e| e["field"].as_str().unwrap())
            .collect();
        assert_eq!(fields, ["email", "message"], "{result}");
        assert!(
            errors[1]["message"]
                .as_str()
                .unwrap()
                .contains("longer than")
        );
    }

    #[test]
    fn validation_leaves_empty_required_fields_to_the_kernel() {
        let result = validate(&[("name", ""), ("email", ""), ("message", "")]);
        assert_eq!(result["errors"], serde_json::json!([]), "{result}");
    }

    #[test]
    fn validation_ignores_every_other_form() {
        let result = __inner_tap_form_validate(serde_json::json!({
            "form_id": "user_login",
            "values": {"email": "nope"},
        }));
        assert_eq!(result, serde_json::json!({}));
    }

    #[test]
    fn a_subject_with_a_newline_is_collapsed_rather_than_refused() {
        let submission = Submission {
            name: "Ada".to_string(),
            email: "ada@example.test".to_string(),
            subject: "Hello\r\nBcc: victim@example.test".to_string(),
            message: "hi".to_string(),
            attach: false,
        };

        let subject = submission.mail_subject();

        assert!(!subject.contains('\r'), "{subject}");
        assert!(!subject.contains('\n'), "{subject}");
        assert_eq!(subject, "[contact] Hello Bcc: victim@example.test");
    }

    #[test]
    fn an_empty_subject_falls_back_to_naming_the_sender() {
        let submission = Submission {
            name: "Ada Lovelace".to_string(),
            email: "ada@example.test".to_string(),
            subject: "   ".to_string(),
            message: "hi".to_string(),
            attach: false,
        };

        assert_eq!(
            submission.mail_subject(),
            "[contact] message from Ada Lovelace"
        );
    }

    #[test]
    fn the_body_carries_the_senders_address_since_there_is_no_reply_to() {
        let submission = Submission {
            name: "Ada".to_string(),
            email: "ada@example.test".to_string(),
            subject: "Hi".to_string(),
            message: "the message".to_string(),
            attach: false,
        };

        let body = submission.mail_body();

        assert!(body.contains("Ada <ada@example.test>"), "{body}");
        assert!(body.contains("the message"), "{body}");
    }

    #[test]
    fn email_shapes_that_pass_and_shapes_that_do_not() {
        for good in [
            "ada@example.test",
            "ada.lovelace+contact@sub.example.co.uk",
            "a@b.co",
        ] {
            assert!(looks_like_email(good), "must accept {good}");
        }
        for bad in [
            "ada",
            "ada@",
            "@example.test",
            "ada@example",
            "ada@.example",
            "ada@example.",
            "ada@exa..mple.test",
            "ada lovelace@example.test",
        ] {
            assert!(!looks_like_email(bad), "must refuse {bad}");
        }
    }

    #[test]
    fn the_attachment_checkbox_is_off_unless_it_says_one() {
        assert!(!Submission::from_values(Some(&values(&[("attach", "0")]))).attach);
        assert!(!Submission::from_values(Some(&values(&[]))).attach);
        assert!(Submission::from_values(Some(&values(&[("attach", "1")]))).attach);
    }
}
