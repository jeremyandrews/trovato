//! The site's password policy (B304): minimum length, optional character-class
//! rules, and a maximum age.
//!
//! One `PasswordPolicy`, stored as one [`crate::models::SiteConfig`] row under
//! [`CONFIG_KEY`], read by every place a password is set and by the login path
//! that checks whether a password has aged out. `PasswordPolicy::validate` is
//! pure and synchronous, so it is unit-tested directly; `load`/`save` are the
//! only parts that touch the database.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::models::SiteConfig;

/// The `site_config` key the policy is stored under.
pub const CONFIG_KEY: &str = "password_policy";

/// The minimum length a site may configure. A policy cannot be loosened past
/// this regardless of what is stored — a corrupt or hand-edited config value
/// cannot silently drop every account to an unsafe minimum.
pub const MINIMUM_MIN_LENGTH: usize = 8;

/// The default minimum length, unless a site configures a higher one.
pub const DEFAULT_MIN_LENGTH: usize = 12;

/// The fixed maximum length. Not configurable: it exists to bound the cost of
/// the Argon2 hash, not to express a site's policy.
pub const MAX_LENGTH: usize = 128;

/// The site's password policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasswordPolicy {
    /// Minimum length, in characters. Clamped up to [`MINIMUM_MIN_LENGTH`]
    /// wherever it is read, so a stored value below it (from an older save,
    /// or a hand-edited config import) is never honoured.
    pub min_length: usize,
    /// Require at least one lowercase letter.
    pub require_lowercase: bool,
    /// Require at least one uppercase letter.
    pub require_uppercase: bool,
    /// Require at least one digit.
    pub require_digit: bool,
    /// Require at least one symbol (anything that is neither alphanumeric
    /// nor whitespace).
    pub require_symbol: bool,
    /// Maximum password age, in days. `0` means no maximum (the default).
    pub max_age_days: u32,
    /// When the maximum-age rule was last turned on (switched from `0` to a
    /// non-zero value). A password's age is measured from the account's
    /// `pass_changed`, or from this moment when `pass_changed` is `None` —
    /// so turning the rule on does not instantly expire every account that
    /// predates it, and no backfill of `pass_changed` is needed.
    pub max_age_enabled_at: Option<DateTime<Utc>>,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: DEFAULT_MIN_LENGTH,
            require_lowercase: false,
            require_uppercase: false,
            require_digit: false,
            require_symbol: false,
            max_age_days: 0,
            max_age_enabled_at: None,
        }
    }
}

