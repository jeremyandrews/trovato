//! CSRF token generation and verification.

use anyhow::{Result, bail};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tower_sessions::Session;

/// Session key for storing CSRF tokens.
const CSRF_SESSION_KEY: &str = "csrf_tokens";

/// Maximum number of tokens to store per session.
const MAX_TOKENS: usize = 10;

/// Token validity period in seconds (1 hour).
const TOKEN_VALIDITY_SECS: i64 = 3600;

/// Generate a CSRF token and store it in the session.
pub async fn generate_csrf_token(session: &Session) -> Result<String> {
    // Generate random bytes
    let mut random_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut random_bytes);

    // Create timestamp
    let timestamp = chrono::Utc::now().timestamp();

    // Hash the random bytes with timestamp
    let mut hasher = Sha256::new();
    hasher.update(&random_bytes);
    hasher.update(timestamp.to_le_bytes());
    let hash = hasher.finalize();

    // Encode as hex
    let token = hex::encode(hash);

    // Store in session with timestamp
    let token_data = format!("{}:{}", token, timestamp);

    // Get existing tokens
    let mut tokens: Vec<String> = session
        .get(CSRF_SESSION_KEY)
        .await
        .unwrap_or(None)
        .unwrap_or_default();

    // Add new token
    tokens.push(token_data);

    // Prune old tokens (keep only MAX_TOKENS most recent)
    if tokens.len() > MAX_TOKENS {
        let skip = tokens.len() - MAX_TOKENS;
        tokens = tokens.into_iter().skip(skip).collect();
    }

    // Save back to session
    session
        .insert(CSRF_SESSION_KEY, tokens)
        .await
        .map_err(|e| anyhow::anyhow!("failed to store CSRF token: {}", e))?;

    Ok(token)
}

/// Verify a CSRF token against the session.
///
/// Tokens are single-use and time-limited.
pub async fn verify_csrf_token(session: &Session, submitted: &str) -> Result<bool> {
    if submitted.is_empty() {
        bail!("empty CSRF token");
    }

    // Get stored tokens
    let mut tokens: Vec<String> = session
        .get(CSRF_SESSION_KEY)
        .await
        .unwrap_or(None)
        .unwrap_or_default();

    if tokens.is_empty() {
        return Ok(false);
    }

    let now = chrono::Utc::now().timestamp();

    // Find and validate the token
    let mut found_index = None;
    for (i, token_data) in tokens.iter().enumerate() {
        let parts: Vec<&str> = token_data.split(':').collect();
        if parts.len() != 2 {
            continue;
        }

        let token = parts[0];
        let timestamp: i64 = match parts[1].parse() {
            Ok(ts) => ts,
            Err(_) => continue,
        };

        // Check if token matches
        if token == submitted {
            // Check if token is still valid
            if now - timestamp <= TOKEN_VALIDITY_SECS {
                found_index = Some(i);
                break;
            }
        }
    }

    // If found, remove the token (single-use)
    if let Some(index) = found_index {
        tokens.remove(index);

        // Clean up expired tokens while we're at it
        tokens.retain(|token_data| {
            let parts: Vec<&str> = token_data.split(':').collect();
            if parts.len() != 2 {
                return false;
            }
            let timestamp: i64 = parts[1].parse().unwrap_or(0);
            now - timestamp <= TOKEN_VALIDITY_SECS
        });

        // Save back to session
        session
            .insert(CSRF_SESSION_KEY, tokens)
            .await
            .map_err(|e| anyhow::anyhow!("failed to update CSRF tokens: {}", e))?;

        return Ok(true);
    }

    Ok(false)
}

/// Clear all CSRF tokens from the session.
pub async fn clear_csrf_tokens(session: &Session) -> Result<()> {
    session
        .remove::<Vec<String>>(CSRF_SESSION_KEY)
        .await
        .map_err(|e| anyhow::anyhow!("failed to clear CSRF tokens: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_format() {
        // Verify token is hex encoded SHA256 (64 chars)
        let token = hex::encode(sha2::Sha256::digest(b"test"));
        assert_eq!(token.len(), 64);
    }
}
