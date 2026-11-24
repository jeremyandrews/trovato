//! Mail: every message is queued in `mail_outbox`, then sent by a worker.
//!
//! BG18 (`docs/design/mail-queue.md`). Nothing in the kernel sends mail
//! directly any more. A caller builds a [`NewMail`] and calls
//! [`EmailService::queue`] with the same executor as the write that caused it,
//! so the message commits or rolls back with that write; the workers in
//! [`crate::services::mail_worker`] send it with retries, backoff, per blog
//! limits and suppression, and every message ends sent, failed, suppressed or
//! cancelled, visibly.
//!
//! A successful `queue` means *accepted for delivery*, not delivered.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::config::MailConfig;
use crate::models::DEFAULT_TENANT_ID;
use crate::services::mail_transport::MailTransport;

/// Where a one time secret goes in a queued body. The worker replaces every
/// occurrence with a secret it mints immediately before the send, so the
/// secret is never stored (see [`MailSecret`]). Letters and underscores only, so
/// it survives HTML escaping and sits safely inside a URL.
pub const SECRET_MARKER: &str = "__TROVATO_ONE_TIME_SECRET__";

/// Longest subject accepted, in bytes.
pub const MAX_SUBJECT_BYTES: usize = 512;

/// One file attached to an outgoing message.
///
/// `bytes` are the file's contents, already decoded: whatever encoding carried
/// them to the kernel is the caller's business, and lettre applies the transfer
/// encoding the message needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Filename offered to the recipient.
    pub filename: String,
    /// MIME type, e.g. `text/plain` or `application/pdf`. Rejected if it does
    /// not parse, rather than guessed at.
    pub content_type: String,
    /// File contents.
    pub bytes: Vec<u8>,
}

/// Transactional mail answers something a person did; bulk mail is sent to
/// many because a blog decided to. Transactional mail is never delayed behind
/// bulk mail, and the network bulk limit never holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailKind {
    /// Account mail and notifications.
    Transactional,
    /// Newsletters, digests and anything else sent to a list.
    Bulk,
}

impl MailKind {
    /// The `mail_outbox.kind` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transactional => "transactional",
            Self::Bulk => "bulk",
        }
    }
}

/// Priority of account mail (verification, password reset, recovery).
pub const PRIORITY_ACCOUNT: i16 = 0;
/// Priority of notifications (comments, plugin mail to the site).
pub const PRIORITY_NOTIFICATION: i16 = 1;
/// Priority of bulk mail.
pub const PRIORITY_BULK: i16 = 2;

/// A one time secret the worker mints at send time.
///
/// Reset links, verification links and recovery codes are stored hashed so a
/// database read cannot sign anyone in. A queued body holding the plain secret
/// would undo that for as long as it waited, so the row holds this reference
/// instead and the body holds [`SECRET_MARKER`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailSecret {
    /// A `password_reset_tokens` token for this user.
    PasswordReset {
        /// The account.
        user_id: Uuid,
    },
    /// An `email_verification_tokens` token with this purpose.
    Verification {
        /// The account.
        user_id: Uuid,
        /// `registration` or `email_change`.
        purpose: String,
    },
    /// A `recovery_email_challenges` code for this recovery flow.
    RecoveryCode {
        /// The account.
        user_id: Uuid,
        /// The flow the code is bound to.
        flow_id: String,
    },
}

/// A message to queue.
#[derive(Debug, Clone)]
pub struct NewMail {
    /// The blog it is sent for.
    pub tenant_id: Uuid,
    /// Transactional or bulk.
    pub kind: MailKind,
    /// Lower is sent first: [`PRIORITY_ACCOUNT`], [`PRIORITY_NOTIFICATION`],
    /// [`PRIORITY_BULK`].
    pub priority: i16,
    /// What caused it (`password_reset`, `plugin:trovato_contact`, ...).
    pub category: String,
    /// One address.
    pub recipient: String,
    /// Subject line, no control characters.
    pub subject: String,
    /// Plain text body.
    pub text_body: String,
    /// HTML alternative.
    pub html_body: Option<String>,
    /// Files attached.
    pub attachments: Vec<Attachment>,
    /// `List-Unsubscribe` target.
    pub unsubscribe_url: Option<String>,
    /// A secret to mint at send time.
    pub secret: Option<MailSecret>,
    /// A second message with the same key, for the same blog, is not queued.
    pub dedupe_key: Option<String>,
}

