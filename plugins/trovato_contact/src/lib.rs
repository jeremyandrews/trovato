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
//! - `GET /contact` serves a form with no JavaScript in it, carrying the
//!   kernel-minted token from [`ApiRequest::csrf_token`] in a hidden `_token`
//!   input, rendered into the site's page template.
//! - `POST /contact` validates, sends the message to the site's configured
//!   contact address through the `mail` host interface, and renders a themed
//!   confirmation. On invalid input it re-renders the form with the errors, the
//!   values the visitor typed, and a **fresh** token, because a token is
//!   single-use and the one that arrived has been spent.
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
use trovato_sdk::types::{ApiRequest, ApiResponse, MailAttachment, MenuRoute};

/// Where the form lives. One path, two methods, so the form posts to the URL it
/// was served from — which is what a plain `<form method="post">` with no
/// `action` would do anyway.
const PATH: &str = "/contact";

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

/// Register the two routes.
///
/// Both are public: `permission` is empty, which the kernel reads as "anybody",
/// and a contact form that needs an account is not a contact form. `visible` is
/// left on so the page appears in navigation like any other.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("GET", PATH, "show").title("Contact"),
        MenuRoute::api("POST", PATH, "submit").title("Contact"),
    ]
}

/// Serve one request.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "show" => show(&request),
        "submit" => submit(&request),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
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
    /// Read a submission out of a form-urlencoded body.
    fn from_body(body: &str) -> Self {
        Self {
            name: field(body, "name"),
            email: field(body, "email"),
            subject: field(body, "subject"),
            message: field(body, "message"),
            attach: field(body, "attach") == "1",
        }
    }

    /// Everything wrong with it, in the order the fields appear.
    ///
    /// All of them at once rather than the first: a visitor who has typed a long
    /// message should not discover the problems one round-trip at a time.
    fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.name.trim().is_empty() {
            errors.push("Please give your name.".to_string());
        } else if self.name.chars().count() > MAX_NAME {
            errors.push(format!("Your name is longer than {MAX_NAME} characters."));
        }

        if self.email.trim().is_empty() {
            errors.push("Please give your email address.".to_string());
        } else if !looks_like_email(self.email.trim()) {
            errors.push("That email address does not look right.".to_string());
        }

        if self.subject.chars().count() > MAX_SUBJECT {
            errors.push(format!(
                "Your subject is longer than {MAX_SUBJECT} characters."
            ));
        }

        if self.message.trim().is_empty() {
            errors.push("Please write a message.".to_string());
        } else if self.message.chars().count() > MAX_MESSAGE {
            errors.push(format!(
                "Your message is longer than {MAX_MESSAGE} characters."
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
fn show(request: &ApiRequest) -> ApiResponse {
    ApiResponse::themed(
        "Contact",
        form_html(&request.csrf_token, &Submission::default(), &[]),
    )
}

/// Validate and send.
fn submit(request: &ApiRequest) -> ApiResponse {
    let submission = Submission::from_body(&request.body);
    let errors = submission.errors();
    if !errors.is_empty() {
        // 422 rather than 200: the request was understood and not acted on. The
        // token is the fresh one the kernel minted for *this* request, because
        // the one the visitor submitted has been spent.
        return ApiResponse::themed_with_status(
            422,
            "Contact",
            form_html(&request.csrf_token, &submission, &errors),
        );
    }

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
                format!(
                    "{}{}",
                    "<p class=\"contact-error\">Your message could not be sent. \
                     Please try again later.</p>",
                    form_html(&request.csrf_token, &submission, &[]),
                ),
            )
        }
    }
}

/// The form, with the values the visitor typed and any errors above it.
///
/// Every interpolation is escaped: this is a public form, so everything in it
/// arrived from a stranger, and the kernel does not sanitize a plugin's HTML.
fn form_html(token: &str, values: &Submission, errors: &[String]) -> String {
    let mut html = String::new();

    if !errors.is_empty() {
        html.push_str("<div class=\"contact-errors\"><ul>");
        for error in errors {
            html.push_str(&format!("<li>{}</li>", escape_html(error)));
        }
        html.push_str("</ul></div>");
    }

    html.push_str(&format!(
        r#"<form method="post" action="{path}" class="contact-form">
<input type="hidden" name="_token" value="{token}">
<p><label for="contact-name">Your name</label>
<input type="text" id="contact-name" name="name" value="{name}" maxlength="{max_name}" required></p>
<p><label for="contact-email">Your email</label>
<input type="email" id="contact-email" name="email" value="{email}" required></p>
<p><label for="contact-subject">Subject</label>
<input type="text" id="contact-subject" name="subject" value="{subject}" maxlength="{max_subject}"></p>
<p><label for="contact-message">Message</label>
<textarea id="contact-message" name="message" rows="10" maxlength="{max_message}" required>{message}</textarea></p>
<p><label><input type="checkbox" name="attach" value="1"{attach_checked}> Attach a copy of my message as a file</label></p>
<p><button type="submit">Send message</button></p>
</form>"#,
        path = escape_html(PATH),
        token = escape_html(token),
        name = escape_html(&values.name),
        email = escape_html(&values.email),
        subject = escape_html(&values.subject),
        message = escape_html(&values.message),
        max_name = MAX_NAME,
        max_subject = MAX_SUBJECT,
        max_message = MAX_MESSAGE,
        attach_checked = if values.attach { " checked" } else { "" },
    ));

    html
}

/// The confirmation.
fn sent_html(submission: &Submission) -> String {
    format!(
        "<p class=\"contact-sent\">Thank you, {name}. Your message has been sent.</p>",
        name = escape_html(submission.name.trim()),
    )
}

