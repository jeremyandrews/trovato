//! How a queued message leaves the building (BG18, `docs/design/mail-queue.md`).
//!
//! The mail worker hands a fully resolved [`OutgoingMail`] to a
//! [`MailTransport`] and learns one of three things: it was accepted, it failed
//! for a reason that may pass ([`TransportError::permanent`] `false`: try again
//! later), or it failed for a reason that will not ([`TransportError::permanent`]
//! `true`: stop now). SMTP ([`SmtpTransport`]) is the default and the only one
//! built; an HTTP API provider is another implementation of the trait, and the
//! worker never knows which it has.

use anyhow::Context as _;
use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::circuit_breaker::{BreakerConfig, CircuitBreaker, CircuitBreakerError};
use crate::services::email::Attachment;

/// One message, ready to send: every address resolved, every secret minted.
#[derive(Debug, Clone)]
pub struct OutgoingMail {
    /// `Message-ID` without angle brackets, stable across retries.
    pub message_id: String,
    /// Display name for `From`, the blog's name.
    pub from_name: Option<String>,
    /// `From` address, the network's sending address.
    pub from_address: String,
    /// `Reply-To`, the blog's own address when it has one.
    pub reply_to: Option<String>,
    /// The one recipient.
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// Plain text body.
    pub text: String,
    /// HTML alternative, when there is one.
    pub html: Option<String>,
    /// Files attached.
    pub attachments: Vec<Attachment>,
    /// `List-Unsubscribe` target; bulk mail only.
    pub unsubscribe_url: Option<String>,
}

/// Why a send did not happen.
#[derive(Debug, Clone)]
pub struct TransportError {
    /// `true` when trying again cannot help (an SMTP 5xx refusing the
    /// recipient or the message, or a message that cannot be built).
    pub permanent: bool,
    /// What went wrong, for `mail_outbox.last_error`.
    pub message: String,
}

impl TransportError {
    /// A failure that may pass.
    pub fn temporary(message: impl Into<String>) -> Self {
        Self {
            permanent: false,
            message: message.into(),
        }
    }

    /// A failure that will not.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            permanent: true,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = if self.permanent {
            "permanent"
        } else {
            "temporary"
        };
        write!(f, "{kind}: {}", self.message)
    }
}

/// Something that can deliver an [`OutgoingMail`].
#[async_trait]
pub trait MailTransport: Send + Sync {
    /// Hand the message over. `Ok` means the far end accepted it for delivery.
    async fn send(&self, message: &OutgoingMail) -> Result<(), TransportError>;

    /// The transport's circuit breaker state (`closed`, `open`, `half_open`),
    /// when it has one, for `/metrics` and `/health`.
    fn breaker_state(&self) -> Option<&'static str> {
        None
    }
}

/// SMTP over lettre, behind the kernel's circuit breaker.
pub struct SmtpTransport {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    breaker: CircuitBreaker,
}

/// SMTP reply codes that mean the *site* is misconfigured, not that this
/// message is bad: authentication required, too weak, failed, or needs
/// encryption. They are 5xx, but failing every queued message at once on them
/// would turn a wrong password into lost mail, so they are retried like an
/// outage.
const SMTP_AUTH_CODES: [&str; 4] = ["530", "534", "535", "538"];

impl SmtpTransport {
    /// Build the transport. `encryption` is `starttls` (default), `tls`
    /// (implicit TLS) or `none` (local development only). Nothing connects
    /// until the first send.
    ///
    /// # Errors
    ///
    /// When the relay cannot be configured for `host`.
    pub fn new(
        host: &str,
        port: u16,
        username: Option<&str>,
        password: Option<&str>,
        encryption: &str,
    ) -> anyhow::Result<Self> {
        let mut builder = match encryption {
            "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(host)
                .context("failed to create SMTP relay transport")?
                .port(port),
            "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host).port(port),
            _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
                .context("failed to create SMTP STARTTLS transport")?
                .port(port),
        };
        if let (Some(user), Some(pass)) = (username, password) {
            builder = builder.credentials(Credentials::new(user.to_string(), pass.to_string()));
        }
        // A black-holed server must not hold a worker (and its claim) for
        // lettre's default minute per command.
        builder = builder.timeout(Some(std::time::Duration::from_secs(30)));
        Ok(Self {
            transport: builder.build(),
            breaker: CircuitBreaker::new("email_smtp", BreakerConfig::default()),
        })
    }
}

/// Build the MIME message for `mail`.
///
/// # Errors
///
/// A permanent [`TransportError`] when an address does not parse, an
/// attachment's content type is not a MIME type, or lettre refuses the
/// message. None of those change on a retry.
pub fn build_message(mail: &OutgoingMail) -> Result<Message, TransportError> {
    use lettre::message::header::{HeaderName, HeaderValue};

    let bad = |what: &str, e: &dyn std::fmt::Display| {
        TransportError::permanent(format!("cannot build the message: {what}: {e}"))
    };
    let from_address = mail
        .from_address
        .parse()
        .map_err(|e| bad("from address", &e))?;
    let from = Mailbox::new(mail.from_name.clone(), from_address);
    let to: Mailbox = mail.to.parse().map_err(|e| bad("recipient", &e))?;

    let mut builder = Message::builder()
        .from(from)
        .to(to)
        .subject(mail.subject.clone())
        .message_id(Some(format!("<{}>", mail.message_id)));
    if let Some(reply_to) = &mail.reply_to {
        let reply_to: Mailbox = reply_to.parse().map_err(|e| bad("reply-to", &e))?;
        builder = builder.reply_to(reply_to);
    }
    if let Some(url) = &mail.unsubscribe_url {
        builder = builder
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("List-Unsubscribe"),
                format!("<{url}>"),
            ))
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("List-Unsubscribe-Post"),
                "List-Unsubscribe=One-Click".to_string(),
            ));
    }

    let text_part = SinglePart::builder()
        .header(ContentType::TEXT_PLAIN)
        .body(mail.text.clone());
    let body = mail.html.as_ref().map(|html| {
        MultiPart::alternative()
            .singlepart(text_part.clone())
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(html.clone()),
            )
    });

    let message = if mail.attachments.is_empty() {
        match body {
            Some(alternative) => builder.multipart(alternative),
            None => builder
                .header(ContentType::TEXT_PLAIN)
                .body(mail.text.clone()),
        }
    } else {
        let mut mixed = match body {
            Some(alternative) => MultiPart::mixed().multipart(alternative),
            None => MultiPart::mixed().singlepart(text_part),
        };
        for attachment in &mail.attachments {
            let content_type = attachment
                .content_type
                .parse::<ContentType>()
                .map_err(|e| bad(&format!("attachment '{}'", attachment.filename), &e))?;
            mixed = mixed.singlepart(
                lettre::message::Attachment::new(attachment.filename.clone())
                    .body(attachment.bytes.clone(), content_type),
            );
        }
        builder.multipart(mixed)
    };
    message.map_err(|e| bad("message", &e))
}

