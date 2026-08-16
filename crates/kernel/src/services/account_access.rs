//! The ≥1-active-recovery-path invariant (FR-7b/7c, design §2.2, D-33).
//!
//! An account under FR-7 is `{ password?, 0..N passkeys, 0..N recovery paths }`.
//! Every one of those is a *way in*, and the single job of this module is to make
//! sure a removal never takes the last one away. That is the whole reason
//! "passwordless" is safe to offer: an account cannot reach a locked-out shape,
//! by construction rather than by a fallback.
//!
//! # Why the check lives at removal time
//!
//! Removal is the only moment an account can transition *into* a locked-out
//! shape. Checking anywhere else (at login, say) would be too late — the account
//! would already be unreachable — and checking everywhere would be noise. So
//! there is exactly one gate, at the one transition that matters.
//!
//! # Why there is no silent password fallback
//!
//! D-33 is explicit: a passwordless account does not quietly regain password
//! login when its passkeys go away. A fallback would undo passwordless, which is
//! the property the user asked for. Instead the removal that *would* have
//! created the problem is refused, and the user is told what to configure first.
//!
//! # The asymmetry between the two removals
//!
//! They are deliberately not the same rule:
//!
//! - Removing a **passkey** only needs *some* way in to survive, and a set
//!   password counts. That keeps the common password+passkey case frictionless.
//! - Removing the **password** — going passwordless — needs a passkey *and* a
//!   **non-password** recovery path. A password cannot be its own recovery path,
//!   and a passkey-only account with no recovery path is one lost device away
//!   from permanent lockout. This is the deliberate friction at the passwordless
//!   boundary: an explicit, audited choice rather than a silent one.
//!
//! Story 4.3 enforces both against whatever paths exist; Story 4.6 supplies the
//! real recovery paths that `non_password_recovery_paths` counts.

use serde::Serialize;

/// How many **non-password** recovery paths are currently active for an account.
///
/// This is the one number the ≥1-path invariant depends on that neither the
/// password nor the credential table can answer. Story 4.3 introduced the call
/// site with a fail-safe zero; Story 4.6 fills it in for real by asking every
/// recovery provider — built-in and plugin — what it offers for this account
/// through the frozen `describe` op, and counting the methods reported
/// available.
///
/// Counting what `describe` actually reports, rather than a static list, is what
/// makes the invariant honest: a path that exists in configuration but is
/// unavailable for this particular account (no email address on file, no
/// recovery codes generated) does not count toward it.
pub async fn active_recovery_path_count(
    state: &crate::state::AppState,
    user: &crate::models::User,
) -> usize {
    crate::routes::recovery::active_recovery_path_count(state, user).await
}

/// A snapshot of every way into one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountAccess {
    /// The account has a usable password hash set.
    pub has_password: bool,
    /// How many passkeys are registered.
    pub passkey_count: usize,
    /// How many **non-password** recovery paths are active for the account
    /// (built-in email/codes, or a plugin-provided method). A password is
    /// deliberately not counted here, even though it satisfies the weaker
    /// passkey-removal rule.
    pub non_password_recovery_paths: usize,
}

/// Why a removal was refused, in terms the user can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RemovalBlocked {
    /// Removing this passkey would leave no way into the account at all.
    LastWayIn,
    /// Going passwordless requires at least one passkey to sign in with.
    PasswordlessNeedsAPasskey,
    /// Going passwordless requires a recovery path that is not the password.
    PasswordlessNeedsRecoveryPath,
}

impl RemovalBlocked {
    /// A message safe to show the user, naming what to do about it.
    pub fn message(self) -> &'static str {
        match self {
            Self::LastWayIn => {
                "This is the only way into your account. Set a password or add another passkey first."
            }
            Self::PasswordlessNeedsAPasskey => {
                "Register a passkey before removing your password, or you will not be able to sign in."
            }
            Self::PasswordlessNeedsRecoveryPath => {
                "Set up a recovery method before removing your password, so a lost device cannot lock you out."
            }
        }
    }

    /// A stable slug for the audit stream.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LastWayIn => "last_way_in",
            Self::PasswordlessNeedsAPasskey => "passwordless_needs_a_passkey",
            Self::PasswordlessNeedsRecoveryPath => "passwordless_needs_recovery_path",
        }
    }
}

impl AccountAccess {
    /// Total ways into the account right now.
    pub fn ways_in(&self) -> usize {
        usize::from(self.has_password) + self.passkey_count + self.non_password_recovery_paths
    }

    /// Whether removing one passkey is permitted (design §2.2).
    ///
    /// Allowed as long as *something* survives: another passkey, a set password,
    /// or an active recovery path.
    pub fn can_remove_passkey(&self) -> Result<(), RemovalBlocked> {
        let remaining_passkeys = self.passkey_count.saturating_sub(1);
        let survives =
            remaining_passkeys > 0 || self.has_password || self.non_password_recovery_paths > 0;
        if survives {
            Ok(())
        } else {
            Err(RemovalBlocked::LastWayIn)
        }
    }