impl NewMail {
    fn base(
        kind: MailKind,
        priority: i16,
        category: &str,
        to: &str,
        subject: &str,
        text: &str,
    ) -> Self {
        Self {
            tenant_id: DEFAULT_TENANT_ID,
            kind,
            priority,
            category: category.to_string(),
            recipient: to.trim().to_string(),
            subject: clamp_subject(subject),
            text_body: text.to_string(),
            html_body: None,
            attachments: Vec::new(),
            unsubscribe_url: None,
            secret: None,
            dedupe_key: None,
        }
    }

    /// Account mail: verification, password reset, recovery. Sent first.
    pub fn account(category: &str, to: &str, subject: &str, text: &str) -> Self {
        Self::base(
            MailKind::Transactional,
            PRIORITY_ACCOUNT,
            category,
            to,
            subject,
            text,
        )
    }

    /// A notification about something that happened (a comment, a contact
    /// form, an account change).
    pub fn notification(category: &str, to: &str, subject: &str, text: &str) -> Self {
        Self::base(
            MailKind::Transactional,
            PRIORITY_NOTIFICATION,
            category,
            to,
            subject,
            text,
        )
    }

    /// Bulk mail, with the unsubscribe link its headers carry.
    pub fn bulk(
        category: &str,
        to: &str,
        subject: &str,
        text: &str,
        unsubscribe_url: &str,
    ) -> Self {
        let mut mail = Self::base(MailKind::Bulk, PRIORITY_BULK, category, to, subject, text);
        mail.unsubscribe_url = Some(unsubscribe_url.to_string());
        mail
    }

    /// Add an HTML alternative.
    #[must_use]
    pub fn html(mut self, html: Option<String>) -> Self {
        self.html_body = html;
        self
    }

    /// Queue at most one message with this key for this blog.
    #[must_use]
    pub fn dedupe(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    /// Mint `secret` at send time in place of [`SECRET_MARKER`].
    #[must_use]
    pub fn secret(mut self, secret: MailSecret) -> Self {
        self.secret = Some(secret);
        self
    }

    /// Send for this blog.
    #[must_use]
    pub fn tenant(mut self, tenant_id: Uuid) -> Self {
        self.tenant_id = tenant_id;
        self
    }

    /// Attach files.
    #[must_use]
    pub fn attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Refuse what cannot become a safe message, before it is stored.
    ///
    /// # Errors
    ///
    /// An unparseable recipient; an empty or over long subject; a control
    /// character in the subject, category, dedupe key, unsubscribe URL or an
    /// attachment's name or type (header injection); an unparseable attachment
    /// type; a secret without its marker in the body.
    pub fn validate(&self) -> Result<()> {
        if self.recipient.parse::<lettre::Address>().is_err() {
            bail!("refusing to queue mail: the recipient is not an email address");
        }
        let subject = self.subject.trim();
        if subject.is_empty() || subject.len() > MAX_SUBJECT_BYTES {
            bail!("refusing to queue mail: the subject is empty or too long");
        }
        let header_like = [
            Some(self.subject.as_str()),
            Some(self.category.as_str()),
            self.dedupe_key.as_deref(),
            self.unsubscribe_url.as_deref(),
        ];
        if header_like.into_iter().flatten().any(has_control) {
            bail!("refusing to queue mail: a header value contains a control character");
        }
        if self
            .unsubscribe_url
            .as_deref()
            .is_some_and(|u| u.contains('>'))
        {
            bail!("refusing to queue mail: the unsubscribe URL contains '>'");
        }
        for attachment in &self.attachments {
            if has_control(&attachment.filename) || has_control(&attachment.content_type) {
                bail!("refusing to queue mail: an attachment header contains a control character");
            }
            attachment
                .content_type
                .parse::<lettre::message::header::ContentType>()
                .with_context(|| {
                    format!(
                        "refusing to queue mail: attachment '{}' declares an invalid content type '{}'",
                        attachment.filename, attachment.content_type
                    )
                })?;
        }
        if self.secret.is_some() && !self.text_body.contains(SECRET_MARKER) {
            bail!(
                "refusing to queue mail: a secret is to be minted but the body has no place for it"
            );
        }
        Ok(())
    }
}

/// The `Message-ID` message `id` is sent with from `from_email`'s domain,
/// without angle brackets. See [`EmailService::message_id`].
pub fn message_id(from_email: &str, id: i64) -> String {
    let domain = from_email.rsplit_once('@').map_or("localhost", |(_, d)| d);
    format!("outbox-{id}@{domain}")
}

/// Whether `s` holds a control character (CR and LF above all).
pub fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// The site's mail: the outbox, the transport the workers send through, and
/// the settings they follow.
pub struct EmailService {
    pool: PgPool,
    transport: Arc<dyn MailTransport>,
    from_email: String,
    site_url: String,
    config: MailConfig,
    wake: Arc<Notify>,
}

impl EmailService {
    /// The service over `transport`.
    pub fn new(
        pool: PgPool,
        transport: Arc<dyn MailTransport>,
        from_email: String,
        site_url: String,
        config: MailConfig,
    ) -> Self {
        Self {
            pool,
            transport,
            from_email,
            site_url,
            config,
            wake: Arc::new(Notify::new()),
        }
    }

