//! Webhook dispatch and delivery service.
//!
//! Dispatches webhooks on content events with HMAC-SHA256 signed payloads
//! and exponential backoff retry.

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Retry delays in seconds: 1min, 5min, 30min, 2hr.
const RETRY_DELAYS: &[i64] = &[60, 300, 1800, 7200];

/// Maximum delivery attempts.
const MAX_ATTEMPTS: i16 = 4;

/// Webhook configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct Webhook {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: serde_json::Value,
    pub secret: String,
    pub active: bool,
    pub created: i64,
    pub changed: i64,
}

/// Webhook delivery record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub webhook_id: Uuid,
    pub event: String,
    pub payload: serde_json::Value,
    pub status_code: Option<i16>,
    pub response: Option<String>,
    pub attempts: i16,
    pub next_retry: Option<i64>,
    pub created: i64,
}

/// Webhook dispatch and delivery service.
#[derive(Clone)]
pub struct WebhookService {
    pool: PgPool,
    client: reqwest::Client,
    encryption_key: Option<[u8; 32]>,
}

impl WebhookService {
    /// Create a new webhook service.
    ///
    /// If `encryption_key` is provided, webhook secrets are encrypted at rest
    /// using AES-256-GCM. Pass `None` to disable encryption (not recommended
    /// for production).
    pub fn new(pool: PgPool, encryption_key: Option<[u8; 32]>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            // Disable redirect following to prevent SSRF bypass via 302 to internal IPs
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();

        Self {
            pool,
            client,
            encryption_key,
        }
    }

    /// Encrypt a webhook secret for storage.
    ///
    /// Returns `enc:` + hex(nonce || ciphertext) or the raw secret if no key.
    pub fn encrypt_secret(&self, plaintext: &str) -> Result<String> {
        let Some(key) = &self.encryption_key else {
            return Ok(plaintext.to_string());
        };

        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
        use rand::RngCore;

        let cipher = Aes256Gcm::new(key.into());

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

        // Concatenate nonce + ciphertext and hex-encode
        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        Ok(format!("enc:{}", hex::encode(combined)))
    }

    /// Decrypt a webhook secret from storage.
    ///
    /// Handles both encrypted (`enc:...`) and legacy plaintext secrets.
    pub fn decrypt_secret(&self, stored: &str) -> Result<String> {
        let Some(encrypted_hex) = stored.strip_prefix("enc:") else {
            // Legacy plaintext secret — log warning
            if !stored.is_empty() {
                warn!("webhook secret stored as plaintext; re-save to encrypt");
            }
            return Ok(stored.to_string());
        };

        let Some(key) = &self.encryption_key else {
            anyhow::bail!("encrypted webhook secret but no WEBHOOK_ENCRYPTION_KEY configured");
        };

        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};

