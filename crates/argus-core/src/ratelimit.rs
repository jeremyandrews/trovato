//! When a notification may be sent: debounce, quiet hours, digest collapse and
//! the priority override (M4).
//!
//! All of this is arithmetic over a unix timestamp and a configuration, with no
//! host in sight — which is what lets the midnight-crossing and boundary cases
//! be tested exhaustively instead of observed in production at 23:00.
//!
//! # Quiet hours have no timezone, and cannot
//!
//! A wasm plugin has no clock, no `TZ` and no tz database; `now` arrives as UTC
//! seconds from Postgres. "23:00–07:00 local" is therefore expressed as a window
//! in hours plus a site-configured UTC offset in minutes
//! ([`NotifyConfig::quiet_utc_offset_minutes`]). An operator in a
//! DST-observing zone moves the offset twice a year or accepts an hour of drift
//! on when the window opens. `M4-DESIGN.md` Decision 5 argues why the honest
//! offset beats a fiction about a timezone the plugin cannot know.

use crate::notify::{EventKind, NotifyPriority};

/// Default seconds between two notifications about the same subject.
pub const DEFAULT_DEBOUNCE_SECONDS: i64 = 3_600;

/// Default number of due story events that collapse into one digest.
pub const DEFAULT_DIGEST_THRESHOLD: usize = 5;

/// Default window the digest counts within.
pub const DEFAULT_DIGEST_WINDOW_SECONDS: i64 = 900;

/// Default first hour of the quiet window, in the configured local offset.
pub const DEFAULT_QUIET_START_HOUR: u8 = 23;

/// Default hour the quiet window ends.
pub const DEFAULT_QUIET_END_HOUR: u8 = 7;

/// Default relevance score at or above which a story notifies.
pub const DEFAULT_NOTIFY_THRESHOLD: u8 = 70;

/// Default token-overlap distance above which a re-summarized story counts as
/// materially changed, when the AI judge is switched off.
pub const DEFAULT_CHANGE_RATIO: f64 = 0.35;

/// Default first retry delay for a failed channel.
pub const DEFAULT_RETRY_BASE_SECONDS: i64 = 60;

/// Ceiling on a retry delay, so exponential backoff cannot park a notification
/// past the point where anyone still wants it.
pub const MAX_RETRY_DELAY_SECONDS: i64 = 3_600;

/// Default attempts per channel before a delivery is abandoned.
pub const DEFAULT_MAX_DELIVERY_ATTEMPTS: u32 = 5;

/// Default consecutive fetch failures before a feed is alerted on.
///
/// Three, because one failure is a blip and two is a bad afternoon: a public
/// feed that 503s twice in a row usually comes back on its own, and an alert
/// that fires on that is an alert an operator learns to ignore.
pub const DEFAULT_FEED_FAILURE_THRESHOLD: u32 = 3;

/// Default seconds an eligible job may sit unclaimed before the queue is called
/// stuck. Fifteen minutes is comfortably longer than any legitimate backlog
/// drain and comfortably shorter than a night's sleep.
pub const DEFAULT_QUEUE_STUCK_SECONDS: i64 = 900;

/// Most channels one notify job dispatches to before handing the rest to
/// channel-scoped jobs.
///
/// Each channel is one outbound POST with its own 60 s transfer budget, and the
/// background epoch is 150 s. Eight is comfortably inside that with room for the
/// judge call that may precede it.
pub const MAX_CHANNELS_PER_DISPATCH: usize = 8;