/// Sort an SMTP error into temporary or permanent.
fn classify_smtp(error: &lettre::transport::smtp::Error) -> TransportError {
    let code = error.status().map(|c| c.to_string());
    let permanent = error.is_permanent()
        && !code
            .as_deref()
            .is_some_and(|c| SMTP_AUTH_CODES.contains(&c));
    let message = format!("SMTP: {error}");
    if permanent {
        TransportError::permanent(message)
    } else {
        TransportError::temporary(message)
    }
}

#[async_trait]
impl MailTransport for SmtpTransport {
    async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        let message = build_message(mail)?;
        // Only temporary failures count against the breaker: one bad address
        // answering 550 says nothing about whether the server is up, and must
        // not stop everyone else's mail.
        let outcome = self
            .breaker
            .call(|| async {
                match self.transport.send(message).await {
                    Ok(_) => Ok(Ok(())),
                    Err(e) => {
                        let classified = classify_smtp(&e);
                        if classified.permanent {
                            Ok(Err(classified))
                        } else {
                            Err(classified)
                        }
                    }
                }
            })
            .await;
        match outcome {
            Ok(result) => result,
            Err(CircuitBreakerError::Open) => Err(TransportError::temporary(
                "SMTP circuit breaker is open after repeated failures",
            )),
            Err(CircuitBreakerError::ServiceError(e)) => Err(e),
        }
    }

    fn breaker_state(&self) -> Option<&'static str> {
        Some(self.breaker.state_name())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn mail() -> OutgoingMail {
        OutgoingMail {
            message_id: "outbox-7@example.test".to_string(),
            from_name: Some("Blog \"One\", with <brackets>".to_string()),
            from_address: "noreply@example.test".to_string(),
            reply_to: Some("owner@example.test".to_string()),
            to: "reader@example.test".to_string(),
            subject: "Hello".to_string(),
            text: "Plain".to_string(),
            html: None,
            attachments: Vec::new(),
            unsubscribe_url: None,
        }
    }

    fn formatted(mail: &OutgoingMail) -> String {
        String::from_utf8(build_message(mail).unwrap().formatted()).unwrap()
    }

    // Pooled connections are dropped on the runtime, so this needs one.
    #[tokio::test]
    async fn transports_build_lazily_in_every_mode() {
        for mode in ["starttls", "tls", "none"] {
            assert!(SmtpTransport::new("localhost", 25, None, None, mode).is_ok());
        }
    }

    #[test]
    fn headers_carry_the_stable_message_id_and_reply_to() {
        let text = formatted(&mail());
        assert!(
            text.contains("Message-ID: <outbox-7@example.test>"),
            "{text}"
        );
        assert!(text.contains("Reply-To: owner@example.test"), "{text}");
        assert!(!text.contains("List-Unsubscribe"), "{text}");
    }

    #[test]
    fn a_blog_name_with_quotes_and_brackets_stays_one_display_name() {
        let text = formatted(&mail());
        let from = text.lines().find(|l| l.starts_with("From: ")).unwrap();
        assert!(from.ends_with("<noreply@example.test>"), "{from}");
        // The name is encoded (RFC 2047), so its own quotes and angle brackets
        // cannot open a second address.
        assert_eq!(from.matches('<').count(), 1, "one address only: {from}");
    }

    #[test]
    fn bulk_mail_carries_one_click_unsubscribe_headers() {
        let mut m = mail();
        m.unsubscribe_url = Some("https://blog.test/u/abc".to_string());
        let text = formatted(&m);
        assert!(
            text.contains("List-Unsubscribe: <https://blog.test/u/abc>"),
            "{text}"
        );
        assert!(
            text.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"),
            "{text}"
        );
    }

    #[test]
    fn html_and_attachments_nest_as_mixed_around_alternative() {
        let mut m = mail();
        m.html = Some("<p>Hi</p>".to_string());
        m.attachments = vec![Attachment {
            filename: "a.txt".to_string(),
            content_type: "text/plain".to_string(),
            bytes: b"file".to_vec(),
        }];
        let text = formatted(&m);
        assert!(text.contains("multipart/mixed"), "{text}");
        assert!(text.contains("multipart/alternative"), "{text}");
        assert!(text.contains("a.txt"), "{text}");
    }

    #[test]
    fn an_unparseable_recipient_is_permanent() {
        let mut m = mail();
        m.to = "not an address".to_string();
        let err = build_message(&m).unwrap_err();
        assert!(err.permanent, "{err}");
    }
}