        let combined = hex::decode(encrypted_hex).context("invalid hex in encrypted secret")?;
        if combined.len() < 12 {
            anyhow::bail!("encrypted secret too short");
        }

        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let cipher = Aes256Gcm::new(key.into());
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("decryption failed: {e}"))?;

        String::from_utf8(plaintext).context("decrypted secret is not valid UTF-8")
    }

    /// Dispatch an event to all matching webhooks.
    pub async fn dispatch(
        &self,
        event: &str,
        entity_type: &str,
        entity_id: &str,
        payload: serde_json::Value,
    ) -> Result<u64> {
        // Find active webhooks that subscribe to this event
        let webhooks = sqlx::query_as::<_, Webhook>(
            r#"
            SELECT id, name, url, events, secret, active, created, changed
            FROM webhook
            WHERE active = true AND events @> $1::jsonb
            "#,
        )
        .bind(serde_json::json!([event]))
        .fetch_all(&self.pool)
        .await;

        let webhooks = match webhooks {
            Ok(w) => w,
            Err(e) => {
                debug!(error = %e, "webhook table may not exist yet");
                return Ok(0);
            }
        };

        let mut queued = 0u64;
        let now = chrono::Utc::now().timestamp();

        let full_payload = serde_json::json!({
            "event": event,
            "entity_type": entity_type,
            "entity_id": entity_id,
            "timestamp": now,
            "data": payload,
        });

        for webhook in &webhooks {
            sqlx::query(
                r#"
                INSERT INTO webhook_delivery (id, webhook_id, event, payload, attempts, created)
                VALUES (gen_random_uuid(), $1, $2, $3, 0, $4)
                "#,
            )
            .bind(webhook.id)
            .bind(event)
            .bind(&full_payload)
            .bind(now)
            .execute(&self.pool)
            .await
            .context("failed to queue webhook delivery")?;

            queued += 1;
        }

        if queued > 0 {
            debug!(event = %event, queued = queued, "queued webhook deliveries");
        }

        Ok(queued)
    }

    /// Process pending webhook deliveries.
    ///
    /// Called by cron. Attempts delivery for items with no next_retry or
    /// next_retry <= now, up to MAX_ATTEMPTS.
    pub async fn process_deliveries(&self) -> Result<u64> {
        let now = chrono::Utc::now().timestamp();
        let mut processed = 0u64;

        // Fetch pending deliveries with row-level locking to prevent
        // duplicate delivery when multiple cron workers run concurrently.
        // FOR UPDATE SKIP LOCKED ensures each delivery is processed by exactly one worker.
        let deliveries = sqlx::query_as::<_, WebhookDelivery>(
            r#"
            SELECT d.id, d.webhook_id, d.event, d.payload, d.status_code,
                   d.response, d.attempts, d.next_retry, d.created
            FROM webhook_delivery d
            WHERE d.status_code IS NULL
              AND d.attempts < $1
              AND (d.next_retry IS NULL OR d.next_retry <= $2)
            ORDER BY d.created ASC
            LIMIT 50
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(MAX_ATTEMPTS)
        .bind(now)
        .fetch_all(&self.pool)
        .await;

        let deliveries = match deliveries {
            Ok(d) => d,
            Err(e) => {
                debug!(error = %e, "webhook_delivery table may not exist yet");
                return Ok(0);
            }
        };

        for delivery in deliveries {
            // Load webhook for URL and secret
            let webhook = sqlx::query_as::<_, Webhook>(
                "SELECT id, name, url, events, secret, active, created, changed FROM webhook WHERE id = $1",
            )
            .bind(delivery.webhook_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to load webhook")?;

            let Some(webhook) = webhook else {
                // Webhook deleted, mark delivery as failed
                sqlx::query("UPDATE webhook_delivery SET status_code = 0, response = 'webhook deleted' WHERE id = $1")
                    .bind(delivery.id)
                    .execute(&self.pool)
                    .await?;
                processed += 1;
                continue;
            };

            // Attempt delivery
            let result = self.deliver(&webhook, &delivery).await;
            let attempt = delivery.attempts + 1;

            match result {
                Ok((status, body)) => {
                    sqlx::query(
                        r#"
                        UPDATE webhook_delivery
                        SET status_code = $1, response = $2, attempts = $3, next_retry = NULL
                        WHERE id = $4
                        "#,
                    )
                    .bind(status)
                    .bind(&body)
                    .bind(attempt)
                    .bind(delivery.id)
                    .execute(&self.pool)
                    .await?;

                    info!(
                        webhook = %webhook.name,
                        status = status,
                        "webhook delivered"
                    );
                }
                Err(e) => {
                    // Schedule retry with exponential backoff
                    let retry_index = (attempt - 1).min(RETRY_DELAYS.len() as i16 - 1) as usize;
                    let next_retry = if attempt < MAX_ATTEMPTS {
                        Some(now + RETRY_DELAYS[retry_index])
                    } else {
                        None // Max attempts reached
                    };

                    let err_msg = e.to_string();
                    sqlx::query(
                        r#"
                        UPDATE webhook_delivery
                        SET attempts = $1, next_retry = $2,
                            response = $3
                        WHERE id = $4
                        "#,
                    )
                    .bind(attempt)
                    .bind(next_retry)
                    .bind(&err_msg)
                    .bind(delivery.id)
                    .execute(&self.pool)
                    .await?;

                    warn!(
                        webhook = %webhook.name,
                        attempt = attempt,
                        error = %e,
                        "webhook delivery failed"
                    );
                }
            }

            processed += 1;
        }

        Ok(processed)
    }

    /// Deliver a single webhook.
    async fn deliver(
        &self,
        webhook: &Webhook,
        delivery: &WebhookDelivery,
    ) -> Result<(i16, String)> {
        // SSRF prevention: validate URL structure and resolve DNS to verify IPs.
        // DNS resolution at delivery time mitigates DNS rebinding attacks where
        // a domain initially resolves to a public IP (passing registration checks)
        // then rebinds to an internal IP at request time.
        validate_webhook_url(&webhook.url)?;
        validate_resolved_ips(&webhook.url).await?;

        let payload =
            serde_json::to_string(&delivery.payload).context("failed to serialize payload")?;

        // Decrypt secret (handles both encrypted and legacy plaintext)
        let secret = self
            .decrypt_secret(&webhook.secret)
            .context("failed to decrypt webhook secret")?;

        // Compute HMAC-SHA256 signature.
        // Webhooks without a secret are delivered unsigned — this is a security
        // risk since recipients can't verify payload authenticity. Warn loudly.
        let signature = if !secret.is_empty() {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let mut mac =
                Hmac::<Sha256>::new_from_slice(secret.as_bytes()).context("invalid secret key")?;
            mac.update(payload.as_bytes());
            let result = mac.finalize();
            hex::encode(result.into_bytes())
        } else {
            warn!(
                webhook = %webhook.name,
                "delivering webhook without HMAC signature: no secret configured"
            );
            String::new()
        };

        let mut request = self
            .client
            .post(&webhook.url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "Trovato-Webhook/1.0");

        if !signature.is_empty() {
            request = request.header("X-Webhook-Signature", format!("sha256={signature}"));
        }

        let response = request
            .body(payload)
            .send()
            .await
            .context("HTTP request failed")?;

        let status = response.status().as_u16() as i16;
        let body = response.text().await.unwrap_or_else(|_| String::new());

        // Sanitize response body before storing.
        // Strip control characters (except \n, \r, \t) to prevent log injection
        // and reduce XSS risk if the admin UI renders this without escaping.
        let body: String = body
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
            .collect();

        // Truncate to ~4096 bytes at a valid char boundary
        let body = if body.len() > 4096 {
            // Find the last valid char boundary at or before 4096
            let mut truncate_at = 4096;
            while truncate_at > 0 && !body.is_char_boundary(truncate_at) {
                truncate_at -= 1;
            }
            format!("{}...[truncated]", &body[..truncate_at])
        } else {
            body
        };

        if (200..300).contains(&status) {
            Ok((status, body))
        } else {
            anyhow::bail!("HTTP {}: {}", status, &body[..body.len().min(200)])
        }
    }
}

/// Validate that a webhook URL is safe to deliver to (SSRF prevention).
///
/// Blocks requests to:
/// - Non-HTTP(S) schemes
/// - Loopback addresses (127.x.x.x, ::1)
/// - Private network ranges (10.x, 172.16-31.x, 192.168.x, fc00::/7)
/// - Link-local addresses (169.254.x.x, fe80::/10)
/// - Cloud metadata endpoints (169.254.169.254)
/// - Known private hostnames (localhost, *.local, *.internal)
fn validate_webhook_url(url_str: &str) -> Result<()> {
    let parsed = url::Url::parse(url_str).context("invalid webhook URL")?;

    match parsed.scheme() {
        "http" | "https" => {}
        scheme => anyhow::bail!("unsupported URL scheme: {scheme}"),
    }

    // Restrict to standard HTTP ports to prevent port scanning via SSRF.
    // Allow 80, 443 (defaults), and 8080-8443 (common webhook/API ports).
    if let Some(port) = parsed.port()
        && port != 80
        && port != 443
        && !(8080..=8443).contains(&port)
    {
        anyhow::bail!(
            "webhook URL uses non-standard port {port}: only 80, 443, and 8080-8443 are allowed"
        );
    }

    let Some(host) = parsed.host() else {
        anyhow::bail!("webhook URL has no host");
    };

    match host {
        url::Host::Domain(domain) => {
            let domain_lower = domain.to_lowercase();
            if domain_lower == "localhost"
                || domain_lower.ends_with(".local")
                || domain_lower.ends_with(".internal")
                || domain_lower.ends_with(".localhost")
            {
                anyhow::bail!("webhook URL points to a private hostname: {domain}");
            }
            // Domain could also be a raw IP string in some edge cases
            if let Ok(ip) = domain.parse::<std::net::IpAddr>()
                && !is_public_ip(ip)
            {
                anyhow::bail!("webhook URL points to a non-public IP: {ip}");
            }
        }
        url::Host::Ipv4(ip) => {
            if !is_public_ip(std::net::IpAddr::V4(ip)) {
                anyhow::bail!("webhook URL points to a non-public IPv4: {ip}");
            }
        }
        url::Host::Ipv6(ip) => {
            if !is_public_ip(std::net::IpAddr::V6(ip)) {
                anyhow::bail!("webhook URL points to a non-public IPv6: {ip}");
            }
        }
    }

    Ok(())
}

/// Check if an IP address is publicly routable.
fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let octets = v4.octets();
            !v4.is_loopback()         // 127.0.0.0/8
                && !v4.is_private()       // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                && !v4.is_link_local()    // 169.254.0.0/16
                && !v4.is_unspecified()   // 0.0.0.0
                && !v4.is_broadcast()     // 255.255.255.255
                && !v4.is_documentation() // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24
                // Cloud metadata endpoint
                && v4 != std::net::Ipv4Addr::new(169, 254, 169, 254)
                // CGNAT / Shared Address Space (RFC 6598): 100.64.0.0/10
                && !(octets[0] == 100 && (octets[1] & 0xC0) == 64)
                // Benchmarking (RFC 2544): 198.18.0.0/15
                && !(octets[0] == 198 && (octets[1] & 0xFE) == 18)
        }
        std::net::IpAddr::V6(v6) => {
            // Check IPv4-mapped addresses (::ffff:x.x.x.x) — validate the embedded IPv4
            if let Some(mapped_v4) = v6.to_ipv4_mapped() {
                return is_public_ip(std::net::IpAddr::V4(mapped_v4));
            }
            !v6.is_loopback()       // ::1
                && !v6.is_unspecified() // ::
                // fc00::/7 (unique local)
                && (v6.segments()[0] & 0xfe00) != 0xfc00
                // fe80::/10 (link-local)
                && (v6.segments()[0] & 0xffc0) != 0xfe80
        }
    }
}