/// Everything the notification layer is tuned by, read from site variables.
///
/// Every field has a working default, so an operator who configures nothing
/// still gets sane behaviour: quiet overnight, one notification per story per
/// hour, digests past five, and only stories that scored 70 or better.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NotifyConfig {
    /// Seconds between two notifications about the same subject. Zero or
    /// negative disables debouncing.
    pub debounce_seconds: i64,
    /// Due story events in the window that collapse into one digest. Zero or one
    /// disables digesting.
    pub digest_threshold: usize,
    /// The window the digest counts within.
    pub digest_window_seconds: i64,
    /// First hour of the quiet window (`0..=23`, in the configured offset).
    pub quiet_start_hour: u8,
    /// Hour the quiet window ends (`0..=23`). Equal to the start means no quiet
    /// hours at all.
    pub quiet_end_hour: u8,
    /// Minutes to add to UTC to get the operator's local time.
    pub quiet_utc_offset_minutes: i64,
    /// Whether operator alerts are silenced during quiet hours. Default `false`:
    /// a pipeline that has stopped is worth waking someone for.
    pub quiet_hours_alerts: bool,
    /// Relevance score at or above which a story notifies, when its topic is not
    /// high priority.
    pub notify_threshold: u8,
    /// Whether the story-update judge makes an AI call.
    pub judge_enabled: bool,
    /// Token-overlap distance that counts as material change when the judge is
    /// off.
    pub change_ratio: f64,
    /// First retry delay for a failed channel.
    pub retry_base_seconds: i64,
    /// Attempts per channel before a delivery is abandoned.
    pub max_delivery_attempts: u32,
    /// Whether the operator alert pass runs at all.
    pub alerts_enabled: bool,
    /// Consecutive fetch failures at which a feed is alerted on.
    pub feed_failure_threshold: u32,
    /// Seconds an eligible queue job may sit unclaimed before the queue is
    /// called stuck. Zero or negative disables the check.
    pub queue_stuck_seconds: i64,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            debounce_seconds: DEFAULT_DEBOUNCE_SECONDS,
            digest_threshold: DEFAULT_DIGEST_THRESHOLD,
            digest_window_seconds: DEFAULT_DIGEST_WINDOW_SECONDS,
            quiet_start_hour: DEFAULT_QUIET_START_HOUR,
            quiet_end_hour: DEFAULT_QUIET_END_HOUR,
            quiet_utc_offset_minutes: 0,
            quiet_hours_alerts: false,
            notify_threshold: DEFAULT_NOTIFY_THRESHOLD,
            judge_enabled: true,
            change_ratio: DEFAULT_CHANGE_RATIO,
            retry_base_seconds: DEFAULT_RETRY_BASE_SECONDS,
            max_delivery_attempts: DEFAULT_MAX_DELIVERY_ATTEMPTS,
            alerts_enabled: true,
            feed_failure_threshold: DEFAULT_FEED_FAILURE_THRESHOLD,
            queue_stuck_seconds: DEFAULT_QUEUE_STUCK_SECONDS,
        }
    }
}

impl NotifyConfig {
    /// Whether a quiet window is configured at all.
    #[must_use]
    pub fn has_quiet_hours(&self) -> bool {
        self.quiet_start_hour != self.quiet_end_hour
            && self.quiet_start_hour < 24
            && self.quiet_end_hour < 24
    }

    /// Whether digesting is switched on.
    #[must_use]
    pub fn has_digest(&self) -> bool {
        self.digest_threshold > 1 && self.digest_window_seconds > 0
    }
}

// ---------------------------------------------------------------------------
// Quiet hours
// ---------------------------------------------------------------------------

/// The local hour (`0..=23`) at `now`, given the configured offset.
#[must_use]
pub fn local_hour(now: i64, offset_minutes: i64) -> u8 {
    let local = now + offset_minutes * 60;
    // `rem_euclid` rather than `%`: a pre-epoch or large-negative-offset instant
    // must still land in `0..=23` rather than going negative.
    let seconds_into_day = local.rem_euclid(86_400);
    (seconds_into_day / 3_600) as u8
}

/// Whether `now` falls inside the quiet window.
///
/// The window wraps midnight when `start > end`, which is the default case
/// (23:00 → 07:00) and therefore the one that must be right.
#[must_use]
pub fn in_quiet_hours(now: i64, config: &NotifyConfig) -> bool {
    if !config.has_quiet_hours() {
        return false;
    }
    let hour = local_hour(now, config.quiet_utc_offset_minutes);
    if config.quiet_start_hour < config.quiet_end_hour {
        // A same-day window, e.g. 01:00 → 06:00.
        hour >= config.quiet_start_hour && hour < config.quiet_end_hour
    } else {
        // Wraps midnight, e.g. 23:00 → 07:00.
        hour >= config.quiet_start_hour || hour < config.quiet_end_hour
    }
}