    /// The site URL used for links in mail.
    pub fn site_url(&self) -> &str {
        &self.site_url
    }

    /// The network's sending address.
    pub fn sender_address(&self) -> &str {
        &self.from_email
    }

    /// The `Message-ID` (without angle brackets) message `id` is sent with,
    /// every attempt: `outbox-{id}@{sending domain}`. A provider's bounce
    /// report names it ([`crate::services::mail_log::outbox_id_from_message_id`]).
    pub fn message_id(&self, id: i64) -> String {
        message_id(&self.from_email, id)
    }

    /// The transport the workers send through.
    pub fn transport(&self) -> &Arc<dyn MailTransport> {
        &self.transport
    }

    /// Worker and limit settings.
    pub fn config(&self) -> &MailConfig {
        &self.config
    }

    /// The pool the outbox lives in.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The notifier the workers wait on.
    pub fn wake_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Wake a worker now rather than at its next poll. Call after committing a
    /// transaction [`Self::queue`] wrote into.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Queue `mail` with `executor`: pass the transaction of the write that
    /// caused it, so the two commit or roll back together.
    ///
    /// Returns the new row's id, or `None` when a message with the same dedupe
    /// key is already queued or sent for this blog. Call [`Self::wake`] once the
    /// transaction commits: a wake before then finds nothing to claim.
    ///
    /// # Errors
    ///
    /// When [`NewMail::validate`] refuses the message or the insert fails.
    pub async fn queue<'e, E: PgExecutor<'e>>(
        &self,
        executor: E,
        mail: NewMail,
    ) -> Result<Option<i64>> {
        mail.validate()?;
        let attachments = if mail.attachments.is_empty() {
            None
        } else {
            use base64::Engine as _;
            let encoded: Vec<serde_json::Value> = mail
                .attachments
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "filename": a.filename,
                        "content_type": a.content_type,
                        "bytes_base64": base64::engine::general_purpose::STANDARD.encode(&a.bytes),
                    })
                })
                .collect();
            Some(serde_json::Value::Array(encoded))
        };
        let secret = mail
            .secret
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .context("failed to encode the mail secret reference")?;
        let id: Option<i64> = sqlx::query_scalar(
            r#"
            INSERT INTO mail_outbox
                (tenant_id, kind, priority, category, recipient, subject, text_body,
                 html_body, attachments, unsubscribe_url, secret, dedupe_key, max_attempts)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            ON CONFLICT (tenant_id, dedupe_key) WHERE dedupe_key IS NOT NULL DO NOTHING
            RETURNING id
            "#,
        )
        .bind(mail.tenant_id)
        .bind(mail.kind.as_str())
        .bind(mail.priority)
        .bind(&mail.category)
        .bind(&mail.recipient)
        .bind(mail.subject.trim())
        .bind(&mail.text_body)
        .bind(&mail.html_body)
        .bind(attachments)
        .bind(&mail.unsubscribe_url)
        .bind(secret)
        .bind(&mail.dedupe_key)
        .bind(self.config.max_attempts)
        .fetch_optional(executor)
        .await
        .context("failed to queue mail")?;
        Ok(id)
    }

    /// Queue `mail` on its own, for a caller with no write to join.
    ///
    /// # Errors
    ///
    /// As [`Self::queue`].
    pub async fn queue_now(&self, mail: NewMail) -> Result<Option<i64>> {
        let id = self.queue(&self.pool, mail).await?;
        self.wake();
        Ok(id)
    }
}

