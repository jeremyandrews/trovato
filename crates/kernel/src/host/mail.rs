//! Mail host function for WASM plugins.
//!
//! # Why the plugin does not name the recipient
//!
//! A host function that sends to an address the caller supplies is a spam relay
//! wearing a CMS. Every plugin is code the site owner installed, but "installed
//! it" is not "audited it", and the blast radius of a relay is the site's
//! reputation and its SMTP credentials rather than its own data.
//!
//! So the recipient is not a parameter. `send-to-site-contacts` sends to the
//! address the site configured (`site_mail`), and nothing else. That is enough
//! for the case a CMS actually needs a plugin to cover — a visitor reaching the
//! site owner — and it is useless for sending mail to strangers, which is the
//! point.
//!
//! Two consequences worth stating plainly:
//!
//! - **The site's own address must be configured.** With `site_mail` empty there
//!   is no recipient to fall back to, and the call is refused rather than
//!   guessing at one (the `from` address is a transport identity, not a contact
//!   address).
//! - **A plugin can still be a nuisance.** Nothing here bounds how often a
//!   plugin calls this, so a plugin in a loop can fill the site owner's mailbox.
//!   The web-facing case is bounded by the `forms` rate-limit bucket every
//!   plugin-served POST already falls into (`middleware::rate_limit`); a plugin
//!   calling from a cron tap is not bounded, and is recorded in KNOWN-ISSUES.md
//!   rather than half-gated here.
//!
//! # Delivery
//!
//! The send goes through the site's one [`EmailService`], so it uses the SMTP
//! transport, the `from` address and — importantly — the **shared circuit
//! breaker** the kernel's own mail uses. A plugin cannot configure its own
//! delivery, and cannot keep hammering an SMTP host the kernel has already given
//! up on.

use anyhow::Result;
use base64::Engine as _;
use serde::Deserialize;
use tracing::{info, warn};
use wasmtime::Linker;

use super::read_string_from_memory;
use crate::plugin::{PluginState, WasmtimeExt};
use crate::services::email::Attachment;
use trovato_sdk::host_errors;

/// Most attachments one message may carry.
pub const MAX_ATTACHMENTS: usize = 5;

/// Most attachment bytes one message may carry, decoded and totalled.
///
/// Deliberately modest: this exists so a visitor can attach a document to a
/// contact form, not so a plugin can move files through the site's SMTP relay.
pub const MAX_ATTACHMENT_BYTES: usize = 1024 * 1024;

/// Longest subject accepted, in bytes.
pub const MAX_SUBJECT_BYTES: usize = 512;

/// Longest body accepted, in bytes.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Longest attachment filename accepted, in bytes.
pub const MAX_FILENAME_BYTES: usize = 255;

/// What a plugin sends across the boundary. Attachment bytes are base64 because
/// the payload is JSON, the same way every other structured host call travels.
#[derive(Debug, Deserialize)]
struct MailRequest {
    subject: String,
    body: String,
    #[serde(default)]
    attachments: Vec<MailAttachment>,
}

/// One attachment as it crosses the boundary.
#[derive(Debug, Deserialize)]
struct MailAttachment {
    filename: String,
    content_type: String,
    /// Base64-encoded file contents (standard alphabet, padding optional).
    bytes_base64: String,
}