/// The first instant at or after `now` when the quiet window is over.
///
/// Returns `now` unchanged when the window is not in force, so a caller can use
/// it unconditionally.
#[must_use]
pub fn quiet_until(now: i64, config: &NotifyConfig) -> i64 {
    if !in_quiet_hours(now, config) {
        return now;
    }
    let local = now + config.quiet_utc_offset_minutes * 60;
    let day_start = local - local.rem_euclid(86_400);
    let end = day_start + i64::from(config.quiet_end_hour) * 3_600;
    // A window that wraps midnight ends on the *next* local day whenever the
    // end hour has already passed today.
    let end = if end <= local { end + 86_400 } else { end };
    end - config.quiet_utc_offset_minutes * 60
}

// ---------------------------------------------------------------------------
// The send decision
// ---------------------------------------------------------------------------

/// What the rate limiter says about sending a notification now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendVerdict {
    /// Send it.
    Send,
    /// Hold it until `until`, then reconsider.
    Defer {
        /// The instant to reconsider at.
        until: i64,
        /// Why, for the operator-readable job result.
        reason: &'static str,
    },
    /// Do not send it at all.
    Suppress {
        /// Why, recorded on the outbox row.
        reason: &'static str,
    },
}

impl SendVerdict {
    /// Whether this verdict sends.
    #[must_use]
    pub fn is_send(&self) -> bool {
        matches!(self, SendVerdict::Send)
    }
}

/// Decide whether a notification may go out now.
///
/// `last_sent_at` is the most recent *sent* notification about the same subject,
/// which is what debouncing is measured against.
///
/// The order is deliberate. The priority override comes first, because a
/// high-priority event bypasses everything below it. Debounce comes before quiet
/// hours because a debounced notification is dead — deferring it past the quiet
/// window would deliver a stale duplicate at 07:00 rather than dropping it.
#[must_use]
pub fn verdict(
    now: i64,
    kind: EventKind,
    priority: NotifyPriority,
    last_sent_at: Option<i64>,
    config: &NotifyConfig,
) -> SendVerdict {
    if priority == NotifyPriority::High {
        return SendVerdict::Send;
    }

    if config.debounce_seconds > 0
        && let Some(last) = last_sent_at
        && now < last + config.debounce_seconds
    {
        return SendVerdict::Suppress {
            reason: "debounced: the same subject was notified inside the debounce window",
        };
    }

    // Operator alerts are not silenced by default: a pipeline that has stopped
    // is the thing worth an interruption.
    let silenced = !kind.is_operator_alert() || config.quiet_hours_alerts;
    if silenced && in_quiet_hours(now, config) {
        return SendVerdict::Defer {
            until: quiet_until(now, config),
            reason: "quiet hours",
        };
    }

    SendVerdict::Send
}

// ---------------------------------------------------------------------------
// Digest
// ---------------------------------------------------------------------------

/// Whether `pending` due story events in the window collapse into one digest.
#[must_use]
pub fn should_digest(pending: usize, config: &NotifyConfig) -> bool {
    config.has_digest() && pending >= config.digest_threshold
}

/// The window start a digest counts from.
#[must_use]
pub fn digest_window_start(now: i64, config: &NotifyConfig) -> i64 {
    now - config.digest_window_seconds.max(0)
}

// ---------------------------------------------------------------------------
// Retry backoff
// ---------------------------------------------------------------------------

/// The delay before retrying a failed channel delivery.
///
/// Exponential in the attempt count, capped at [`MAX_RETRY_DELAY_SECONDS`]. A
/// WASM plugin cannot sleep, so this is the `delay` on a channel-scoped
/// re-enqueue rather than a pause inside the worker (`M4-DESIGN.md` Decision 4).
#[must_use]
pub fn retry_delay(attempt: u32, config: &NotifyConfig) -> i64 {
    let base = config.retry_base_seconds.max(1);
    // Saturating rather than wrapping: attempt 40 must clamp, not overflow to a
    // negative delay that would re-enqueue in the past.
    let factor = 1i64.checked_shl(attempt.min(20)).unwrap_or(i64::MAX);
    base.saturating_mul(factor).min(MAX_RETRY_DELAY_SECONDS)
}

/// Whether a failed delivery has attempts left.
#[must_use]
pub fn may_retry(attempts: u32, config: &NotifyConfig) -> bool {
    attempts < config.max_delivery_attempts
}

// ---------------------------------------------------------------------------
// Qualification
// ---------------------------------------------------------------------------

