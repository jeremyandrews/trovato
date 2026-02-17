//! OAuth2 provider service.
//!
//! JWT token signing/verification, client validation, authorization code
//! generation with PKCE (RFC 7636), and token endpoint grant handling.

use anyhow::{Context, Result};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

/// JWT issuer claim value.
const ISSUER: &str = "trovato";

/// Sentinel UUID for client_credentials grant tokens.
///
/// Client credentials tokens have no real user context. We use a dedicated
/// sentinel UUID (not `Uuid::nil()`) to avoid colliding with `ANONYMOUS_USER_ID`
/// which is `Uuid::nil()`. Downstream code can check `bearer_auth.is_client_credentials`
/// to distinguish machine-to-machine tokens from user tokens.
pub const CLIENT_CREDENTIALS_USER_ID: Uuid = Uuid::from_u128(1);

/// JWT token claims.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenClaims {
    /// Issuer.
    pub iss: String,
    /// Subject (user ID, or client_id for client_credentials).
    pub sub: String,
    /// Audience (client ID).
    pub aud: String,
    /// Issued at (Unix timestamp).
    pub iat: i64,
    /// Expiration (Unix timestamp).
    pub exp: i64,
    /// JWT ID (unique per token, for revocation).
    pub jti: String,
    /// Client ID that requested the token.
    pub client_id: String,
    /// Scopes granted.
    pub scope: String,
    /// Token type: "access" or "refresh".
    #[serde(default = "default_token_type")]
    pub token_type: String,
}

fn default_token_type() -> String {
    "access".to_string()
}

/// OAuth2 client record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct OAuthClient {
    pub id: Uuid,
    pub client_id: String,
    pub client_secret_hash: String,
    pub name: String,
    pub redirect_uris: serde_json::Value,
    pub grant_types: serde_json::Value,
    /// Allowed scopes. Empty array means all scopes are permitted.
    #[serde(default)]
    #[sqlx(default)]
    pub scopes: serde_json::Value,
    pub created: i64,
}

impl OAuthClient {
    /// Check if this client is confidential (has a secret set).
    pub fn is_confidential(&self) -> bool {
        !self.client_secret_hash.is_empty()
    }

    /// Check if this client supports a given grant type.
    pub fn supports_grant_type(&self, grant_type: &str) -> bool {
        let grant_types: Vec<String> =
            serde_json::from_value(self.grant_types.clone()).unwrap_or_default();
        grant_types.iter().any(|g| g == grant_type)
    }

    /// Filter a requested scope string to only include scopes this client is allowed.
    ///
    /// If the client has no scope restrictions (empty `scopes` array), the
    /// requested scope is returned as-is. Otherwise, only the intersection of
    /// requested and allowed scopes is returned.
    pub fn filter_scope(&self, requested: &str) -> String {
        let allowed: Vec<String> = serde_json::from_value(self.scopes.clone()).unwrap_or_default();
        if allowed.is_empty() {
            return requested.to_string();
        }
        requested
            .split_whitespace()
            .filter(|s| allowed.iter().any(|a| a == s))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Token response for the /oauth/token endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub refresh_token: Option<String>,
}

/// Default access token lifetime in seconds (1 hour).
const ACCESS_TOKEN_LIFETIME: i64 = 3600;

/// Default refresh token lifetime in seconds (30 days).
const REFRESH_TOKEN_LIFETIME: i64 = 30 * 86400;

/// Authorization code lifetime in seconds (60 seconds per RFC 6749 §4.1.2).
const AUTH_CODE_LIFETIME: u64 = 60;

/// Data stored in Redis for an authorization code.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuthCodeData {
    pub user_id: String,
    pub client_id: String,
    pub scope: String,
    pub redirect_uri: String,
    /// PKCE code challenge (RFC 7636). None if PKCE not used.
    #[serde(default)]
    pub code_challenge: Option<String>,
    /// PKCE code challenge method ("S256" or "plain").
    #[serde(default)]
    pub code_challenge_method: Option<String>,
}

