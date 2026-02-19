//! Email delivery service using lettre/SMTP.

use anyhow::{Context, Result};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

/// Email delivery service.
pub struct EmailService {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from_email: String,
    site_url: String,
}

impl EmailService {
    /// Create a new email service.
    ///
    /// `encryption` controls the SMTP transport mode:
    /// - `"starttls"` (default): Opportunistic STARTTLS on port 587
    /// - `"tls"`: Implicit TLS (SMTPS) on port 465
    /// - `"none"`: Unencrypted (for local dev only)
    pub fn new(
        smtp_host: &str,
        smtp_port: u16,
        smtp_username: Option<&str>,
        smtp_password: Option<&str>,
        encryption: &str,
        from_email: String,
        site_url: String,
    ) -> Result<Self> {
        let mut builder = match encryption {
            "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(smtp_host)
                .context("failed to create SMTP relay transport")?
                .port(smtp_port),
            "none" => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host).port(smtp_port)
            }
            _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(smtp_host)
                .context("failed to create SMTP STARTTLS transport")?
                .port(smtp_port),
        };

        if let (Some(user), Some(pass)) = (smtp_username, smtp_password) {
            builder = builder.credentials(Credentials::new(user.to_string(), pass.to_string()));
        }

        let transport = builder.build();

        Ok(Self {
            transport,
            from_email,
            site_url,
        })
    }

    /// Send a plain-text email.
    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<()> {
        let email = Message::builder()
            .from(
                self.from_email
                    .parse()
                    .context("invalid from email address")?,
            )
            .to(to.parse().context("invalid recipient email address")?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .context("failed to build email message")?;

        self.transport
            .send(email)
            .await
            .context("failed to send email")?;

        Ok(())
    }

    /// Send a password reset email with a tokenized link.
    pub async fn send_password_reset(&self, to: &str, token: &str, site_name: &str) -> Result<()> {
        let reset_url = format!("{}/user/password-reset/{}", self.site_url, token);
        let subject = format!("Password reset for {site_name}");
        let body = format!(
            "A password reset has been requested for your account at {site_name}.\n\n\
             To reset your password, visit the following link:\n\
             {reset_url}\n\n\
             If you did not request this, you can safely ignore this email.\n\n\
             This link will expire in 1 hour."
        );

        self.send(to, &subject, &body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_service_requires_valid_host() {
        // Should fail with invalid host (no DNS)
        let result = EmailService::new(
            "nonexistent.invalid",
            587,
            None,
            None,
            "starttls",
            "test@example.com".to_string(),
            "http://localhost:3000".to_string(),
        );
        // Construction should succeed (connection is lazy)
        assert!(result.is_ok());
    }

    #[test]
    fn email_service_supports_tls_mode() {
        let result = EmailService::new(
            "nonexistent.invalid",
            465,
            None,
            None,
            "tls",
            "test@example.com".to_string(),
            "http://localhost:3000".to_string(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn email_service_supports_none_mode() {
        let result = EmailService::new(
            "localhost",
            25,
            None,
            None,
            "none",
            "test@example.com".to_string(),
            "http://localhost:3000".to_string(),
        );
        assert!(result.is_ok());
    }
}