/// Whether a story qualifies for a notification, and how loud it is.
///
/// A high-priority topic notifies whatever the score; otherwise the story's
/// relevance must reach the configured floor. Returns `None` when it does not
/// qualify at all, which is the common case and the reason a busy pipeline is
/// not a busy phone.
#[must_use]
pub fn story_priority(
    topic_priority: NotifyPriority,
    relevance_score: Option<i32>,
    config: &NotifyConfig,
) -> Option<NotifyPriority> {
    if topic_priority == NotifyPriority::High {
        return Some(NotifyPriority::High);
    }
    match relevance_score {
        Some(score) if score >= i32::from(config.notify_threshold) => Some(NotifyPriority::Normal),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// 2026-01-01T00:00:00Z, a Thursday, used as the base for hour arithmetic.
    const MIDNIGHT: i64 = 1_767_225_600;

    fn at(hour: i64) -> i64 {
        MIDNIGHT + hour * 3_600
    }

    fn cfg() -> NotifyConfig {
        NotifyConfig::default()
    }

    // ---- local hour ------------------------------------------------------

    #[test]
    fn local_hour_tracks_utc_when_the_offset_is_zero() {
        assert_eq!(local_hour(MIDNIGHT, 0), 0);
        assert_eq!(local_hour(at(13), 0), 13);
        assert_eq!(local_hour(at(23) + 3_599, 0), 23);
    }

    #[test]
    fn a_positive_offset_wraps_forward_over_midnight() {
        // 23:00 UTC at +02:00 is 01:00 the next local day.
        assert_eq!(local_hour(at(23), 120), 1);
    }

    #[test]
    fn a_negative_offset_wraps_backward_over_midnight() {
        // 00:30 UTC at -05:00 is 19:00 the previous local day.
        assert_eq!(local_hour(MIDNIGHT + 1_800, -300), 19);
    }

    #[test]
    fn a_pre_epoch_instant_still_lands_in_the_day() {
        assert_eq!(local_hour(-1, 0), 23);
        assert_eq!(local_hour(-86_400, 0), 0);
    }

    // ---- quiet hours -----------------------------------------------------

    #[test]
    fn the_default_window_is_quiet_overnight_and_loud_by_day() {
        let c = cfg();
        for hour in [23, 0, 3, 6] {
            assert!(in_quiet_hours(at(hour), &c), "{hour}:00 should be quiet");
        }
        for hour in [7, 12, 18, 22] {
            assert!(!in_quiet_hours(at(hour), &c), "{hour}:00 should be loud");
        }
    }

    #[test]
    fn the_window_boundaries_are_inclusive_at_the_start_and_exclusive_at_the_end() {
        let c = cfg();
        // 22:59:59 is loud, 23:00:00 is quiet.
        assert!(!in_quiet_hours(at(23) - 1, &c));
        assert!(in_quiet_hours(at(23), &c));
        // 06:59:59 is quiet, 07:00:00 is loud.
        assert!(in_quiet_hours(at(7) - 1, &c));
        assert!(!in_quiet_hours(at(7), &c));
    }

    #[test]
    fn a_same_day_window_does_not_wrap() {
        let c = NotifyConfig {
            quiet_start_hour: 1,
            quiet_end_hour: 6,
            ..cfg()
        };
        assert!(!in_quiet_hours(at(0), &c));
        assert!(in_quiet_hours(at(1), &c));
        assert!(in_quiet_hours(at(5), &c));
        assert!(!in_quiet_hours(at(6), &c));
        assert!(!in_quiet_hours(at(23), &c));
    }

    #[test]
    fn equal_start_and_end_hours_mean_no_quiet_window() {
        let c = NotifyConfig {
            quiet_start_hour: 3,
            quiet_end_hour: 3,
            ..cfg()
        };
        assert!(!c.has_quiet_hours());
        for hour in 0..24 {
            assert!(!in_quiet_hours(at(hour), &c), "{hour}:00");
        }
    }

    #[test]
    fn the_offset_moves_the_whole_window() {
        // Quiet 23:00-07:00 at +02:00 means quiet 21:00-05:00 UTC.
        let c = NotifyConfig {
            quiet_utc_offset_minutes: 120,
            ..cfg()
        };
        assert!(in_quiet_hours(at(21), &c));
        assert!(in_quiet_hours(at(4), &c));
        assert!(!in_quiet_hours(at(5), &c));
        assert!(!in_quiet_hours(at(20), &c));
    }

    #[test]
    fn quiet_until_lands_exactly_on_the_end_hour_after_a_midnight_crossing() {
        let c = cfg();
        // 23:30 on day one resolves to 07:00 on day two.
        let now = at(23) + 1_800;
        let until = quiet_until(now, &c);
        assert_eq!(until, MIDNIGHT + 86_400 + 7 * 3_600);
        assert!(!in_quiet_hours(until, &c), "the deferral must land loud");
        assert!(until > now);
    }

    #[test]
    fn quiet_until_lands_on_the_same_day_when_the_window_has_not_wrapped_yet() {
        let c = cfg();
        // 02:00 resolves to 07:00 the same day.
        let until = quiet_until(at(2), &c);
        assert_eq!(until, MIDNIGHT + 7 * 3_600);
        assert!(!in_quiet_hours(until, &c));
    }

    #[test]
    fn quiet_until_is_a_no_op_outside_the_window() {
        let c = cfg();
        assert_eq!(quiet_until(at(12), &c), at(12));
    }

    #[test]
    fn quiet_until_is_always_in_the_future_from_every_minute_of_the_window() {
        let c = cfg();
        // Every minute of the eight-hour window must resolve forward and land
        // outside it — the property the two hand-picked cases above sample.
        for minute in 0..(8 * 60) {
            let now = at(23) + minute * 60;
            let until = quiet_until(now, &c);
            assert!(until > now, "minute {minute} did not move forward");
            assert!(!in_quiet_hours(until, &c), "minute {minute} stayed quiet");
        }
    }

    // ---- the verdict -----------------------------------------------------

    #[test]
    fn an_ordinary_story_in_working_hours_sends() {
        assert_eq!(
            verdict(
                at(12),
                EventKind::StoryNew,
                NotifyPriority::Normal,
                None,
                &cfg()
            ),
            SendVerdict::Send
        );
    }

    #[test]
    fn a_repeat_inside_the_debounce_window_is_suppressed() {
        let c = cfg();
        let last = at(12);
        let v = verdict(
            last + 60,
            EventKind::StoryUpdated,
            NotifyPriority::Normal,
            Some(last),
            &c,
        );
        assert!(matches!(v, SendVerdict::Suppress { .. }));
        // One second past the window, it sends again.
        assert_eq!(
            verdict(
                last + DEFAULT_DEBOUNCE_SECONDS,
                EventKind::StoryUpdated,
                NotifyPriority::Normal,
                Some(last),
                &c
            ),
            SendVerdict::Send
        );
    }

    #[test]
    fn a_zero_debounce_never_suppresses() {
        let c = NotifyConfig {
            debounce_seconds: 0,
            ..cfg()
        };
        assert_eq!(
            verdict(
                at(12),
                EventKind::StoryUpdated,
                NotifyPriority::Normal,
                Some(at(12)),
                &c
            ),
            SendVerdict::Send
        );
    }

    #[test]
    fn a_story_in_quiet_hours_is_deferred_to_the_end_of_the_window() {
        let c = cfg();
        let now = at(1);
        match verdict(now, EventKind::StoryNew, NotifyPriority::Normal, None, &c) {
            SendVerdict::Defer { until, reason } => {
                assert_eq!(until, MIDNIGHT + 7 * 3_600);
                assert_eq!(reason, "quiet hours");
            }
            other => panic!("expected a deferral, got {other:?}"),
        }
    }

    #[test]
    fn high_priority_bypasses_both_debounce_and_quiet_hours() {
        let c = cfg();
        let last = at(1);
        assert_eq!(
            verdict(
                last + 1,
                EventKind::StoryNew,
                NotifyPriority::High,
                Some(last),
                &c
            ),
            SendVerdict::Send,
            "a high-priority story at 01:00, one second after the last one"
        );
    }

    #[test]
    fn operator_alerts_bypass_quiet_hours_by_default_and_obey_it_when_told_to() {
        let c = cfg();
        for kind in EventKind::all()
            .into_iter()
            .filter(|k| k.is_operator_alert())
        {
            assert_eq!(
                verdict(at(3), kind, NotifyPriority::Normal, None, &c),
                SendVerdict::Send,
                "{kind:?} must not be silenced by default"
            );
        }

        let silent = NotifyConfig {
            quiet_hours_alerts: true,
            ..c
        };
        assert!(matches!(
            verdict(
                at(3),
                EventKind::QueueStuck,
                NotifyPriority::Normal,
                None,
                &silent
            ),
            SendVerdict::Defer { .. }
        ));
    }

    #[test]
    fn an_alert_is_still_debounced_inside_quiet_hours() {
        // Bypassing quiet hours is not a licence to repeat: a feed that has been
        // failing for six hours must not notify hourly through the night.
        let c = cfg();
        let last = at(2);
        assert!(matches!(
            verdict(
                last + 60,
                EventKind::FeedFailing,
                NotifyPriority::Normal,
                Some(last),
                &c
            ),
            SendVerdict::Suppress { .. }
        ));
    }

    // ---- digest ----------------------------------------------------------

    #[test]
    fn the_digest_threshold_is_inclusive() {
        let c = cfg();
        assert!(!should_digest(4, &c));
        assert!(should_digest(5, &c));
        assert!(should_digest(50, &c));
    }

    #[test]
    fn digesting_can_be_switched_off() {
        for threshold in [0, 1] {
            let c = NotifyConfig {
                digest_threshold: threshold,
                ..cfg()
            };
            assert!(!c.has_digest());
            assert!(!should_digest(100, &c));
        }
        let c = NotifyConfig {
            digest_window_seconds: 0,
            ..cfg()
        };
        assert!(!should_digest(100, &c));
    }

    #[test]
    fn the_digest_window_starts_behind_now() {
        let c = cfg();
        assert_eq!(digest_window_start(at(12), &c), at(12) - 900);
        let negative = NotifyConfig {
            digest_window_seconds: -5,
            ..c
        };
        assert_eq!(digest_window_start(at(12), &negative), at(12));
    }

    // ---- retry -----------------------------------------------------------

    #[test]
    fn retry_delay_doubles_and_then_caps() {
        let c = cfg();
        assert_eq!(retry_delay(0, &c), 60);
        assert_eq!(retry_delay(1, &c), 120);
        assert_eq!(retry_delay(2, &c), 240);
        assert_eq!(retry_delay(5, &c), 1_920);
        assert_eq!(retry_delay(6, &c), MAX_RETRY_DELAY_SECONDS);
        assert_eq!(retry_delay(u32::MAX, &c), MAX_RETRY_DELAY_SECONDS);
    }

    #[test]
    fn a_retry_delay_is_always_positive() {
        let c = NotifyConfig {
            retry_base_seconds: 0,
            ..cfg()
        };
        for attempt in [0u32, 1, 10, 30, u32::MAX] {
            assert!(retry_delay(attempt, &c) > 0, "attempt {attempt}");
        }
    }

    #[test]
    fn attempts_run_out() {
        let c = cfg();
        assert!(may_retry(0, &c));
        assert!(may_retry(4, &c));
        assert!(!may_retry(5, &c));
        assert!(!may_retry(500, &c));
    }

    // ---- qualification ---------------------------------------------------

    #[test]
    fn a_high_priority_topic_notifies_whatever_the_score() {
        let c = cfg();
        assert_eq!(
            story_priority(NotifyPriority::High, Some(1), &c),
            Some(NotifyPriority::High)
        );
        assert_eq!(
            story_priority(NotifyPriority::High, None, &c),
            Some(NotifyPriority::High)
        );
    }

    #[test]
    fn an_ordinary_topic_notifies_only_past_the_relevance_floor() {
        let c = cfg();
        assert_eq!(story_priority(NotifyPriority::Normal, Some(69), &c), None);
        assert_eq!(
            story_priority(NotifyPriority::Normal, Some(70), &c),
            Some(NotifyPriority::Normal)
        );
        assert_eq!(story_priority(NotifyPriority::Normal, None, &c), None);
    }

    #[test]
    fn a_zero_threshold_notifies_on_every_scored_story_but_not_an_unscored_one() {
        let c = NotifyConfig {
            notify_threshold: 0,
            ..cfg()
        };
        assert_eq!(
            story_priority(NotifyPriority::Normal, Some(0), &c),
            Some(NotifyPriority::Normal)
        );
        assert_eq!(story_priority(NotifyPriority::Normal, None, &c), None);
    }
}