/// Register the mail host functions.
///
/// # Errors
///
/// Returns an error if the linker rejects a definition (a duplicate name).
pub fn register_mail_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // send-to-site-contacts(req_ptr, req_len) -> i32
    // 0 on success, negative host_errors code otherwise. There is nothing to
    // return on success, so there is no output buffer.
    linker
        .func_wrap_async(
            "trovato:kernel/mail",
            "send-to-site-contacts",
            |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(request_json) =
                        read_string_from_memory(&memory, &caller, req_ptr, req_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let plugin_name = caller.data().plugin_name.clone();
                    let db = services.db.clone();
                    let Some(email) = services.email.clone() else {
                        warn!(
                            plugin = %plugin_name,
                            "plugin asked to send mail and the site has no SMTP host configured"
                        );
                        return host_errors::ERR_MAIL_NOT_CONFIGURED;
                    };

                    let request: MailRequest = match serde_json::from_str(&request_json) {
                        Ok(r) => r,
                        Err(e) => {
                            warn!(
                                plugin = %plugin_name,
                                error = %e,
                                "invalid mail request JSON from plugin"
                            );
                            return host_errors::ERR_PARAM_DESERIALIZE;
                        }
                    };

                    let (subject, body, attachments) = match validate(&request) {
                        Ok(parts) => parts,
                        Err(code) => {
                            warn!(
                                plugin = %plugin_name,
                                code,
                                "refusing a mail request the plugin built wrong"
                            );
                            return code;
                        }
                    };

                    // The recipient the plugin does not get to choose.
                    let recipient = match crate::models::SiteConfig::site_mail(&db).await {
                        Ok(address) if !address.trim().is_empty() => address,
                        Ok(_) => {
                            warn!(
                                plugin = %plugin_name,
                                "plugin asked to send to the site contact address and none is set"
                            );
                            return host_errors::ERR_MAIL_NO_RECIPIENT;
                        }
                        Err(e) => {
                            warn!(
                                plugin = %plugin_name,
                                error = %e,
                                "failed to read the site contact address"
                            );
                            return host_errors::ERR_MAIL_SEND_FAILED;
                        }
                    };

                    match email
                        .send_with_attachments(&recipient, subject, body, &attachments)
                        .await
                    {
                        Ok(()) => {
                            info!(
                                plugin = %plugin_name,
                                attachments = attachments.len(),
                                "plugin sent mail to the site contact address"
                            );
                            0
                        }
                        Err(e) => {
                            warn!(
                                plugin = %plugin_name,
                                error = %e,
                                "plugin mail send failed"
                            );
                            host_errors::ERR_MAIL_SEND_FAILED
                        }
                    }
                })
            },
        )
        .into_anyhow()?;

    Ok(())
}

/// Check a request and decode its attachments, or give back the code to return.
///
/// Borrowed subject and body come back rather than copies, so a valid request
/// costs nothing but the attachment decode.
fn validate(request: &MailRequest) -> Result<(&str, &str, Vec<Attachment>), i32> {
    let subject = request.subject.trim();
    if subject.is_empty() || subject.len() > MAX_SUBJECT_BYTES {
        return Err(host_errors::ERR_MAIL_INVALID_REQUEST);
    }
    // A newline in a header value is header injection: it would let a plugin
    // append its own headers, including a Bcc, and get the relay this interface
    // exists to withhold.
    if contains_control(subject) {
        return Err(host_errors::ERR_MAIL_INVALID_REQUEST);
    }

    if request.body.is_empty() || request.body.len() > MAX_BODY_BYTES {
        return Err(host_errors::ERR_MAIL_INVALID_REQUEST);
    }

    if request.attachments.len() > MAX_ATTACHMENTS {
        return Err(host_errors::ERR_MAIL_ATTACHMENT_TOO_LARGE);
    }

    let mut total = 0usize;
    let mut attachments = Vec::with_capacity(request.attachments.len());
    for attachment in &request.attachments {
        if !is_usable_filename(&attachment.filename) {
            return Err(host_errors::ERR_MAIL_INVALID_REQUEST);
        }
        if attachment.content_type.trim().is_empty() || contains_control(&attachment.content_type) {
            return Err(host_errors::ERR_MAIL_INVALID_REQUEST);
        }

        // Refuse on the encoded length before decoding, so an oversized
        // attachment cannot make the kernel allocate its decoded size first.
        // Base64 encodes 3 bytes as 4, so the decoded size cannot exceed
        // encoded * 3 / 4.
        if attachment.bytes_base64.len() / 4 * 3 > MAX_ATTACHMENT_BYTES {
            return Err(host_errors::ERR_MAIL_ATTACHMENT_TOO_LARGE);
        }

        let Ok(bytes) = base64::engine::general_purpose::STANDARD
            .decode(attachment.bytes_base64.as_bytes())
            .or_else(|_| {
                base64::engine::general_purpose::STANDARD_NO_PAD
                    .decode(attachment.bytes_base64.as_bytes())
            })
        else {
            return Err(host_errors::ERR_MAIL_INVALID_REQUEST);
        };

        total = total.saturating_add(bytes.len());
        if total > MAX_ATTACHMENT_BYTES {
            return Err(host_errors::ERR_MAIL_ATTACHMENT_TOO_LARGE);
        }

        attachments.push(Attachment {
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.trim().to_string(),
            bytes,
        });
    }

    Ok((subject, &request.body, attachments))
}

