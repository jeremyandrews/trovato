//! Whether a newer Trovato release exists, and whether it is a security release.
//!
//! # Why this is not server infrastructure
//!
//! A site with no way to learn that a security fix exists is a site that does not
//! get it. The usual answer is an update server; the answer here is that GitHub
//! already is one. Tagging a release produces two stable endpoints for free:
//!
//! - `https://api.github.com/repos/<owner>/<repo>/releases/latest` — JSON.
//! - `https://github.com/<owner>/<repo>/releases.atom` — a feed, for people.
//!
//! So nothing is deployed, nothing is operated, and there is no service to keep
//! running for the life of the project.
//!
//! # Why the title carries the security signal
//!
//! The latest-release JSON says what the newest version is. It does not say
//! whether upgrading is urgent, and "newer exists" and "act now" are different
//! messages to put in front of an administrator. GitHub has no field for it, so
//! the convention is in the one field a human already writes deliberately: a
//! security release's title **starts with `[security]`**. `CONTRIBUTING.md` records
//! it as part of the release process; [`is_security_title`] is the reader.
//!
//! # Why this is kernel rather than a plugin
//!
//! It concerns the kernel's own version, which a plugin cannot know, and it needs
//! one outbound HTTPS request, which a plugin would need a network capability for.
//! Making every site grant a plugin network access to learn its own version is a
//! worse trade than a kernel cron task.
//!
//! # What it costs a site, stated plainly
//!
//! One outbound HTTPS GET to `api.github.com`, at most once per configured
//! interval, carrying no site data: no URL, no version, no identifier, nothing but
//! the request itself and a User-Agent. GitHub learns an IP address asked about a
//! public repository. The check is controlled by the `update_check` site setting
//! (default on) and by `UPDATE_CHECK=0` in the environment for a deployment that
//! must make no outbound requests at all. Drupal core has shipped update status on
//! by default for two decades; this is that posture, with the data flow written
//! down.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::debug;

use crate::models::SiteConfig;

/// The `site_config` key the last check's result is stored under.
pub const UPDATE_STATUS_KEY: &str = "update_status";

/// The `site_config` key that turns the check on and off.
pub const UPDATE_CHECK_KEY: &str = "update_check";

/// The default latest-release endpoint.
pub const DEFAULT_RELEASE_ENDPOINT: &str =
    "https://api.github.com/repos/jeremyandrews/trovato/releases/latest";

/// The prefix a security release's title carries.
pub const SECURITY_TITLE_PREFIX: &str = "[security]";

/// Default interval between checks: one day.
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 86_400;

/// How long the request may take before it is abandoned.
///
/// Short on purpose. Nothing waits on this and nothing is worse for having skipped
/// it, so a slow or unreachable GitHub costs a site five seconds of a cron run and
/// not a moment of a page render.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The running version, compiled in.
///
/// Everything in Trovato carries one version, so the kernel's package version *is*
/// the site's version (`docs/design/Versioning.md`).
pub fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The subset of GitHub's latest-release payload this reads.
///
/// Deliberately three fields. Anything else GitHub adds or removes is not this
/// module's business, and `serde` ignores what is not named.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    /// The git tag, e.g. `v0.99.1`.
    tag_name: String,
    /// The human-written release title, which carries the security convention.
    #[serde(default)]
    name: Option<String>,
    /// Whether GitHub marks it a prerelease. A prerelease is not an update.
    #[serde(default)]
    prerelease: bool,
    /// Whether it is still a draft. A draft is not published.
    #[serde(default)]
    draft: bool,
}

/// What the last check found, as stored in `site_config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// The latest published release's version, without the `v`.
    pub latest_version: String,
    /// The release title, as written.
    pub latest_title: String,
    /// Whether that release announces itself as a security release.
    pub is_security: bool,
    /// Unix timestamp of the check that produced this.
    pub checked_at: i64,
}

impl UpdateStatus {
    /// Whether the running version is older than the latest release.
    pub fn is_behind(&self) -> bool {
        matches!(
            compare_versions(running_version(), &self.latest_version),
            Some(std::cmp::Ordering::Less)
        )
    }
}

/// Whether a release title announces a security release.
///
/// Case-insensitive and leading-whitespace-tolerant, because the convention is
/// written by a person. Anything past the prefix is the title.
pub fn is_security_title(title: &str) -> bool {
    title
        .trim_start()
        .to_ascii_lowercase()
        .starts_with(SECURITY_TITLE_PREFIX)
}