/// Resolve the webhook URL's hostname via DNS and verify all resolved IPs are public.
///
/// This is a defense-in-depth measure against DNS rebinding attacks. By resolving
/// the hostname ourselves and checking each resolved IP, we narrow the window
/// between validation and connection. While not a perfect mitigation (DNS could
/// change between our check and reqwest's connect), it significantly raises the bar.
async fn validate_resolved_ips(url_str: &str) -> Result<()> {
    let parsed = url::Url::parse(url_str).context("invalid webhook URL")?;

    let host = match parsed.host() {
        Some(url::Host::Domain(d)) => d.to_string(),
        // IP-literal hosts are already validated by validate_webhook_url
        _ => return Ok(()),
    };

    let port = parsed.port_or_known_default().unwrap_or(443);
    let lookup = format!("{host}:{port}");

    let addrs = tokio::net::lookup_host(&lookup)
        .await
        .with_context(|| format!("DNS resolution failed for {host}"))?;

    let mut found_any = false;
    for addr in addrs {
        found_any = true;
        if !is_public_ip(addr.ip()) {
            anyhow::bail!(
                "webhook URL '{}' resolved to non-public IP: {}",
                host,
                addr.ip()
            );
        }
    }

    if !found_any {
        anyhow::bail!("webhook URL '{host}' did not resolve to any address");
    }

    Ok(())
}