/// OAuth2 service.
#[derive(Clone)]
pub struct OAuthService {
    pool: PgPool,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl OAuthService {
    /// Create a new OAuth service with HMAC-SHA256 signing.
    ///
    /// The secret should be loaded from environment configuration
    /// and must be at least 32 bytes.
    pub fn new(pool: PgPool, jwt_secret: &[u8]) -> Self {
        Self {
            pool,
            encoding_key: EncodingKey::from_secret(jwt_secret),
            decoding_key: DecodingKey::from_secret(jwt_secret),
        }
    }

    /// Create an access token (and optionally a refresh token) for a user.
    ///
    /// Set `include_refresh` to `false` for grants that should not receive
    /// refresh tokens (e.g., `client_credentials` per RFC 6749 §4.4.3).
    pub fn create_access_token(
        &self,
        user_id: Uuid,
        client_id: &str,
        scope: &str,
        include_refresh: bool,
    ) -> Result<TokenResponse> {
        let now = chrono::Utc::now().timestamp();
        let jti = Uuid::now_v7().to_string();

        let claims = TokenClaims {
            iss: ISSUER.to_string(),
            sub: user_id.to_string(),
            aud: client_id.to_string(),
            iat: now,
            exp: now + ACCESS_TOKEN_LIFETIME,
            jti,
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            token_type: "access".to_string(),
        };

        let header = Header::new(Algorithm::HS256);
        let access_token = jsonwebtoken::encode(&header, &claims, &self.encoding_key)
            .context("failed to encode access token")?;

        let refresh_token = if include_refresh {
            let refresh_jti = Uuid::now_v7().to_string();
            let refresh_claims = TokenClaims {
                iss: ISSUER.to_string(),
                sub: user_id.to_string(),
                aud: client_id.to_string(),
                iat: now,
                exp: now + REFRESH_TOKEN_LIFETIME,
                jti: refresh_jti,
                client_id: client_id.to_string(),
                scope: scope.to_string(),
                token_type: "refresh".to_string(),
            };

            Some(
                jsonwebtoken::encode(&header, &refresh_claims, &self.encoding_key)
                    .context("failed to encode refresh token")?,
            )
        } else {
            None
        };

        Ok(TokenResponse {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: ACCESS_TOKEN_LIFETIME,
            refresh_token,
        })
    }

    /// Verify a JWT token and return claims.
    ///
    /// Validates the `iss` claim but not `aud` (the generic bearer auth
    /// middleware doesn't know the expected audience).
    pub fn verify_token(&self, token: &str) -> Result<TokenClaims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        validation.validate_aud = false;

        let data = jsonwebtoken::decode::<TokenClaims>(token, &self.decoding_key, &validation)
            .context("invalid token")?;

        Ok(data.claims)
    }

    /// Verify a JWT and require it to be an access token.
    pub fn verify_access_token(&self, token: &str) -> Result<TokenClaims> {
        let claims = self.verify_token(token)?;
        if claims.token_type != "access" {
            anyhow::bail!("expected access token, got {}", claims.token_type);
        }
        Ok(claims)
    }

    /// Verify a JWT and require it to be a refresh token.
    pub fn verify_refresh_token(&self, token: &str) -> Result<TokenClaims> {
        let claims = self.verify_token(token)?;
        if claims.token_type != "refresh" {
            anyhow::bail!("expected refresh token, got {}", claims.token_type);
        }
        Ok(claims)
    }

    /// Find a client by client_id.
    pub async fn find_client(&self, client_id: &str) -> Result<Option<OAuthClient>> {
        let client = sqlx::query_as::<_, OAuthClient>(
            r#"
            SELECT id, client_id, client_secret_hash, name, redirect_uris, grant_types,
                   COALESCE(scopes, '[]'::jsonb) AS scopes, created
            FROM oauth_client
            WHERE client_id = $1
            "#,
        )
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await;

        match client {
            Ok(c) => Ok(c),
            Err(e) => {
                debug!(error = %e, "oauth_client table may not exist yet");
                Ok(None)
            }
        }
    }