    /// Whether removing the password — going passwordless — is permitted (D-33).
    ///
    /// Needs a passkey to sign in with *and* a non-password recovery path, so a
    /// lost device is recoverable. Both are required; neither substitutes for
    /// the other.
    pub fn can_remove_password(&self) -> Result<(), RemovalBlocked> {
        if self.passkey_count == 0 {
            return Err(RemovalBlocked::PasswordlessNeedsAPasskey);
        }
        if self.non_password_recovery_paths == 0 {
            return Err(RemovalBlocked::PasswordlessNeedsRecoveryPath);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn access(has_password: bool, passkeys: usize, recovery: usize) -> AccountAccess {
        AccountAccess {
            has_password,
            passkey_count: passkeys,
            non_password_recovery_paths: recovery,
        }
    }

    // ── Removing a passkey ──────────────────────────────────────────────────

    #[test]
    fn removing_one_of_several_passkeys_is_fine() {
        assert!(access(false, 2, 0).can_remove_passkey().is_ok());
    }

    #[test]
    fn removing_the_last_passkey_is_fine_when_a_password_remains() {
        // The common case: password + passkey. A set password satisfies the
        // weaker rule, so this stays frictionless.
        assert!(access(true, 1, 0).can_remove_passkey().is_ok());
    }

    #[test]
    fn removing_the_last_passkey_is_fine_when_a_recovery_path_remains() {
        assert!(access(false, 1, 1).can_remove_passkey().is_ok());
    }

    #[test]
    fn removing_the_last_way_in_is_blocked() {
        // A passwordless, recovery-less account removing its only passkey would
        // be permanently locked out. This is the case the invariant exists for.
        assert_eq!(
            access(false, 1, 0).can_remove_passkey(),
            Err(RemovalBlocked::LastWayIn)
        );
    }

    #[test]
    fn removing_a_passkey_from_an_account_that_has_none_is_still_blocked() {
        // Defensive: saturating_sub must not make "0 passkeys" look survivable.
        assert_eq!(
            access(false, 0, 0).can_remove_passkey(),
            Err(RemovalBlocked::LastWayIn)
        );
    }

    // ── Removing the password (going passwordless) ──────────────────────────

    #[test]
    fn going_passwordless_needs_a_passkey() {
        assert_eq!(
            access(true, 0, 1).can_remove_password(),
            Err(RemovalBlocked::PasswordlessNeedsAPasskey)
        );
    }

    #[test]
    fn going_passwordless_needs_a_non_password_recovery_path() {
        // A passkey alone is not enough: lose the device and there is no way
        // back. D-33 makes this the explicit, audited friction point.
        assert_eq!(
            access(true, 1, 0).can_remove_password(),
            Err(RemovalBlocked::PasswordlessNeedsRecoveryPath)
        );
    }

    #[test]
    fn going_passwordless_is_allowed_with_a_passkey_and_a_recovery_path() {
        assert!(access(true, 1, 1).can_remove_password().is_ok());
    }

    #[test]
    fn many_passkeys_do_not_substitute_for_a_recovery_path() {
        // Five passkeys on five devices is still no way back if they are all
        // lost together (one stolen bag, one dead laptop). The rule does not
        // count passkeys toward the recovery requirement.
        assert_eq!(
            access(true, 5, 0).can_remove_password(),
            Err(RemovalBlocked::PasswordlessNeedsRecoveryPath)
        );
    }

    #[test]
    fn a_password_never_counts_as_its_own_recovery_path() {
        // The whole point of going passwordless is that the password is going
        // away; it cannot be the thing that makes doing so safe.
        let with_password_only = access(true, 1, 0);
        assert!(with_password_only.can_remove_password().is_err());
    }

    // ── ways_in ─────────────────────────────────────────────────────────────

    #[test]
    fn ways_in_counts_every_factor() {
        assert_eq!(access(true, 2, 1).ways_in(), 4);
        assert_eq!(access(false, 0, 0).ways_in(), 0);
        assert_eq!(access(true, 0, 0).ways_in(), 1);
    }

    #[test]
    fn blocked_reasons_have_distinct_slugs_and_actionable_messages() {
        let reasons = [
            RemovalBlocked::LastWayIn,
            RemovalBlocked::PasswordlessNeedsAPasskey,
            RemovalBlocked::PasswordlessNeedsRecoveryPath,
        ];
        let mut seen = std::collections::HashSet::new();
        for r in reasons {
            assert!(seen.insert(r.as_str()), "duplicate slug {}", r.as_str());
            assert!(!r.message().is_empty());
        }
    }
}
