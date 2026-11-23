//! The one API token resolver.
//!
//! A raw bearer token is turned into the account it acts as here and nowhere
//! else: the HTTP middleware ([`crate::middleware::api_token`]) and the MCP
//! server both call [`resolve`], so the two cannot come to disagree about
//! which tokens are good (expiry, a blocked account, a deleted one) or about
//! recording a token's use.

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::models::User;
use crate::models::api_token::ApiToken;
use crate::state::AppState;

/// A token that authenticates someone, and who.
#[derive(Debug, Clone)]
pub struct TokenHolder {
    /// The token's id, for the audit trail; never the token itself.
    pub token_id: Uuid,
    /// The active account the token acts as.
    pub user: User,
}

/// The account `raw_token` acts as, or `None` when it does not authenticate
/// anyone: unknown, expired, or belonging to an account that is missing or
/// not active.
///
/// Records the use (`last_used`) in the background on success, so a caller
/// never waits on that write.
///
/// # Errors
///
/// A database failure while looking the token or its account up.
pub async fn resolve(state: &AppState, raw_token: &str) -> Result<Option<TokenHolder>> {
    let Some(token) = ApiToken::find_by_token(state.db(), raw_token)
        .await
        .context("failed to verify API token")?
    else {
        return Ok(None);
    };
    let Some(user) = state
        .users()
        .find_by_id(token.user_id)
        .await
        .context("failed to load the API token's user")?
    else {
        return Ok(None);
    };
    if !user.is_active() {
        return Ok(None);
    }

    let pool = state.db().clone();
    let token_id = token.id;
    tokio::spawn(async move {
        if let Err(e) = ApiToken::touch_last_used(&pool, token_id).await {
            tracing::warn!(error = %e, "failed to update API token last_used");
        }
    });

    Ok(Some(TokenHolder { token_id, user }))
}