impl std::fmt::Debug for WebhookService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookService").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_are_exponential() {
        assert_eq!(RETRY_DELAYS, &[60, 300, 1800, 7200]);
    }

    #[test]
    fn max_attempts_is_four() {
        assert_eq!(MAX_ATTEMPTS, 4);
    }

    #[test]
    fn webhook_payload_structure() {
        let payload = serde_json::json!({
            "event": "item.create",
            "entity_type": "blog",
            "entity_id": "123",
            "timestamp": 1000,
            "data": {},
        });
        assert_eq!(payload["event"], "item.create");
    }

    /// Test encrypt/decrypt directly using the standalone logic, avoiding PgPool.
    fn encrypt_with_key(key: &[u8; 32], plaintext: &str) -> Result<String> {
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
        use rand::RngCore;

        let cipher = Aes256Gcm::new(key.into());
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);
        Ok(format!("enc:{}", hex::encode(combined)))
    }

    fn decrypt_with_key(key: &[u8; 32], stored: &str) -> Result<String> {
        let encrypted_hex = stored
            .strip_prefix("enc:")
            .ok_or_else(|| anyhow::anyhow!("not encrypted"))?;

        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};

        let combined = hex::decode(encrypted_hex).context("invalid hex")?;
        if combined.len() < 12 {
            anyhow::bail!("too short");
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let cipher = Aes256Gcm::new(key.into());
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("decryption failed: {e}"))?;
        String::from_utf8(plaintext).context("not utf-8")
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let secret = "my-webhook-secret-key";
        let encrypted = encrypt_with_key(&key, secret).unwrap();

        assert!(encrypted.starts_with("enc:"));
        assert_ne!(encrypted, secret);

        let decrypted = decrypt_with_key(&key, &encrypted).unwrap();
        assert_eq!(decrypted, secret);
    }

    #[test]
    fn plaintext_passthrough_without_enc_prefix() {
        let key = [0x42u8; 32];
        let secret = "plaintext-secret";
        // Without enc: prefix, decrypt_with_key fails — this is expected.
        // The WebhookService.decrypt_secret handles this by returning the raw string.
        assert!(decrypt_with_key(&key, secret).is_err());
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key1 = [0x42u8; 32];
        let key2 = [0x99u8; 32];
        let encrypted = encrypt_with_key(&key1, "secret").unwrap();
        assert!(decrypt_with_key(&key2, &encrypted).is_err());
    }

    #[test]
    fn ssrf_blocks_private_ips() {
        assert!(validate_webhook_url("https://127.0.0.1/hook").is_err());
        assert!(validate_webhook_url("https://10.0.0.1/hook").is_err());
        assert!(validate_webhook_url("https://172.16.0.1/hook").is_err());
        assert!(validate_webhook_url("https://192.168.1.1/hook").is_err());
        assert!(validate_webhook_url("https://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_webhook_url("http://0.0.0.0/hook").is_err());
        assert!(validate_webhook_url("http://[::1]/hook").is_err());
    }

    #[test]
    fn ssrf_blocks_ipv4_mapped_ipv6() {
        // ::ffff:127.0.0.1 — IPv4-mapped loopback
        assert!(validate_webhook_url("http://[::ffff:127.0.0.1]/hook").is_err());
        // ::ffff:10.0.0.1 — IPv4-mapped private
        assert!(validate_webhook_url("http://[::ffff:10.0.0.1]/hook").is_err());
        // ::ffff:169.254.169.254 — IPv4-mapped cloud metadata
        assert!(validate_webhook_url("http://[::ffff:169.254.169.254]/meta").is_err());
        // ::ffff:192.168.1.1 — IPv4-mapped private
        assert!(validate_webhook_url("http://[::ffff:192.168.1.1]/hook").is_err());
    }

    #[test]
    fn ipv4_mapped_public_ip_allowed() {
        use std::net::{IpAddr, Ipv6Addr};
        // ::ffff:8.8.8.8 should be allowed (public)
        let mapped = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0808, 0x0808);
        assert!(is_public_ip(IpAddr::V6(mapped)));
    }

    #[test]
    fn ssrf_blocks_private_hostnames() {
        assert!(validate_webhook_url("https://localhost/hook").is_err());
        assert!(validate_webhook_url("https://server.local/hook").is_err());
        assert!(validate_webhook_url("https://db.internal/hook").is_err());
        assert!(validate_webhook_url("https://foo.localhost/hook").is_err());
    }

    #[test]
    fn ssrf_blocks_non_http_schemes() {
        assert!(validate_webhook_url("ftp://example.com/hook").is_err());
        assert!(validate_webhook_url("file:///etc/passwd").is_err());
        assert!(validate_webhook_url("gopher://example.com/").is_err());
    }

    #[test]
    fn ssrf_allows_public_urls() {
        assert!(validate_webhook_url("https://example.com/webhook").is_ok());
        assert!(validate_webhook_url("https://hooks.slack.com/services/T00/B00/xxx").is_ok());
        assert!(validate_webhook_url("http://203.0.114.1/hook").is_ok());
    }

    #[test]
    fn is_public_ip_checks() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))));
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(203, 0, 114, 1))));
    }

    #[test]
    fn ssrf_blocks_cgnat_and_benchmarking() {
        use std::net::{IpAddr, Ipv4Addr};
        // CGNAT / Shared Address Space (RFC 6598): 100.64.0.0/10
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254))));
        // Just outside CGNAT range
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));

        // Benchmarking (RFC 2544): 198.18.0.0/15
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(198, 19, 255, 254))));
        // Just outside benchmarking range
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(198, 20, 0, 1))));
    }

    #[test]
    fn ssrf_blocks_non_standard_ports() {
        // Standard ports allowed (no explicit port = default)
        assert!(validate_webhook_url("https://example.com/hook").is_ok());
        assert!(validate_webhook_url("http://example.com/hook").is_ok());
        assert!(validate_webhook_url("https://example.com:443/hook").is_ok());
        assert!(validate_webhook_url("http://example.com:80/hook").is_ok());
        // Common webhook ports allowed
        assert!(validate_webhook_url("https://example.com:8080/hook").is_ok());
        assert!(validate_webhook_url("https://example.com:8443/hook").is_ok());
        // Non-standard ports blocked
        assert!(validate_webhook_url("https://example.com:22/hook").is_err());
        assert!(validate_webhook_url("https://example.com:3306/hook").is_err());
        assert!(validate_webhook_url("https://example.com:6379/hook").is_err());
        assert!(validate_webhook_url("https://example.com:9999/hook").is_err());
    }

    #[test]
    fn sanitize_strips_control_chars() {
        let dirty = "ok\x00hidden\x07bell\nnewline\ttab";
        let clean: String = dirty
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
            .collect();
        assert_eq!(clean, "okhiddenbell\nnewline\ttab");
    }

    #[test]
    fn each_encryption_produces_different_ciphertext() {
        let key = [0x42u8; 32];
        let secret = "same-secret";
        let enc1 = encrypt_with_key(&key, secret).unwrap();
        let enc2 = encrypt_with_key(&key, secret).unwrap();
        // Different random nonces produce different ciphertexts
        assert_ne!(enc1, enc2);
        // Both decrypt to the same value
        assert_eq!(decrypt_with_key(&key, &enc1).unwrap(), secret);
        assert_eq!(decrypt_with_key(&key, &enc2).unwrap(), secret);
    }
}
