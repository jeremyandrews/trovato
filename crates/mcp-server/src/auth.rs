//! API token authentication for the MCP server.
//!
//! Resolves a raw API token string to a [`User`] via the kernel's
//! [`ApiToken`] model. The resolved user identity is used for all
//! subsequent tool and resource calls.

use anyhow::{Context, Result, bail};
use trovato_kernel::models::User;
use trovato_kernel::models::api_token::ApiToken;
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

/// Resolve a raw API token to a fully loaded [`User`].
///
/// Returns an error if the token is invalid, expired, or the associated
/// user account is not active.
pub async fn resolve_token(state: &AppState, raw_token: &str) -> Result<User> {
    let api_token: ApiToken = ApiToken::find_by_token(state.db(), raw_token)
        .await
        .context("failed to verify API token")?
        .ok_or_else(|| anyhow::anyhow!("invalid or expired API token"))?;

    // Update last_used timestamp (best-effort)
    if let Err(e) = ApiToken::touch_last_used(state.db(), api_token.id).await {
        tracing::warn!(error = %e, "failed to update API token last_used");
    }

    let user = User::find_by_id(state.db(), api_token.user_id)
        .await
        .context("failed to load user for API token")?
        .ok_or_else(|| anyhow::anyhow!("user associated with API token not found"))?;

    if !user.is_active() {
        bail!("user account is not active");
    }

    Ok(user)
}

/// Build a [`UserContext`] from a [`User`] by loading their permissions.
///
/// The returned context includes all role-based permissions from the database.
/// Admin users additionally receive `"administer site"` so that
/// [`UserContext::is_admin`] returns `true`.
pub async fn build_user_context(state: &AppState, user: &User) -> Result<UserContext> {
    let perms_set = state
        .permissions()
        .load_user_permissions(user)
        .await
        .context("failed to load user permissions")?;

    let mut permissions: Vec<String> = perms_set.into_iter().collect();

    // Admin users need "administer site" for UserContext::is_admin() to work.
    // The permission service bypasses permission loading for admins, so this
    // permission may not be in their role-based set.
    if user.is_admin && !permissions.iter().any(|p| p == "administer site") {
        permissions.push("administer site".to_string());
    }

    Ok(UserContext::authenticated(user.id, permissions))
}
