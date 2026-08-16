//! Daily spend accounting and the budget gate (M2).
//!
//! Every AI call reports its own dollar cost on the response
//! (`AiResponse.cost_estimate`, the p11j companion), so Argus can account its
//! own spend without reading a kernel table. This module holds the arithmetic
//! and the decision; the store port holds the row.
//!
//! # What "paused" means
//!
//! Past the daily limit, the two AI-consuming stages stop **without failing**.
//! A refused analyze or summarize job re-enqueues itself for the next UTC day
//! and returns success, so the queue accumulates deferred work instead of
//! burning retry attempts against a limit that will not move for hours. Fetch,
//! decide's already-queued work, embed and cluster keep running: the pipeline
//! keeps ingesting and keeps its cheap stages current, and only the spending
//! stages wait.
//!
//! (Decide also spends. It is not gated here: M1 shipped decide as the cost
//! floor of the pipeline and gating it would silently stop relevance scoring,
//! which is the one thing that keeps volume down. The limit is expressed
//! against the stages M2 adds. This is recorded as a deliberate scope choice in
//! `M2-FRICTION.md`, not an oversight.)

/// Days since the Unix epoch for a unix timestamp, floored — negative
/// timestamps included, which `/` alone would round toward zero.
fn days_since_epoch(unix_seconds: i64) -> i64 {
    unix_seconds.div_euclid(86_400)
}

/// The UTC calendar day of a unix timestamp as `YYYY-MM-DD`.
///
/// Implemented here rather than pulled from a date crate because the core is
/// compiled to wasm and must stay free of clock/locale dependencies; the
/// civil-from-days conversion is the standard shift-to-March algorithm.
#[must_use]
pub fn utc_day(unix_seconds: i64) -> String {
    let z = days_since_epoch(unix_seconds) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The first instant of the UTC day after the one containing `unix_seconds`.
#[must_use]
pub fn next_utc_day_start(unix_seconds: i64) -> i64 {
    (days_since_epoch(unix_seconds) + 1) * 86_400
}

/// Seconds until the next UTC day starts, at least one.
///
/// This is the delay a budget-paused job re-enqueues itself with. The floor of
/// one second keeps a job scheduled exactly on a day boundary from re-running
/// inside the same second it was refused.
#[must_use]
pub fn seconds_until_next_day(unix_seconds: i64) -> i64 {
    (next_utc_day_start(unix_seconds) - unix_seconds).max(1)
}

/// Budget limits, read from site config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetConfig {
    /// Dollars per UTC day above which AI-consuming stages pause. Zero or
    /// negative disables the limit entirely.
    pub daily_limit_usd: f64,
    /// Dollars per UTC day above which a warning is emitted. Zero or negative
    /// disables the warning.
    pub alert_threshold_usd: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            // No limit until an operator sets one: a plugin that silently
            // stopped analyzing on an unconfigured default would be a worse
            // failure than an unbounded bill an operator can see coming in the
            // spend-by-stage figures.
            daily_limit_usd: 0.0,
            alert_threshold_usd: 0.0,
        }
    }
}

/// What the budget gate says about running an AI-consuming stage now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetVerdict {
    /// Spend is below every configured threshold.
    Ok,
    /// Past the alert threshold but below the limit: run, and warn.
    Warn,
    /// At or past the daily limit: do not run.
    Pause,
}

/// Judge today's spend against the configured thresholds.
///
/// The limit is checked before the alert, so a limit set below the alert
/// threshold (a misconfiguration) still pauses rather than merely warning.
/// Unpriced calls do not appear in `spent_usd` at all — they are counted
/// separately by the store, because an unpriced model is unknown spend, not
/// zero spend, and quietly folding it in as zero is how a budget stops meaning
/// anything.
#[must_use]
pub fn verdict(spent_usd: f64, config: &BudgetConfig) -> BudgetVerdict {
    if config.daily_limit_usd > 0.0 && spent_usd >= config.daily_limit_usd {
        return BudgetVerdict::Pause;
    }
    if config.alert_threshold_usd > 0.0 && spent_usd >= config.alert_threshold_usd {
        return BudgetVerdict::Warn;
    }
    BudgetVerdict::Ok
}

/// Today's spend for one stage, as the store reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DailySpend {
    /// Priced dollars spent today across all stages.
    pub spent_usd: f64,
    /// Calls made today across all stages.
    pub calls: u32,
    /// Of those, calls the host could not price.
    pub unpriced_calls: u32,
}