/// Whether a string carries a character that must not reach a message header.
///
/// CR and LF are the injection vector; the rest of the C0 range has no business
/// in a subject or a MIME type either.
fn contains_control(value: &str) -> bool {
    value.chars().any(|c| c.is_control())
}

/// Whether a filename can be offered to a recipient as-is.
///
/// Rejects an empty name, control characters, a quote (which would break out of
/// the `Content-Disposition` parameter), and path separators, so a plugin cannot
/// suggest a path to the recipient's mail client.
fn is_usable_filename(filename: &str) -> bool {
    !filename.is_empty()
        && filename.len() <= MAX_FILENAME_BYTES
        && !contains_control(filename)
        && !filename.contains('"')
        && !filename.contains('/')
        && !filename.contains('\\')
        && filename != "."
        && filename != ".."
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn request(subject: &str, body: &str) -> MailRequest {
        MailRequest {
            subject: subject.to_string(),
            body: body.to_string(),
            attachments: Vec::new(),
        }
    }

    fn attachment(filename: &str, content_type: &str, bytes: &[u8]) -> MailAttachment {
        MailAttachment {
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    #[test]
    fn a_plain_message_validates_and_is_trimmed() {
        let req = request("  Contact form  ", "hello");
        let (subject, body, attachments) = validate(&req).unwrap();
        assert_eq!(subject, "Contact form");
        assert_eq!(body, "hello");
        assert!(attachments.is_empty());
    }

    #[test]
    fn an_empty_subject_or_body_is_refused() {
        assert_eq!(
            validate(&request("", "hello")).unwrap_err(),
            host_errors::ERR_MAIL_INVALID_REQUEST
        );
        assert_eq!(
            validate(&request("   ", "hello")).unwrap_err(),
            host_errors::ERR_MAIL_INVALID_REQUEST
        );
        assert_eq!(
            validate(&request("Subject", "")).unwrap_err(),
            host_errors::ERR_MAIL_INVALID_REQUEST
        );
    }

    /// The one that matters: a newline in the subject would let a plugin append
    /// its own headers and get the relay this interface exists to withhold.
    #[test]
    fn a_subject_carrying_a_newline_is_refused() {
        for subject in [
            "Hello\r\nBcc: victim@example.com",
            "Hello\nBcc: victim@example.com",
            "Hello\rBcc: victim@example.com",
            "Hello\u{0}there",
        ] {
            assert_eq!(
                validate(&request(subject, "body")).unwrap_err(),
                host_errors::ERR_MAIL_INVALID_REQUEST,
                "must refuse {subject:?}"
            );
        }
    }

    #[test]
    fn an_oversized_subject_or_body_is_refused() {
        let long_subject = "s".repeat(MAX_SUBJECT_BYTES + 1);
        assert_eq!(
            validate(&request(&long_subject, "body")).unwrap_err(),
            host_errors::ERR_MAIL_INVALID_REQUEST
        );
        let long_body = "b".repeat(MAX_BODY_BYTES + 1);
        assert_eq!(
            validate(&request("Subject", &long_body)).unwrap_err(),
            host_errors::ERR_MAIL_INVALID_REQUEST
        );
    }

    #[test]
    fn an_attachment_round_trips_from_base64() {
        let mut req = request("Subject", "body");
        req.attachments = vec![attachment("notes.txt", "text/plain", b"hello bytes")];

        let (_, _, attachments) = validate(&req).unwrap();

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, "notes.txt");
        assert_eq!(attachments[0].content_type, "text/plain");
        assert_eq!(attachments[0].bytes, b"hello bytes");
    }

    #[test]
    fn bytes_that_are_not_base64_are_refused() {
        let mut req = request("Subject", "body");
        req.attachments = vec![MailAttachment {
            filename: "notes.txt".to_string(),
            content_type: "text/plain".to_string(),
            bytes_base64: "not valid base64 !!!".to_string(),
        }];

        assert_eq!(
            validate(&req).unwrap_err(),
            host_errors::ERR_MAIL_INVALID_REQUEST
        );
    }

    #[test]
    fn an_unusable_filename_is_refused() {
        for filename in [
            "",
            "../../etc/passwd",
            "sub/dir.txt",
            "back\\slash.txt",
            "quote\".txt",
            "new\nline.txt",
            ".",
            "..",
        ] {
            let mut req = request("Subject", "body");
            req.attachments = vec![attachment(filename, "text/plain", b"x")];
            assert_eq!(
                validate(&req).unwrap_err(),
                host_errors::ERR_MAIL_INVALID_REQUEST,
                "must refuse filename {filename:?}"
            );
        }
    }

    #[test]
    fn a_content_type_carrying_a_newline_is_refused() {
        let mut req = request("Subject", "body");
        req.attachments = vec![attachment("notes.txt", "text/plain\r\nBcc: x", b"x")];

        assert_eq!(
            validate(&req).unwrap_err(),
            host_errors::ERR_MAIL_INVALID_REQUEST
        );
    }

    #[test]
    fn too_many_attachments_are_refused() {
        let mut req = request("Subject", "body");
        req.attachments = (0..=MAX_ATTACHMENTS)
            .map(|i| attachment(&format!("f{i}.txt"), "text/plain", b"x"))
            .collect();

        assert_eq!(
            validate(&req).unwrap_err(),
            host_errors::ERR_MAIL_ATTACHMENT_TOO_LARGE
        );
    }

    #[test]
    fn attachments_over_the_byte_ceiling_are_refused_in_total_not_just_singly() {
        // Each is under the ceiling; together they are over it. Totalling is the
        // check, because five allowed attachments would otherwise be five times
        // the limit.
        let half = vec![b'x'; MAX_ATTACHMENT_BYTES / 2 + 1024];
        let mut req = request("Subject", "body");
        req.attachments = vec![
            attachment("a.bin", "application/octet-stream", &half),
            attachment("b.bin", "application/octet-stream", &half),
        ];

        assert_eq!(
            validate(&req).unwrap_err(),
            host_errors::ERR_MAIL_ATTACHMENT_TOO_LARGE
        );
    }

    #[test]
    fn a_single_oversized_attachment_is_refused_before_it_is_decoded() {
        // The encoded-length check fires first, so the kernel never allocates the
        // decoded size. Built as encoded text directly rather than by encoding
        // two megabytes of input.
        let mut req = request("Subject", "body");
        req.attachments = vec![MailAttachment {
            filename: "big.bin".to_string(),
            content_type: "application/octet-stream".to_string(),
            bytes_base64: "A".repeat(MAX_ATTACHMENT_BYTES * 2),
        }];

        assert_eq!(
            validate(&req).unwrap_err(),
            host_errors::ERR_MAIL_ATTACHMENT_TOO_LARGE
        );
    }
}