    /// Validate client credentials (for confidential clients).
    ///
    /// Returns the client on success, None on authentication failure.
    pub async fn validate_client_credentials(
        &self,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Option<OAuthClient>> {
        let client = self.find_client(client_id).await?;

        let Some(client) = client else {
            return Ok(None);
        };

        // Verify secret using argon2
        let hash = argon2::PasswordHash::new(&client.client_secret_hash);
        match hash {
            Ok(hash) => {
                use argon2::PasswordVerifier;
                if argon2::Argon2::default()
                    .verify_password(client_secret.as_bytes(), &hash)
                    .is_ok()
                {
                    Ok(Some(client))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }

    /// Authenticate a client for the token endpoint.
    ///
    /// For confidential clients (those with a secret hash set), the
    /// `client_secret` must be provided and valid.
    /// For public clients, authentication is skipped (PKCE provides security).
    pub async fn authenticate_client(
        &self,
        client_id: &str,
        client_secret: &str,
    ) -> Result<OAuthClient> {
        let client = self
            .find_client(client_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown client"))?;

        if client.is_confidential() {
            // Confidential client: must provide valid secret
            if client_secret.is_empty() {
                anyhow::bail!("client_secret required for confidential client");
            }
            let valid = self
                .validate_client_credentials(client_id, client_secret)
                .await?;
            if valid.is_none() {
                anyhow::bail!("invalid client credentials");
            }
        }

        Ok(client)
    }

    /// Create an opaque authorization code stored in Redis.
    ///
    /// Per RFC 6749 §4.1.2, authorization codes are short-lived (60s)
    /// and MUST be single-use. The code is an opaque random UUID; actual
    /// grant data is stored server-side in Redis.
    ///
    /// PKCE parameters (RFC 7636) are stored alongside the code data
    /// for verification at exchange time.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_authorization_code(
        &self,
        user_id: Uuid,
        client_id: &str,
        scope: &str,
        redirect_uri: &str,
        code_challenge: Option<&str>,
        code_challenge_method: Option<&str>,
        redis_client: &redis::Client,
    ) -> Result<String> {
        // Use UUID v4 (122 bits of randomness) rather than v7 (which embeds a
        // predictable timestamp) per RFC 6749 §10.10 guidance on code entropy.
        let code = Uuid::new_v4().to_string();
        let data = AuthCodeData {
            user_id: user_id.to_string(),
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            redirect_uri: redirect_uri.to_string(),
            code_challenge: code_challenge.map(|s| s.to_string()),
            code_challenge_method: code_challenge_method.map(|s| s.to_string()),
        };
        let json = serde_json::to_string(&data).context("failed to serialize auth code data")?;

        let mut conn = redis_client
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        redis::cmd("SET")
            .arg(format!("oauth:code:{code}"))
            .arg(&json)
            .arg("EX")
            .arg(AUTH_CODE_LIFETIME)
            .query_async::<()>(&mut conn)
            .await
            .context("failed to store authorization code")?;

        debug!(client_id = %client_id, "authorization code created");
        Ok(code)
    }

    /// Exchange an authorization code for an access token.
    ///
    /// Per RFC 6749 §4.1.2, the code MUST be used at most once. We use
    /// Redis GETDEL for atomic single-use: if the key is gone, the code
    /// was already consumed or expired.
    ///
    /// Validates:
    /// - `client_id` and `redirect_uri` match the original request (RFC 6749 §4.1.3)
    /// - Confidential clients must authenticate with `client_secret`
    /// - PKCE `code_verifier` if a challenge was provided (RFC 7636)
    /// - Public clients MUST have used PKCE
    pub async fn exchange_authorization_code(
        &self,
        code: &str,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        code_verifier: &str,
        redis_client: &redis::Client,
    ) -> Result<TokenResponse> {
        let mut conn = redis_client
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        // Atomic consume — GETDEL returns the value and deletes the key
        let json: Option<String> = redis::cmd("GETDEL")
            .arg(format!("oauth:code:{code}"))
            .query_async(&mut conn)
            .await
            .context("failed to consume authorization code")?;

        let json = json.ok_or_else(|| {
            anyhow::anyhow!("authorization code is invalid, expired, or already used")
        })?;

        let data: AuthCodeData =
            serde_json::from_str(&json).context("failed to parse auth code data")?;

        // Validate client_id matches (RFC 6749 §4.1.3)
        if data.client_id != client_id {
            anyhow::bail!("client_id mismatch");
        }

        // Validate redirect_uri matches if it was provided in the original request
        if !data.redirect_uri.is_empty() && data.redirect_uri != redirect_uri {
            anyhow::bail!("redirect_uri mismatch");
        }

        // Authenticate the client (confidential clients must provide valid secret)
        let client = self.authenticate_client(client_id, client_secret).await?;

        // PKCE verification (RFC 7636)
        if let Some(ref challenge) = data.code_challenge {
            let method = data.code_challenge_method.as_deref().unwrap_or("S256");
            if code_verifier.is_empty() {
                anyhow::bail!("code_verifier required when PKCE was used");
            }
            if !verify_pkce(challenge, method, code_verifier) {
                anyhow::bail!("PKCE verification failed");
            }
        } else if !client.is_confidential() {
            // Public clients MUST use PKCE (per OAuth 2.1 / best practice)
            anyhow::bail!("PKCE required for public clients");
        }

        // Validate grant_type is allowed for this client
        if !client.supports_grant_type("authorization_code") {
            anyhow::bail!("client is not authorized for authorization_code grant");
        }

        // Filter scope against client's allowed scopes at exchange time.
        // This prevents scope escalation even if the auth code stored an unfiltered scope.
        let granted_scope = client.filter_scope(&data.scope);

        let user_id: Uuid = data
            .user_id
            .parse()
            .context("invalid user_id in authorization code")?;
        self.create_access_token(user_id, &data.client_id, &granted_scope, true)
    }

    /// Validate that a JTI is a well-formed UUID.
    ///
    /// JTIs are generated as `Uuid::now_v7().to_string()`. Validating format
    /// prevents arbitrary strings from being used as Redis keys.
    fn validate_jti(jti: &str) -> Result<()> {
        if jti.parse::<Uuid>().is_err() {
            anyhow::bail!("malformed JTI: not a valid UUID");
        }
        Ok(())
    }

    /// Check if a token's JTI has been revoked (via Redis blocklist).
    pub async fn is_revoked(&self, jti: &str, redis: &redis::Client) -> Result<bool> {
        Self::validate_jti(jti)?;

        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let exists: bool = redis::cmd("EXISTS")
            .arg(format!("oauth:revoked:{jti}"))
            .query_async(&mut conn)
            .await
            .context("failed to check revocation")?;

        Ok(exists)
    }

    /// Revoke a token by adding its JTI to the Redis blocklist.
    pub async fn revoke_token(
        &self,
        jti: &str,
        ttl_secs: u64,
        redis: &redis::Client,
    ) -> Result<()> {
        Self::validate_jti(jti)?;

        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        redis::cmd("SET")
            .arg(format!("oauth:revoked:{jti}"))
            .arg("1")
            .arg("EX")
            .arg(ttl_secs)
            .query_async::<()>(&mut conn)
            .await
            .context("failed to revoke token")?;

        debug!(jti = %jti, "token revoked");
        Ok(())
    }
}

/// Verify a PKCE code challenge against a code verifier (RFC 7636).
///
/// Only the `S256` method is accepted. The `plain` method is rejected because
/// it provides no security benefit and leaks length information through
/// constant-time comparison on variable-length inputs. RFC 7636 §4.2
/// recommends S256 as the default; we enforce it as the only option.
///
/// Uses constant-time comparison to prevent timing side-channel attacks.
pub fn verify_pkce(code_challenge: &str, method: &str, code_verifier: &str) -> bool {
    use subtle::ConstantTimeEq;

    match method {
        "S256" => {
            use base64::Engine;
            use sha2::{Digest, Sha256};

            let digest = Sha256::digest(code_verifier.as_bytes());
            let computed =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_slice());
            computed.as_bytes().ct_eq(code_challenge.as_bytes()).into()
        }
        _ => false, // Only S256 is accepted
    }
}

impl std::fmt::Debug for OAuthService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthService").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a JWT and verify it using the jsonwebtoken crate directly,
    /// avoiding the need for a PgPool instance.
    fn create_and_verify(
        secret: &[u8],
        user_id: Uuid,
        client_id: &str,
        scope: &str,
    ) -> TokenClaims {
        let encoding_key = EncodingKey::from_secret(secret);
        let decoding_key = DecodingKey::from_secret(secret);

        let now = chrono::Utc::now().timestamp();
        let claims = TokenClaims {
            iss: ISSUER.to_string(),
            sub: user_id.to_string(),
            aud: client_id.to_string(),
            iat: now,
            exp: now + ACCESS_TOKEN_LIFETIME,
            jti: Uuid::now_v7().to_string(),
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            token_type: "access".to_string(),
        };

        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
            .expect("encoding failed");

        // Validate with same settings as production verify_token
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        validation.validate_aud = false;

        let decoded = jsonwebtoken::decode::<TokenClaims>(&token, &decoding_key, &validation)
            .expect("decoding failed");

        decoded.claims
    }

    #[test]
    fn token_roundtrip() {
        let secret = b"test-secret-key-at-least-32-bytes-long!!";
        let user_id = Uuid::now_v7();
        let claims = create_and_verify(secret, user_id, "test-client", "read write");

        assert_eq!(claims.sub, user_id.to_string());
        assert_eq!(claims.iss, ISSUER);
        assert_eq!(claims.aud, "test-client");
        assert_eq!(claims.client_id, "test-client");
        assert_eq!(claims.scope, "read write");
    }

    #[test]
    fn client_credentials_user_id_not_anonymous() {
        // CLIENT_CREDENTIALS_USER_ID must not collide with ANONYMOUS_USER_ID (Uuid::nil)
        assert_ne!(CLIENT_CREDENTIALS_USER_ID, Uuid::nil());
    }

    #[test]
    fn wrong_issuer_rejected() {
        let secret = b"test-secret-key-at-least-32-bytes-long!!";
        let encoding_key = EncodingKey::from_secret(secret);
        let decoding_key = DecodingKey::from_secret(secret);

        let now = chrono::Utc::now().timestamp();
        let claims = TokenClaims {
            iss: "wrong-issuer".to_string(),
            sub: Uuid::nil().to_string(),
            aud: "test".to_string(),
            iat: now,
            exp: now + 3600,
            jti: "jti".to_string(),
            client_id: "test".to_string(),
            scope: "".to_string(),
            token_type: "access".to_string(),
        };

        let token =
            jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key).unwrap();

        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        validation.validate_aud = false;

        let result = jsonwebtoken::decode::<TokenClaims>(&token, &decoding_key, &validation);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_token_rejected() {
        let secret = b"test-secret-key-at-least-32-bytes-long!!";
        let decoding_key = DecodingKey::from_secret(secret);
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        validation.validate_aud = false;

        let result =
            jsonwebtoken::decode::<TokenClaims>("invalid.jwt.token", &decoding_key, &validation);
        assert!(result.is_err());
    }

    #[test]
    fn token_response_serialization() {
        let response = TokenResponse {
            access_token: "abc".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: 3600,
            refresh_token: Some("def".to_string()),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("access_token"));
        assert!(json.contains("Bearer"));
    }

    #[test]
    fn pkce_s256_verification() {
        use base64::Engine;
        use sha2::{Digest, Sha256};

        // code_verifier is a high-entropy random string
        let code_verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

        // code_challenge = BASE64URL(SHA256(code_verifier))
        let digest = Sha256::digest(code_verifier.as_bytes());
        let code_challenge =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_slice());

        assert!(verify_pkce(&code_challenge, "S256", code_verifier));
        assert!(!verify_pkce(&code_challenge, "S256", "wrong-verifier"));
    }

    #[test]
    fn pkce_plain_method_rejected() {
        // plain method is no longer accepted — only S256
        let verifier = "my-code-verifier";
        assert!(!verify_pkce(verifier, "plain", verifier));
    }

    #[test]
    fn pkce_unknown_method_rejected() {
        assert!(!verify_pkce("challenge", "unknown", "verifier"));
    }

    #[test]
    fn client_scope_filtering() {
        let restricted = OAuthClient {
            id: Uuid::nil(),
            client_id: "test".to_string(),
            client_secret_hash: "".to_string(),
            name: "Test".to_string(),
            redirect_uris: serde_json::json!([]),
            grant_types: serde_json::json!([]),
            scopes: serde_json::json!(["read", "write"]),
            created: 0,
        };
        // Intersects requested with allowed
        assert_eq!(restricted.filter_scope("read write admin"), "read write");
        assert_eq!(restricted.filter_scope("admin delete"), "");
        assert_eq!(restricted.filter_scope("read"), "read");

        let unrestricted = OAuthClient {
            id: Uuid::nil(),
            client_id: "test".to_string(),
            client_secret_hash: "".to_string(),
            name: "Test".to_string(),
            redirect_uris: serde_json::json!([]),
            grant_types: serde_json::json!([]),
            scopes: serde_json::json!([]),
            created: 0,
        };
        // Empty scopes = no restriction
        assert_eq!(
            unrestricted.filter_scope("read write admin"),
            "read write admin"
        );
    }

    #[test]
    fn jti_validation() {
        // Valid UUIDs
        assert!(OAuthService::validate_jti("00000000-0000-0000-0000-000000000000").is_ok());
        assert!(OAuthService::validate_jti(&Uuid::now_v7().to_string()).is_ok());

        // Invalid JTIs
        assert!(OAuthService::validate_jti("not-a-uuid").is_err());
        assert!(OAuthService::validate_jti("").is_err());
        assert!(OAuthService::validate_jti("../../etc/passwd").is_err());
        assert!(OAuthService::validate_jti("key\ninjection").is_err());
    }

    #[test]
    fn client_grant_type_check() {
        let client = OAuthClient {
            id: Uuid::nil(),
            client_id: "test".to_string(),
            client_secret_hash: "".to_string(),
            name: "Test".to_string(),
            redirect_uris: serde_json::json!([]),
            grant_types: serde_json::json!(["authorization_code", "refresh_token"]),
            scopes: serde_json::json!([]),
            created: 0,
        };

        assert!(client.supports_grant_type("authorization_code"));
        assert!(client.supports_grant_type("refresh_token"));
        assert!(!client.supports_grant_type("client_credentials"));
    }

    #[test]
    fn client_confidential_check() {
        let public = OAuthClient {
            id: Uuid::nil(),
            client_id: "pub".to_string(),
            client_secret_hash: "".to_string(),
            name: "Public".to_string(),
            redirect_uris: serde_json::json!([]),
            grant_types: serde_json::json!([]),
            scopes: serde_json::json!([]),
            created: 0,
        };
        assert!(!public.is_confidential());

        let confidential = OAuthClient {
            id: Uuid::nil(),
            client_id: "conf".to_string(),
            client_secret_hash: "$argon2id$v=19$...".to_string(),
            name: "Confidential".to_string(),
            redirect_uris: serde_json::json!([]),
            grant_types: serde_json::json!([]),
            scopes: serde_json::json!([]),
            created: 0,
        };
        assert!(confidential.is_confidential());
    }
}