/// `subject` with control characters made spaces, trimmed and, past
/// [`MAX_SUBJECT_BYTES`], cut at a character boundary with "..." added. A
/// subject built from a blog name or post title must not stop the write the
/// mail belongs to; a plugin's subject is checked against both rules, and
/// refused, before it gets here.
fn clamp_subject(subject: &str) -> String {
    // A tab pasted into a blog name or an imported title is no reason to
    // refuse the mail: control characters become spaces. [`NewMail::validate`]
    // still refuses one put in afterwards.
    let cleaned: String = subject
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let subject = cleaned.trim();
    if subject.len() <= MAX_SUBJECT_BYTES {
        return subject.to_string();
    }
    let mut end = MAX_SUBJECT_BYTES - 3;
    while !subject.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &subject[..end])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_subject_with_a_newline_is_refused() {
        // Set after construction, past the cleaning: validation still refuses it.
        let mut mail = NewMail::notification("t", "a@example.test", "Hi", "b");
        mail.subject = "Hi\r\nBcc: x@evil.test".to_string();
        assert!(mail.validate().is_err());
    }

    #[test]
    fn a_kernel_subject_with_control_characters_is_cleaned_not_refused() {
        let mail =
            NewMail::notification("t", "a@example.test", "My\tBlog\r\nBcc: x@evil.test", "b");
        assert!(mail.validate().is_ok());
        assert!(!has_control(&mail.subject), "{:?}", mail.subject);
        assert_eq!(mail.subject, "My Blog  Bcc: x@evil.test");
    }

    #[test]
    fn a_long_subject_is_cut_at_a_character_boundary_not_refused() {
        // A long post title in a multi-byte script, as a comment notice builds.
        let long = format!("New comment on \"{}\"", "標題".repeat(200));
        let mail = NewMail::notification("t", "a@example.test", &long, "b");
        assert!(mail.validate().is_ok(), "the message is still queued");
        assert!(mail.subject.len() <= MAX_SUBJECT_BYTES);
        assert!(mail.subject.ends_with("..."));
        assert!(long.starts_with(mail.subject.trim_end_matches("...")));
        let short = NewMail::notification("t", "a@example.test", "  Hello  ", "b");
        assert_eq!(short.subject, "Hello");
    }

    #[test]
    fn a_recipient_that_is_not_an_address_is_refused() {
        for to in [
            "",
            "nobody",
            "a@b@c",
            "x@example.test\r\nBcc: y@example.test",
        ] {
            let mail = NewMail::notification("t", to, "s", "b");
            assert!(mail.validate().is_err(), "{to:?}");
        }
    }

    #[test]
    fn an_unsubscribe_url_cannot_close_its_angle_bracket() {
        let mail = NewMail::bulk(
            "t",
            "a@example.test",
            "s",
            "b",
            "https://x.test/>, <mailto:y",
        );
        assert!(mail.validate().is_err());
    }

    #[test]
    fn a_secret_needs_its_marker() {
        let user_id = Uuid::now_v7();
        let without = NewMail::account("password_reset", "a@example.test", "s", "no marker")
            .secret(MailSecret::PasswordReset { user_id });
        assert!(without.validate().is_err());
        let with = NewMail::account(
            "password_reset",
            "a@example.test",
            "s",
            &format!("https://x.test/reset/{SECRET_MARKER}"),
        )
        .secret(MailSecret::PasswordReset { user_id });
        assert!(with.validate().is_ok());
    }

    #[test]
    fn an_attachment_type_must_parse() {
        let mail =
            NewMail::notification("t", "a@example.test", "s", "b").attachments(vec![Attachment {
                filename: "a".to_string(),
                content_type: "not a type".to_string(),
                bytes: vec![],
            }]);
        assert!(mail.validate().is_err());
    }

    #[test]
    fn the_marker_survives_html_escaping_and_url_encoding() {
        assert_eq!(urlencoding::encode(SECRET_MARKER), SECRET_MARKER);
        assert!(
            SECRET_MARKER
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        );
    }

    #[test]
    fn secret_references_round_trip_without_a_secret() {
        let s = MailSecret::Verification {
            user_id: Uuid::nil(),
            purpose: "registration".to_string(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["kind"], "verification");
        assert_eq!(serde_json::from_value::<MailSecret>(v).unwrap(), s);
    }
}