/// A release tag's version, without the conventional leading `v`.
pub fn version_from_tag(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Compare two dotted numeric versions, or `None` if either does not parse.
///
/// Deliberately not semver: Trovato's versions are `major.minor.patch` of plain
/// integers and nothing else (`docs/design/Versioning.md`), so this compares
/// component by component and refuses anything it cannot read rather than
/// guessing. A tag it cannot read produces no banner, which is the safe direction:
/// a false "you are behind" is worse than a missing one, because an administrator
/// who cannot find the release it means stops trusting the banner.
pub fn compare_versions(running: &str, latest: &str) -> Option<std::cmp::Ordering> {
    let running = numeric_components(running)?;
    let latest = numeric_components(latest)?;
    Some(running.cmp(&latest))
}

/// A version's components as numbers, padded to three, or `None`.
fn numeric_components(version: &str) -> Option<Vec<u64>> {
    let version = version.trim();
    if version.is_empty() {
        return None;
    }
    // A pre-release or build suffix is not something this project's versions have,
    // and reading `0.99.0-rc1` as `0.99.0` would call a release candidate the
    // release. Refuse instead.
    let mut components: Vec<u64> = Vec::new();
    for part in version.split('.') {
        components.push(part.parse::<u64>().ok()?);
    }
    if components.is_empty() || components.len() > 4 {
        return None;
    }
    while components.len() < 3 {
        components.push(0);
    }
    Some(components)
}

/// Whether the update check is enabled for this site.
///
/// Two switches, and the environment wins: a site setting an administrator can
/// turn off, and `UPDATE_CHECK` for a deployment where "no outbound requests" is
/// an operational requirement rather than a preference. An air-gapped install
/// should not depend on a database row being right.
pub fn env_disables_check(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "off" | "no")
    )
}

/// Whether a check is due, given the last one and the interval.
pub fn is_due(last_checked_at: Option<i64>, now: i64, interval_secs: u64) -> bool {
    match last_checked_at {
        None => true,
        Some(last) => now.saturating_sub(last) >= i64::try_from(interval_secs).unwrap_or(i64::MAX),
    }
}

/// Read the stored status, or `None` when no check has stored one.
pub async fn stored_status(pool: &PgPool) -> Option<UpdateStatus> {
    let value = SiteConfig::get(pool, UPDATE_STATUS_KEY).await.ok()??;
    serde_json::from_value(value).ok()
}

/// Whether the site setting allows the check. Absent means yes.
pub async fn setting_allows_check(pool: &PgPool) -> bool {
    match SiteConfig::get(pool, UPDATE_CHECK_KEY).await {
        Ok(Some(value)) => value.as_bool().unwrap_or(true),
        // Absent, or unreadable: default on, which is the documented default.
        _ => true,
    }
}