impl PasswordPolicy {
    /// Load the policy from site config, or the default when unset or
    /// unparsable.
    pub async fn load(pool: &PgPool) -> Self {
        SiteConfig::get(pool, CONFIG_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    /// Resolve `max_age_enabled_at` for a save: set to now on an off-to-on
    /// transition, cleared when switched off, left as `previous` held it
    /// otherwise (so repeated saves with the rule already on do not keep
    /// pushing the baseline forward).
    #[must_use]
    pub fn resolved_for_save(mut self, previous: &PasswordPolicy) -> Self {
        self.max_age_enabled_at = if self.max_age_days == 0 {
            None
        } else if previous.max_age_days == 0 {
            Some(Utc::now())
        } else {
            previous.max_age_enabled_at
        };
        self
    }

    /// Save the policy as one site-config row.
    pub async fn save(&self, pool: &PgPool) -> Result<()> {
        SiteConfig::set(pool, CONFIG_KEY, serde_json::to_value(self)?).await
    }

    /// The effective minimum length: never below [`MINIMUM_MIN_LENGTH`].
    pub fn effective_min_length(&self) -> usize {
        self.min_length.max(MINIMUM_MIN_LENGTH)
    }

    /// Validate `password` against this policy. Lengths count characters,
    /// not bytes.
    pub fn validate(&self, password: &str) -> Result<(), String> {
        if password.is_empty() {
            return Err("Password is required.".to_string());
        }
        let len = password.chars().count();
        let min_length = self.effective_min_length();
        if len < min_length {
            return Err(format!(
                "Password must be at least {min_length} characters."
            ));
        }
        if len > MAX_LENGTH {
            return Err(format!(
                "Password must be {MAX_LENGTH} characters or fewer."
            ));
        }
        if self.require_lowercase && !password.chars().any(|c| c.is_lowercase()) {
            return Err("Password must contain at least one lowercase letter.".to_string());
        }
        if self.require_uppercase && !password.chars().any(|c| c.is_uppercase()) {
            return Err("Password must contain at least one uppercase letter.".to_string());
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            return Err("Password must contain at least one digit.".to_string());
        }
        if self.require_symbol
            && !password
                .chars()
                .any(|c| !c.is_alphanumeric() && !c.is_whitespace())
        {
            return Err("Password must contain at least one symbol.".to_string());
        }
        Ok(())
    }

    /// Whether a password last changed at `pass_changed` has exceeded the
    /// maximum age. `pass_changed` of `None` falls back to
    /// `max_age_enabled_at`; if that is also `None` (the rule is on but has
    /// no recorded baseline at all — not reachable through `resolved_for_save`,
    /// but a direct config write could do it) the password cannot be shown to
    /// be expired, so it is not.
    pub fn is_expired(&self, pass_changed: Option<DateTime<Utc>>) -> bool {
        if self.max_age_days == 0 {
            return false;
        }
        let Some(baseline) = pass_changed.or(self.max_age_enabled_at) else {
            return false;
        };
        let age_days = Utc::now().signed_duration_since(baseline).num_days();
        age_days >= i64::from(self.max_age_days)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn defaults_are_twelve_characters_no_classes_no_max_age() {
        let policy = PasswordPolicy::default();
        assert_eq!(policy.min_length, 12);
        assert_eq!(policy.max_age_days, 0);
        assert!(policy.validate(&"a".repeat(12)).is_ok());
        assert!(policy.validate(&"a".repeat(11)).is_err());
    }

    #[test]
    fn a_configured_minimum_below_eight_is_still_enforced_at_eight() {
        let policy = PasswordPolicy {
            min_length: 3,
            ..Default::default()
        };
        assert_eq!(policy.effective_min_length(), MINIMUM_MIN_LENGTH);
        assert!(policy.validate(&"a".repeat(7)).is_err());
        assert!(policy.validate(&"a".repeat(8)).is_ok());
    }

    #[test]
    fn a_configured_minimum_above_twelve_is_enforced() {
        let policy = PasswordPolicy {
            min_length: 20,
            ..Default::default()
        };
        assert!(policy.validate(&"a".repeat(19)).is_err());
        assert!(policy.validate(&"a".repeat(20)).is_ok());
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        let policy = PasswordPolicy {
            min_length: 12,
            ..Default::default()
        };
        // 12 multi-byte characters: well under 128 bytes, 12 chars exactly.
        let password: String = std::iter::repeat_n('é', 12).collect();
        assert_eq!(password.chars().count(), 12);
        assert!(policy.validate(&password).is_ok());
    }

    #[test]
    fn the_maximum_is_fixed_at_128_regardless_of_policy() {
        let policy = PasswordPolicy::default();
        assert!(policy.validate(&"a".repeat(128)).is_ok());
        assert!(policy.validate(&"a".repeat(129)).is_err());
    }

    #[test]
    fn character_class_rules_apply_only_when_switched_on() {
        let off = PasswordPolicy::default();
        assert!(off.validate(&"alllowercase1234".to_string()).is_ok());

        let on = PasswordPolicy {
            require_uppercase: true,
            require_digit: true,
            require_symbol: true,
            require_lowercase: true,
            ..Default::default()
        };
        assert!(on.validate("alllowercase1234").is_err());
        assert!(on.validate("ALLUPPERCASE1234").is_err());
        assert!(on.validate("NoDigitsHere!!!!").is_err());
        assert!(on.validate("NoSymbolsHere1234").is_err());
        assert!(on.validate("Has1Each!Class01").is_ok());
    }

    #[test]
    fn empty_password_is_rejected_before_any_other_check() {
        assert!(PasswordPolicy::default().validate("").is_err());
    }

    #[test]
    fn turning_max_age_on_sets_the_baseline_only_on_the_transition() {
        let off = PasswordPolicy::default();
        let turned_on = PasswordPolicy {
            max_age_days: 90,
            ..Default::default()
        }
        .resolved_for_save(&off);
        assert!(turned_on.max_age_enabled_at.is_some());

        // Saving again with the rule already on keeps the original baseline.
        let still_on = PasswordPolicy {
            max_age_days: 90,
            ..turned_on.clone()
        }
        .resolved_for_save(&turned_on);
        assert_eq!(still_on.max_age_enabled_at, turned_on.max_age_enabled_at);
    }

    #[test]
    fn turning_max_age_off_clears_the_baseline() {
        let on = PasswordPolicy {
            max_age_days: 90,
            max_age_enabled_at: Some(Utc::now()),
            ..Default::default()
        };
        let off = PasswordPolicy {
            max_age_days: 0,
            ..Default::default()
        }
        .resolved_for_save(&on);
        assert!(off.max_age_enabled_at.is_none());
    }

    #[test]
    fn no_max_age_never_expires() {
        let policy = PasswordPolicy::default();
        assert!(!policy.is_expired(Some(Utc::now() - Duration::days(10_000))));
        assert!(!policy.is_expired(None));
    }

    #[test]
    fn a_password_older_than_the_maximum_is_expired() {
        let policy = PasswordPolicy {
            max_age_days: 90,
            ..Default::default()
        };
        assert!(policy.is_expired(Some(Utc::now() - Duration::days(91))));
        assert!(!policy.is_expired(Some(Utc::now() - Duration::days(1))));
    }

    #[test]
    fn a_null_pass_changed_falls_back_to_the_enabled_at_baseline() {
        let policy = PasswordPolicy {
            max_age_days: 90,
            max_age_enabled_at: Some(Utc::now() - Duration::days(91)),
            ..Default::default()
        };
        // No pass_changed recorded (an account older than this column):
        // age is measured from when the rule was turned on, not "never".
        assert!(policy.is_expired(None));

        let recently_enabled = PasswordPolicy {
            max_age_days: 90,
            max_age_enabled_at: Some(Utc::now() - Duration::days(1)),
            ..Default::default()
        };
        assert!(!recently_enabled.is_expired(None));
    }
}