/// Project a daily cost from an observed run.
///
/// `observed_usd` was spent processing `observed_articles`; the projection
/// scales that to `projected_articles`. Returns `None` when the observation
/// carries no signal (no articles, or no priced call), because a projection
/// from zero is a fabrication and the report must say "unknown" instead.
#[must_use]
pub fn project_daily_cost(
    observed_usd: f64,
    observed_articles: u32,
    projected_articles: u32,
) -> Option<f64> {
    if observed_articles == 0 || observed_usd <= 0.0 {
        return None;
    }
    Some(observed_usd / f64::from(observed_articles) * f64::from(projected_articles))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn cfg(limit: f64, alert: f64) -> BudgetConfig {
        BudgetConfig {
            daily_limit_usd: limit,
            alert_threshold_usd: alert,
        }
    }

    // ---- UTC day arithmetic ---------------------------------------------

    #[test]
    fn utc_day_matches_known_timestamps() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(86_399), "1970-01-01");
        assert_eq!(utc_day(86_400), "1970-01-02");
        assert_eq!(utc_day(1_767_225_600), "2026-01-01");
        // A leap day, which the shift-to-March algorithm exists to get right.
        assert_eq!(utc_day(1_709_164_800), "2024-02-29");
        assert_eq!(utc_day(1_709_251_200), "2024-03-01");
    }

    #[test]
    fn utc_day_handles_pre_epoch_timestamps() {
        assert_eq!(utc_day(-1), "1969-12-31");
        assert_eq!(utc_day(-86_400), "1969-12-31");
        assert_eq!(utc_day(-86_401), "1969-12-30");
    }

    #[test]
    fn the_next_day_starts_at_midnight() {
        assert_eq!(next_utc_day_start(0), 86_400);
        assert_eq!(next_utc_day_start(86_399), 86_400);
        assert_eq!(next_utc_day_start(86_400), 172_800);
    }

    #[test]
    fn the_pause_delay_is_the_rest_of_the_day_and_never_zero() {
        assert_eq!(seconds_until_next_day(0), 86_400);
        assert_eq!(seconds_until_next_day(86_399), 1);
        // Exactly on a boundary: a full day, not zero.
        assert_eq!(seconds_until_next_day(86_400), 86_400);
    }

    #[test]
    fn a_paused_job_lands_in_the_next_day() {
        let now = 1_767_225_600 + 3_600; // 2026-01-01T01:00:00Z
        let resumed = now + seconds_until_next_day(now);
        assert_eq!(utc_day(now), "2026-01-01");
        assert_eq!(utc_day(resumed), "2026-01-02");
    }

    // ---- the verdict -----------------------------------------------------

    #[test]
    fn spend_below_everything_is_ok() {
        assert_eq!(verdict(1.0, &cfg(10.0, 5.0)), BudgetVerdict::Ok);
    }

    #[test]
    fn spend_past_the_alert_warns_but_runs() {
        assert_eq!(verdict(5.0, &cfg(10.0, 5.0)), BudgetVerdict::Warn);
        assert_eq!(verdict(9.99, &cfg(10.0, 5.0)), BudgetVerdict::Warn);
    }

    #[test]
    fn spend_at_the_limit_pauses() {
        assert_eq!(verdict(10.0, &cfg(10.0, 5.0)), BudgetVerdict::Pause);
        assert_eq!(verdict(1000.0, &cfg(10.0, 5.0)), BudgetVerdict::Pause);
    }

    #[test]
    fn an_unset_limit_never_pauses() {
        assert_eq!(
            verdict(1_000_000.0, &BudgetConfig::default()),
            BudgetVerdict::Ok
        );
        assert_eq!(verdict(1_000_000.0, &cfg(0.0, 0.0)), BudgetVerdict::Ok);
        assert_eq!(verdict(1_000_000.0, &cfg(-1.0, -1.0)), BudgetVerdict::Ok);
    }

    #[test]
    fn an_alert_without_a_limit_only_warns() {
        assert_eq!(verdict(50.0, &cfg(0.0, 5.0)), BudgetVerdict::Warn);
    }

    #[test]
    fn a_limit_below_the_alert_still_pauses() {
        // Misconfiguration: limit 5, alert 10. Spend of 7 must pause.
        assert_eq!(verdict(7.0, &cfg(5.0, 10.0)), BudgetVerdict::Pause);
    }

    #[test]
    fn the_next_day_resumes() {
        // Same limit, spend reset by the day roll: the gate reopens on its own.
        assert_eq!(verdict(12.0, &cfg(10.0, 5.0)), BudgetVerdict::Pause);
        assert_eq!(verdict(0.0, &cfg(10.0, 5.0)), BudgetVerdict::Ok);
    }

    // ---- projection ------------------------------------------------------

    #[test]
    fn projection_scales_an_observed_run() {
        assert_eq!(project_daily_cost(0.5, 50, 100), Some(1.0));
        assert_eq!(project_daily_cost(1.0, 10, 10), Some(1.0));
    }

    #[test]
    fn projection_from_no_signal_is_unknown_not_zero() {
        assert_eq!(project_daily_cost(0.0, 50, 100), None);
        assert_eq!(project_daily_cost(1.0, 0, 100), None);
    }

    #[test]
    fn daily_spend_defaults_to_nothing_spent() {
        let d = DailySpend::default();
        assert_eq!(d.spent_usd, 0.0);
        assert_eq!(d.calls, 0);
        assert_eq!(d.unpriced_calls, 0);
    }
}