/// Fetch the latest release and store what it says.
///
/// Returns `Ok(None)` when there is nothing to store: a draft, a prerelease, or a
/// tag whose version does not parse. Returns an error only when the request or the
/// write failed, and the caller logs that at debug and carries on — a site whose
/// update check is failing is not a site with a problem to report to its visitors.
pub async fn fetch_and_store(
    pool: &PgPool,
    client: &reqwest::Client,
    endpoint: &str,
    now: i64,
) -> Result<Option<UpdateStatus>> {
    let response = client
        .get(endpoint)
        .header("Accept", "application/vnd.github+json")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("failed to reach the release endpoint")?;

    if !response.status().is_success() {
        anyhow::bail!("release endpoint returned {}", response.status());
    }

    let release: GithubRelease = response
        .json()
        .await
        .context("release endpoint returned something that is not a release")?;

    if release.draft || release.prerelease {
        debug!(
            tag = %release.tag_name,
            draft = release.draft,
            prerelease = release.prerelease,
            "latest release is not a published release; nothing stored"
        );
        return Ok(None);
    }

    let latest_version = version_from_tag(&release.tag_name).to_string();
    if numeric_components(&latest_version).is_none() {
        debug!(
            tag = %release.tag_name,
            "latest release tag is not a version this can compare; nothing stored"
        );
        return Ok(None);
    }

    let latest_title = release.name.unwrap_or_else(|| release.tag_name.clone());
    let status = UpdateStatus {
        is_security: is_security_title(&latest_title),
        latest_version,
        latest_title,
        checked_at: now,
    };

    SiteConfig::set(
        pool,
        UPDATE_STATUS_KEY,
        serde_json::to_value(&status).context("failed to serialize the update status")?,
    )
    .await
    .context("failed to store the update status")?;

    Ok(Some(status))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn a_newer_release_is_behind_an_older_one_is_ahead() {
        assert_eq!(compare_versions("0.99.0", "0.99.1"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.99.0", "0.99.0"), Some(Ordering::Equal));
        assert_eq!(
            compare_versions("1.0.0", "0.99.0"),
            Some(Ordering::Greater),
            "1.0.0 is newer than 0.99.0, which string comparison gets backwards"
        );
        assert_eq!(
            compare_versions("0.9.0", "0.99.0"),
            Some(Ordering::Less),
            "0.9 is older than 0.99, which string comparison also gets backwards"
        );
    }

    #[test]
    fn a_short_version_is_padded_rather_than_refused() {
        assert_eq!(compare_versions("1.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1", "1.0.1"), Some(Ordering::Less));
    }

    /// A tag this cannot read produces no comparison, and therefore no banner.
    #[test]
    fn a_malformed_version_compares_to_nothing() {
        for bad in [
            "",
            "   ",
            "nightly",
            "v0.99.0",
            "0.99.x",
            "0.99.0-rc1",
            "0.99.0+build",
            "1.2.3.4.5",
            "0..1",
        ] {
            assert_eq!(
                compare_versions("0.99.0", bad),
                None,
                "{bad:?} must not compare as a version"
            );
            assert_eq!(compare_versions(bad, "0.99.0"), None);
        }
    }

    #[test]
    fn a_tag_loses_its_leading_v() {
        assert_eq!(version_from_tag("v0.99.1"), "0.99.1");
        assert_eq!(version_from_tag("0.99.1"), "0.99.1");
        assert_eq!(version_from_tag("verbose"), "erbose");
    }

    /// The one convention that separates "newer exists" from "act now".
    #[test]
    fn the_security_prefix_is_recognized_as_a_person_would_write_it() {
        assert!(is_security_title("[security] 0.99.1"));
        assert!(is_security_title("[SECURITY] 0.99.1"));
        assert!(is_security_title("  [Security] fixes CVE-2026-0001"));
        assert!(!is_security_title("0.99.1"));
        assert!(
            !is_security_title("0.99.1 [security]"),
            "the prefix has to lead, or every release mentioning the word becomes urgent"
        );
        assert!(!is_security_title("security release"));
    }

    #[test]
    fn the_environment_override_reads_the_usual_falsy_spellings() {
        for off in ["0", "false", "off", "no", " FALSE ", "Off"] {
            assert!(env_disables_check(Some(off)), "{off:?} must disable");
        }
        for on in ["1", "true", "on", "yes", ""] {
            assert!(!env_disables_check(Some(on)), "{on:?} must not disable");
        }
        assert!(
            !env_disables_check(None),
            "unset means on, which is the documented default"
        );
    }

    #[test]
    fn a_check_is_due_when_it_has_never_run_or_the_interval_has_passed() {
        assert!(is_due(None, 1_000, 86_400), "never checked is always due");
        assert!(!is_due(Some(1_000), 1_500, 86_400));
        assert!(is_due(Some(1_000), 1_000 + 86_400, 86_400));
        assert!(is_due(Some(1_000), 1_000 + 86_401, 86_400));
        assert!(
            !is_due(Some(5_000), 1_000, 86_400),
            "a clock that went backwards must not make every run a check"
        );
    }

    #[test]
    fn is_behind_uses_the_running_version() {
        let ahead = UpdateStatus {
            latest_version: "999.0.0".to_string(),
            latest_title: "999.0.0".to_string(),
            is_security: false,
            checked_at: 0,
        };
        assert!(ahead.is_behind());

        let same = UpdateStatus {
            latest_version: running_version().to_string(),
            latest_title: running_version().to_string(),
            is_security: false,
            checked_at: 0,
        };
        assert!(
            !same.is_behind(),
            "the running version is not behind itself"
        );

        let older = UpdateStatus {
            latest_version: "0.0.1".to_string(),
            latest_title: "0.0.1".to_string(),
            is_security: false,
            checked_at: 0,
        };
        assert!(!older.is_behind());

        let unreadable = UpdateStatus {
            latest_version: "nightly".to_string(),
            latest_title: "nightly".to_string(),
            is_security: false,
            checked_at: 0,
        };
        assert!(
            !unreadable.is_behind(),
            "an unreadable latest version must not claim the site is behind"
        );
    }
}
