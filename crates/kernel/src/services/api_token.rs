//! The one API token resolver.
//!
//! A raw bearer token is turned into the account it acts as here and nowhere
//! else: the HTTP middleware ([`crate::middleware::api_token`]) and the MCP
//! server both call [`resolve`], so the two cannot come to disagree about
//! which tokens are good (expiry, a blocked account, a deleted one) or about
//! recording a token's use.
//!
//! Issuing and revoking a token go through here too ([`create`], [`revoke`],
//! [`revoke_all_for_user`]), so every path that does either writes the same
//! `api_token.created` or `api_token.revoked` security event.

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::audit::{SecurityEvent, SecurityEventKind};
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

/// Issue `user_id` a token named `name`, and record `api_token.created`.
///
/// Returns the stored token and the raw value, which exists only here: the
/// caller shows it once. The event names the token by its hashed id, never
/// the value.
///
/// The token and its event are written by one statement, so they persist
/// together or not at all.
///
/// # Errors
///
/// When the insert or the event fails; then no token exists and nothing is
/// recorded.
pub async fn create(
    state: &AppState,
    user_id: Uuid,
    name: &str,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(ApiToken, String)> {
    // The id is drawn here, so the event naming it can be part of the insert.
    let id = Uuid::now_v7();
    let event = SecurityEvent::new(SecurityEventKind::ApiTokenCreated)
        .user(user_id)
        .subject(&id.to_string())
        // Not the label: it is free text its holder typed, and may hold
        // anything, an address or a secret included.
        .detail("expires", expires_at.is_some());
    match state.audit_trail().event_row(event) {
        Some(row) => {
            ApiToken::create_recording(state.db(), id, user_id, name, expires_at, &row).await
        }
        None => ApiToken::create(state.db(), user_id, name, expires_at).await,
    }
}

/// Revoke `user_id`'s token `id`, at its holder's request, and record
/// `api_token.revoked`. Returns whether there was such a token.
///
/// The delete and its event commit together.
///
/// # Errors
///
/// When the delete or the event fails; then the token remains and nothing is
/// recorded.
pub async fn revoke(state: &AppState, id: Uuid, user_id: Uuid) -> Result<bool> {
    let mut tx = state.db().begin().await?;
    let deleted = ApiToken::delete(&mut *tx, id, user_id).await?;
    if deleted {
        state
            .audit_trail()
            .security_in(&mut tx, revoked_event(user_id, id, None, "self"))
            .await?;
    }
    tx.commit().await?;
    Ok(deleted)
}

/// Revoke every token `user_id` holds, recording one `api_token.revoked`
/// per token (`actor` did it, for `reason`). Returns how many went.
///
/// The deletes and their events commit together.
///
/// # Errors
///
/// When the delete or an event fails; then the tokens remain and nothing is
/// recorded.
pub async fn revoke_all_for_user(
    state: &AppState,
    user_id: Uuid,
    actor: Option<Uuid>,
    reason: &str,
) -> Result<usize> {
    let mut tx = state.db().begin().await?;
    let ids = ApiToken::delete_all_for_user(&mut *tx, user_id).await?;
    state
        .audit_trail()
        .security_all_in(
            &mut tx,
            ids.iter()
                .map(|id| revoked_event(user_id, *id, actor, reason))
                .collect::<Vec<_>>(),
        )
        .await?;
    tx.commit().await?;
    Ok(ids.len())
}

fn revoked_event(
    user_id: Uuid,
    token_id: Uuid,
    actor: Option<Uuid>,
    reason: &str,
) -> SecurityEvent {
    SecurityEvent::new(SecurityEventKind::ApiTokenRevoked)
        .user(user_id)
        .subject(&token_id.to_string())
        .maybe_actor(actor.filter(|a| *a != user_id))
        .detail("reason", reason)
}