/// Read one field out of a URL-encoded body, first occurrence winning.
fn field(body: &str, name: &str) -> String {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| percent_decode(&value.replace('+', " ")))
        .unwrap_or_default()
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

/// Escape text for an HTML body or a double-quoted attribute.
///
/// The kernel does not sanitize a plugin's response body, which is the contract
/// every view tap has, so the plugin does it. Both quote forms are covered
/// because these values land in attributes as well as in text.
fn escape_html(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn valid_body() -> String {
        "name=Ada+Lovelace&email=ada%40example.test&subject=Hello&message=A+message".to_string()
    }

    #[test]
    fn the_menu_registers_one_path_for_two_methods() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert!(menus.iter().all(|m| m.path == PATH));
        assert!(menus.iter().all(|m| m.handler_type == "api"));
        assert!(
            menus.iter().all(|m| m.permission.is_empty()),
            "a contact form that needs an account is not a contact form"
        );
        let methods: Vec<&str> = menus.iter().map(|m| m.method.as_str()).collect();
        assert!(methods.contains(&"GET"), "{methods:?}");
        assert!(methods.contains(&"POST"), "{methods:?}");
    }

    #[test]
    fn the_form_asks_for_the_theme_and_carries_the_kernel_token() {
        let mut request = ApiRequest::new("show", "GET", PATH, "", false);
        request.csrf_token = "tok123".to_string();

        let response = __inner_tap_api(request);

        assert_eq!(response.status, 200);
        assert!(response.theme, "a public page must be themed");
        assert_eq!(response.title, "Contact");
        assert!(
            response
                .body
                .contains(r#"<input type="hidden" name="_token" value="tok123">"#),
            "{}",
            response.body
        );
        // No JavaScript anywhere in it: that is the point of the form.
        assert!(!response.body.contains("<script"), "{}", response.body);
        assert!(!response.body.contains("onsubmit"), "{}", response.body);
    }

    #[test]
    fn a_valid_submission_is_sent_and_confirmed() {
        let mut request = ApiRequest::new("submit", "POST", PATH, "", false);
        request.body = valid_body();
        request.csrf_token = "fresh".to_string();

        let response = __inner_tap_api(request);

        // The native SDK stub accepts the send, so this asserts the plugin's own
        // path: validation passed and the confirmation was rendered.
        assert_eq!(response.status, 200);
        assert!(response.theme);
        assert_eq!(response.title, "Message sent");
        assert!(
            response.body.contains("Thank you, Ada Lovelace"),
            "{}",
            response.body
        );
    }

    #[test]
    fn an_invalid_submission_comes_back_as_the_form_with_its_errors() {
        let mut request = ApiRequest::new("submit", "POST", PATH, "", false);
        request.body = "name=&email=nope&message=".to_string();
        request.csrf_token = "fresh".to_string();

        let response = __inner_tap_api(request);

        assert_eq!(response.status, 422, "understood, and not acted on");
        assert!(response.theme);
        assert!(response.body.contains("Please give your name."));
        assert!(response.body.contains("does not look right"));
        assert!(response.body.contains("Please write a message."));
        // And it carries the fresh token, because the submitted one is spent.
        assert!(
            response.body.contains(r#"name="_token" value="fresh""#),
            "{}",
            response.body
        );
    }

    #[test]
    fn a_rejected_submission_keeps_what_the_visitor_typed() {
        let mut request = ApiRequest::new("submit", "POST", PATH, "", false);
        request.body = "name=Ada&email=nope&subject=Hi&message=Keep+this".to_string();
        request.csrf_token = "fresh".to_string();

        let body = __inner_tap_api(request).body;

        assert!(body.contains(r#"value="Ada""#), "{body}");
        assert!(body.contains(r#"value="Hi""#), "{body}");
        assert!(body.contains(">Keep this</textarea>"), "{body}");
    }

    /// The form is public, so every value in it came from a stranger.
    #[test]
    fn everything_a_visitor_typed_is_escaped_on_the_way_back_out() {
        let mut request = ApiRequest::new("submit", "POST", PATH, "", false);
        request.body =
            "name=%22%3E%3Cscript%3Ealert(1)%3C%2Fscript%3E&email=nope&message=%3Cimg+src%3Dx%3E"
                .to_string();
        request.csrf_token = "fresh".to_string();

        let body = __inner_tap_api(request).body;

        assert!(!body.contains("<script"), "{body}");
        assert!(!body.contains("<img"), "{body}");
        assert!(body.contains("&lt;script&gt;"), "{body}");
        assert!(body.contains("&quot;&gt;"), "{body}");
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
    fn an_over_long_field_is_refused_with_a_reason() {
        let long = "x".repeat(MAX_MESSAGE + 1);
        let submission = Submission {
            name: "Ada".to_string(),
            email: "ada@example.test".to_string(),
            subject: String::new(),
            message: long,
            attach: false,
        };

        let errors = submission.errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("longer than"), "{errors:?}");
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
    fn a_field_reads_percent_and_plus_encoding() {
        let body = "name=Ada+Lovelace&email=ada%40example.test";
        assert_eq!(field(body, "name"), "Ada Lovelace");
        assert_eq!(field(body, "email"), "ada@example.test");
        assert_eq!(field(body, "absent"), "");
    }

    #[test]
    fn the_attachment_checkbox_is_off_unless_it_says_one() {
        assert!(!Submission::from_body("attach=0").attach);
        assert!(!Submission::from_body("").attach);
        assert!(Submission::from_body("attach=1").attach);
    }
}
