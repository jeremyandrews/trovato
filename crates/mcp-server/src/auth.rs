//! API token authentication for the MCP server.
//!
//! Resolves a raw API token string to a [`User`] via the kernel's one token
//! resolver, [`trovato_kernel::services::api_token::resolve`]. The resolved user identity is used for all
//! subsequent tool and resource calls.

use anyhow::{Context, Result};
use trovato_kernel::models::User;
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

/// Resolve a raw API token to a fully loaded [`User`].
///
/// Returns an error if the token is invalid, expired, or the associated
/// user account is not active.
pub async fn resolve_token(state: &AppState, raw_token: &str) -> Result<User> {
    // The kernel's one resolver, the one the HTTP middleware uses: the same
    // tokens are good here as there, and a use is recorded the same way.
    trovato_kernel::services::api_token::resolve(state, raw_token)
        .await?
        .map(|holder| holder.user)
        .ok_or_else(|| anyhow::anyhow!("invalid or expired API token"))
}

/// Build a [`UserContext`] from a [`User`] by loading their permissions.
///
/// The returned context includes all role-based permissions from the database,
/// and nothing else. An administrator carries the `users.is_admin` column on
/// the context itself, so [`UserContext::is_admin`] holds without the
/// permission set being altered to say so.
///
/// Delegates to the kernel's one context builder rather than assembling the
/// permission list here, so an MCP caller and a web request cannot drift into
/// two different notions of the same user's permissions. This path **fails
/// closed** on a permission-load error, which a token API can afford: it has one
/// error channel and no page to degrade.
pub async fn build_user_context(state: &AppState, user: &User) -> Result<UserContext> {
    state
        .permissions()
        .user_context(user)
        .await
        .context("failed to load user permissions")
}
