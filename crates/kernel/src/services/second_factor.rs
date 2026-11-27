//! Whether an account is subject to, and satisfies, a second-factor
//! requirement (B304).
//!
//! A requirement comes from two places: a role marked
//! `requires_second_factor`, or (for an administrator) the site setting
//! [`ADMIN_REQUIRES_SECOND_FACTOR_KEY`]. It is satisfied by a passkey or a
//! confirmed TOTP enrolment — either one; the kernel never asks for both.
//! [`crate::routes::auth::setup_session`] is the one place that decides
//! whether a freshly-authenticated account must be sent to enrol, using
//! [`needs_enrolment`] below.

use uuid::Uuid;

use crate::models::user_totp::UserTotp;
use crate::models::{Role, SiteConfig, User, WebauthnCredential};
use crate::state::AppState;

/// Site-config key: "Administrators require a second factor" (default off).
pub const ADMIN_REQUIRES_SECOND_FACTOR_KEY: &str = "admin_requires_second_factor";

/// Whether the site requires administrators to have a second factor.
pub async fn admin_requirement_enabled(state: &AppState) -> bool {
    SiteConfig::get(state.db(), ADMIN_REQUIRES_SECOND_FACTOR_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Whether `user` is subject to a second-factor requirement at all: holds a
/// role marked `requires_second_factor`, or is an administrator while the
/// site's administrator setting is on.
pub async fn account_subject_to_requirement(state: &AppState, user: &User) -> bool {
    if user.is_admin && admin_requirement_enabled(state).await {
        return true;
    }
    Role::user_holds_second_factor_role(state.db(), user.id)
        .await
        .unwrap_or(false)
}

/// Whether `user_id` currently has at least one registered passkey.
pub async fn has_passkey(state: &AppState, user_id: Uuid) -> bool {
    WebauthnCredential::count_for_user(state.db(), user_id)
        .await
        .unwrap_or(0)
        > 0
}

/// Whether `user_id` has a confirmed TOTP enrolment.
pub async fn has_confirmed_totp(state: &AppState, user_id: Uuid) -> bool {
    UserTotp::is_confirmed(state.db(), user_id).await
}

/// Whether `user` must be sent to enrol a second factor: subject to the
/// requirement, and currently has neither a passkey nor a confirmed TOTP
/// enrolment. The one question [`crate::routes::auth::setup_session`] asks to
/// decide the session's `SESSION_ENROLLMENT_REQUIRED` flag.
pub async fn needs_enrolment(state: &AppState, user: &User) -> bool {
    if !account_subject_to_requirement(state, user).await {
        return false;
    }
    !has_passkey(state, user.id).await && !has_confirmed_totp(state, user.id).await
}

/// Whether a **password** login must be refused because the account is
/// subject to the requirement, already satisfies it with a passkey, and has
/// no TOTP: such an account "signs in with its passkey" and a password login
/// would otherwise hand it a session with neither factor actually checked.
/// Passkey and recovery-completed logins are unaffected: this is a
/// password-route-specific refusal, checked in `attempt_login` before any
/// session exists.
pub async fn password_login_blocked_by_passkey_only(state: &AppState, user: &User) -> bool {
    account_subject_to_requirement(state, user).await
        && has_passkey(state, user.id).await
        && !has_confirmed_totp(state, user.id).await
}

/// The message [`password_login_blocked_by_passkey_only`] refuses with.
pub const PASSKEY_ONLY_MESSAGE: &str = "This account signs in with its passkey.";
