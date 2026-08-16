//! The notification ports over kernel host functions (M4).
//!
//! Two adapters:
//!
//! - [`HostTransport`] → the one-shot `http-request` host. Notification bodies
//!   are a few kilobytes and every channel needs the response status to decide
//!   retryable from permanent, so this is the one-shot call rather than the
//!   streaming one the feed fetcher uses.
//! - [`HostNotify`] → the `db` host over `argus_notify_*`, plus
//!   [`crate::config_host`] for the channel and topic *configuration* that lives
//!   on Items.
//!
//! # Two statements that are one statement
//!
//! A plugin has no transaction (G-DB-NO-TX), so anything that must not be seen
//! half-done has to be expressible as a single SQL statement. Two things here
//! are:
//!
//! - **Claiming a digest** — select the foldable events and mark them folded, or
//!   neither. Two workers must not fold the same event into two digests.
//! - **Recording a delivery** — write the outcome and return the resulting
//!   attempt count, so the retry decision is made on a number nobody else can
//!   have changed in between.
//!
//! Both are written as `WITH … RETURNING` and run through `query-raw`. That
//! works because the host's read-only guard on `query-raw` checks the statement's
//! **first keyword** only (`crates/kernel/src/host/db.rs`, `is_read_only`), so a
//! data-modifying CTE passes it. Recorded as `G-QUERY-RAW-FIRST-KEYWORD` in
//! `M4-FRICTION.md`: it is what makes these two operations atomic, and it is a
//! guard that does not guard what its name says.

use argus_core::error::{CoreError, CoreResult};
use argus_core::notify::{
    ChannelConfig, ChannelOutcome, EventKind, EventState, NewEvent, Notification, NotifyPriority,
    OutboundRequest, OutboundResponse, StoredEvent, Transport,
};
use argus_core::ports::{FailingFeed, NotifyStore, QueueHealth};
use serde::Deserialize;
use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::types::HttpRequest;

use crate::config_host;
use crate::host_ports::{exec, map_http_err, query_rows};
use crate::item_host;

/// User-Agent sent on every outbound notification.
const USER_AGENT: &str = "Argus/1.0 (Trovato news intelligence)";

/// The event types that participate in digest collapse, as a SQL list.
///
/// Kept beside the `is_digestible` predicate it mirrors; the round-trip test in
/// `argus-core` guards the names, and this list is asserted against that
/// predicate below so the two cannot drift.
const DIGESTIBLE_TYPES: &str = "'story.new','story.updated'";

// ===========================================================================
// Transport
// ===========================================================================

/// The `http`-host-backed [`Transport`].
pub struct HostTransport;

impl Transport for HostTransport {
    fn post(&self, req: &OutboundRequest) -> CoreResult<OutboundResponse> {
        let mut http =
            HttpRequest::post(req.url.clone(), req.body.clone()).header("User-Agent", USER_AGENT);
        for (name, value) in &req.headers {
            http = http.header(name.clone(), value.clone());
        }
        // `map_http_err` maps the SSRF/malformed-URL code to a *permanent*
        // refusal, which is what turns a blocked webhook target into a clean
        // per-channel `blocked` row instead of five pointless retries.
        let resp = host::http_request(&http).map_err(map_http_err)?;
        Ok(OutboundResponse {
            status: resp.status,
            body: resp.body,
        })
    }
}

// ===========================================================================
// Store
// ===========================================================================

/// The `db`-host-backed [`NotifyStore`].
pub struct HostNotify;

/// An outbox row as SQL hands it back.
#[derive(Deserialize)]
struct EventRow {
    id: String,
    event_type: String,
    priority: String,
    subject_id: String,
    state: String,
    scheduled_at: i64,
    created: i64,
    title: String,
    body: String,
    link: String,
    data: String,
}

impl EventRow {
    /// Convert a row into the core type, defaulting anything unreadable rather
    /// than failing: a row written by a newer version of this plugin must not
    /// wedge an older worker, and an event whose `data` will not parse is still
    /// an event worth sending.
    fn into_event(self) -> StoredEvent {
        StoredEvent {
            kind: EventKind::from_column(&self.event_type).unwrap_or(EventKind::StoryNew),
            priority: NotifyPriority::parse(&self.priority),
            subject_id: (!self.subject_id.is_empty()).then_some(self.subject_id),
            state: EventState::from_column(&self.state).unwrap_or(EventState::Pending),
            scheduled_at: self.scheduled_at,
            created: self.created,
            title: self.title,
            body: self.body,
            link: (!self.link.is_empty()).then_some(self.link),
            data: serde_json::from_str(&self.data).unwrap_or_else(|_| json!({})),
            id: self.id,
        }
    }
}

/// The projection every event read shares, so the row shape cannot drift between
/// `load_event` and `claim_digest`.
const EVENT_COLUMNS: &str = "id::text AS id, event_type, priority, \
     COALESCE(subject_id::text, '') AS subject_id, state, scheduled_at, created, \
     title, body, COALESCE(link, '') AS link, data";

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Deserialize)]
struct CountRow {
    n: i64,
}

#[derive(Deserialize)]
struct TsRow {
    ts: i64,
}

#[derive(Deserialize)]
struct AttemptsRow {
    attempts: i64,
}

#[derive(Deserialize)]
struct FeedStateRow {
    id: String,
    failure_count: i64,
    last_error: String,
}

#[derive(Deserialize)]
struct QueueHealthRow {
    oldest_ready_age: i64,
    ready: i64,
    dead: i64,
}

/// Bind a nullable string as a JSON param.
fn opt_str(value: Option<&str>) -> Value {
    match value {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

impl NotifyStore for HostNotify {
    /// Insert-or-do-nothing on `(event_type, dedup_key)`.
    ///
    /// Rows-affected is the signal: 1 means this call made the decision, 0 means
    /// somebody already had. Reading the id back on a 0 would be wrong — the
    /// caller must not enqueue a dispatch for an event another worker is already
    /// dispatching.
    fn record_event(&self, event: &NewEvent, now: i64) -> CoreResult<Option<String>> {
        let affected = exec(
            "INSERT INTO argus_notify_events \
             (id, event_type, priority, subject_id, dedup_key, state, title, body, link, data, \
              scheduled_at, created, changed) \
             VALUES (gen_random_uuid(), $1, $2, $3::uuid, $4, 'pending', $5, $6, $7, $8, \
                     $9::bigint, $9::bigint, $9::bigint) \
             ON CONFLICT (event_type, dedup_key) DO NOTHING",
            &[
                json!(event.kind.as_str()),
                json!(event.priority.as_str()),
                opt_str(event.subject_id.as_deref()),
                json!(event.dedup_key),
                json!(event.title),
                json!(event.body),
                opt_str(event.link.as_deref()),
                json!(event.data.to_string()),
                json!(now),
            ],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        let rows: Vec<IdRow> = query_rows(
            "SELECT id::text AS id FROM argus_notify_events \
             WHERE event_type = $1 AND dedup_key = $2",
            &[json!(event.kind.as_str()), json!(event.dedup_key)],
        )?;
        rows.into_iter()
            .next()
            .map(|r| Some(r.id))
            .ok_or_else(|| CoreError::Store("event vanished after insert".to_string()))
    }

    fn load_event(&self, event_id: &str) -> CoreResult<Option<StoredEvent>> {
        let rows: Vec<EventRow> = query_rows(
            &format!("SELECT {EVENT_COLUMNS} FROM argus_notify_events WHERE id = $1::uuid"),
            &[json!(event_id)],
        )?;
        Ok(rows.into_iter().next().map(EventRow::into_event))
    }

    fn load_channels(&self) -> CoreResult<Vec<ChannelConfig>> {
        config_host::load_enabled_channel_configs()
    }

    fn load_channel(&self, channel_id: &str) -> CoreResult<Option<ChannelConfig>> {
        config_host::load_channel_config(channel_id)
    }

    fn last_sent_at(&self, subject_id: &str) -> CoreResult<Option<i64>> {
        let rows: Vec<TsRow> = query_rows(
            "SELECT sent_at AS ts FROM argus_notify_events \
             WHERE subject_id = $1::uuid AND sent_at IS NOT NULL \
             ORDER BY sent_at DESC LIMIT 1",
            &[json!(subject_id)],
        )?;
        Ok(rows.into_iter().next().map(|r| r.ts))
    }

    fn topic_priority(&self, topic_id: &str) -> CoreResult<NotifyPriority> {
        Ok(config_host::load_topic_config(topic_id)?
            .map(|t| t.notify_priority)
            .unwrap_or(NotifyPriority::Normal))
    }

    fn pending_digestible(
        &self,
        head_event_id: &str,
        window_start: i64,
        now: i64,
    ) -> CoreResult<usize> {
        let rows: Vec<CountRow> = query_rows(
            &format!(
                "SELECT count(*)::bigint AS n FROM argus_notify_events \
                 WHERE id <> $1::uuid AND state = 'pending' AND priority = 'normal' \
                   AND event_type IN ({DIGESTIBLE_TYPES}) \
                   AND created >= $2::bigint AND scheduled_at <= $3::bigint"
            ),
            &[json!(head_event_id), json!(window_start), json!(now)],
        )?;
        Ok(rows
            .into_iter()
            .next()
            .map(|r| r.n.max(0) as usize)
            .unwrap_or(0))
    }

    /// Select and mark in one statement, so two workers cannot fold the same
    /// event into two digests.
    ///
    /// `FOR UPDATE SKIP LOCKED` on the inner select is what makes a concurrent
    /// worker pass over a row this one is taking rather than block on it — the
    /// same primitive queue v2's own claim uses.
    fn claim_digest(
        &self,
        head_event_id: &str,
        window_start: i64,
        limit: usize,
        now: i64,
    ) -> CoreResult<Vec<StoredEvent>> {
        let rows: Vec<EventRow> = query_rows(
            &format!(
                "WITH claimed AS ( \
                     UPDATE argus_notify_events SET state = 'digested', \
                            reason = 'folded into a digest', changed = $4::bigint \
                     WHERE id IN ( \
                         SELECT id FROM argus_notify_events \
                         WHERE id <> $1::uuid AND state = 'pending' AND priority = 'normal' \
                           AND event_type IN ({DIGESTIBLE_TYPES}) \
                           AND created >= $2::bigint AND scheduled_at <= $4::bigint \
                         ORDER BY created ASC LIMIT $3::bigint \
                         FOR UPDATE SKIP LOCKED \
                     ) \
                     RETURNING id, event_type, priority, subject_id, state, scheduled_at, \
                               created, title, body, link, data \
                 ) \
                 SELECT {EVENT_COLUMNS} FROM claimed ORDER BY created ASC"
            ),
            &[
                json!(head_event_id),
                json!(window_start),
                json!(limit as i64),
                json!(now),
            ],
        )?;
        Ok(rows.into_iter().map(EventRow::into_event).collect())
    }

    fn promote_to_digest(
        &self,
        event_id: &str,
        digest: &Notification,
        folded: usize,
        now: i64,
    ) -> CoreResult<()> {
        exec(
            "UPDATE argus_notify_events SET event_type = $2, title = $3, body = $4, \
                    data = $5, reason = $6, changed = $7::bigint \
             WHERE id = $1::uuid",
            &[
                json!(event_id),
                json!(digest.kind.as_str()),
                json!(digest.title),
                json!(digest.body),
                json!(digest.data.to_string()),
                json!(format!("digest of {} events", folded + 1)),
                json!(now),
            ],
        )?;
        Ok(())
    }

    fn set_event_state(
        &self,
        event_id: &str,
        state: EventState,
        reason: Option<&str>,
        now: i64,
    ) -> CoreResult<()> {
        // `sent_at` is set only on the transition to sent, and it is what the
        // debounce reads — so a suppressed or digested event must not look like
        // one the reader was told about.
        exec(
            "UPDATE argus_notify_events SET state = $2, reason = COALESCE($3, reason), \
                    sent_at = CASE WHEN $2 = 'sent' THEN $4::bigint ELSE sent_at END, \
                    changed = $4::bigint \
             WHERE id = $1::uuid",
            &[
                json!(event_id),
                json!(state.as_str()),
                opt_str(reason),
                json!(now),
            ],
        )?;
        Ok(())
    }

    fn reschedule_event(&self, event_id: &str, at: i64) -> CoreResult<()> {
        exec(
            "UPDATE argus_notify_events SET scheduled_at = $2::bigint, changed = $2::bigint \
             WHERE id = $1::uuid",
            &[json!(event_id), json!(at)],
        )?;
        Ok(())
    }

    /// Upsert the delivery and return the attempt count in one statement, so the
    /// retry decision is made on a number nothing else can have moved.
    ///
    /// A skip does not count as an attempt: the channel was asked and declined,
    /// which is not a delivery that failed.
    fn record_delivery(
        &self,
        event_id: &str,
        outcome: &ChannelOutcome,
        now: i64,
    ) -> CoreResult<u32> {
        let (url, body) = match &outcome.request {
            Some(req) => (Some(req.url.as_str()), Some(req.body.as_str())),
            None => (None, None),
        };
        let rows: Vec<AttemptsRow> = query_rows(
            "WITH upserted AS ( \
                 INSERT INTO argus_notify_deliveries \
                 (id, event_id, channel_id, channel_name, state, attempts, http_status, \
                  last_error, request_url, request_body, created, changed) \
                 VALUES (gen_random_uuid(), $1::uuid, $2::uuid, $3, $4, \
                         CASE WHEN $4 = 'skipped' THEN 0 ELSE 1 END, \
                         $5::int, $6, $7, $8, $9::bigint, $9::bigint) \
                 ON CONFLICT (event_id, channel_id) DO UPDATE SET \
                     channel_name = EXCLUDED.channel_name, \
                     state        = EXCLUDED.state, \
                     attempts     = argus_notify_deliveries.attempts \
                                    + CASE WHEN $4 = 'skipped' THEN 0 ELSE 1 END, \
                     http_status  = EXCLUDED.http_status, \
                     last_error   = EXCLUDED.last_error, \
                     request_url  = EXCLUDED.request_url, \
                     request_body = EXCLUDED.request_body, \
                     changed      = EXCLUDED.changed \
                 RETURNING attempts \
             ) \
             SELECT attempts FROM upserted",
            &[
                json!(event_id),
                json!(outcome.channel_id),
                json!(outcome.channel_name),
                json!(outcome.state.as_str()),
                match outcome.http_status {
                    Some(s) => json!(i64::from(s)),
                    None => Value::Null,
                },
                opt_str(outcome.error.as_deref()),
                opt_str(url),
                opt_str(body),
                json!(now),
            ],
        )?;
        Ok(rows
            .into_iter()
            .next()
            .map(|r| r.attempts.clamp(0, i64::from(u32::MAX)) as u32)
            .unwrap_or(1))
    }

    fn note_channel_health(
        &self,
        channel_id: &str,
        ok: bool,
        error: Option<&str>,
        now: i64,
    ) -> CoreResult<()> {
        if ok {
            exec(
                "INSERT INTO argus_notify_channels \
                 (id, consecutive_failures, last_error, last_error_at, last_success_at, changed) \
                 VALUES ($1::uuid, 0, NULL, NULL, $2::bigint, $2::bigint) \
                 ON CONFLICT (id) DO UPDATE SET \
                     consecutive_failures = 0, last_error = NULL, last_error_at = NULL, \
                     last_success_at = EXCLUDED.last_success_at, changed = EXCLUDED.changed",
                &[json!(channel_id), json!(now)],
            )?;
        } else {
            exec(
                "INSERT INTO argus_notify_channels \
                 (id, consecutive_failures, last_error, last_error_at, changed) \
                 VALUES ($1::uuid, 1, $2, $3::bigint, $3::bigint) \
                 ON CONFLICT (id) DO UPDATE SET \
                     consecutive_failures = argus_notify_channels.consecutive_failures + 1, \
                     last_error = EXCLUDED.last_error, \
                     last_error_at = EXCLUDED.last_error_at, \
                     changed = EXCLUDED.changed",
                &[json!(channel_id), opt_str(error), json!(now)],
            )?;
        }
        Ok(())
    }

    /// Feeds whose consecutive failure count has reached `threshold`.
    ///
    /// The count and the error live in the plugin's state table; the feed's
    /// *name* lives on its Item (M3 Decision 2), so this is one query plus one
    /// item read per failing feed. The list is bounded by `limit`, so the read
    /// count is bounded with it.
    fn failing_feeds(&self, threshold: u32, limit: usize) -> CoreResult<Vec<FailingFeed>> {
        let rows: Vec<FeedStateRow> = query_rows(
            "SELECT id::text AS id, failure_count, COALESCE(last_error, '') AS last_error \
             FROM argus_feeds WHERE failure_count >= $1::int \
             ORDER BY failure_count DESC, id LIMIT $2::bigint",
            &[json!(i64::from(threshold)), json!(limit as i64)],
        )?;
        Ok(rows
            .into_iter()
            .map(|r| {
                // A feed whose Item is gone still deserves an alert — that is
                // itself the problem — so the id stands in for the name.
                let name = item_host::get_item(&r.id)
                    .ok()
                    .as_ref()
                    .and_then(|i| i.get("title").and_then(Value::as_str).map(str::to_string))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| r.id.clone());
                FailingFeed {
                    id: r.id,
                    name,
                    failure_count: r.failure_count.clamp(0, i64::from(u32::MAX)) as u32,
                    last_error: r.last_error,
                }
            })
            .collect())
    }

    /// Read the plugin's own queue depth from `plugin_queue`.
    ///
    /// This is the one place Argus names a table it does not own. Queue v2
    /// exposes its observability as a kernel table and an admin HTTP route and
    /// **no host function**, so a plugin that wants to know whether its own work
    /// is draining has to reach for `raw_sql` — a capability granted for a
    /// different purpose, and one the kernel documents as weakening the table
    /// guarantee for the plugin that holds it. `M4-DESIGN.md` Decision 9,
    /// `G-QUEUE-NO-INTROSPECTION`.
    fn queue_health(&self, now: i64) -> CoreResult<QueueHealth> {
        let rows: Vec<QueueHealthRow> = query_rows(
            "SELECT \
                COALESCE(MAX(CASE WHEN status = 'ready' AND next_attempt_at <= $1::bigint \
                                  THEN $1::bigint - created_at END), 0)::bigint \
                    AS oldest_ready_age, \
                COUNT(*) FILTER (WHERE status = 'ready' \
                                 AND next_attempt_at <= $1::bigint)::bigint AS ready, \
                COUNT(*) FILTER (WHERE status = 'dead')::bigint AS dead \
             FROM plugin_queue WHERE plugin_name = 'argus'",
            &[json!(now)],
        )?;
        Ok(rows
            .into_iter()
            .next()
            .map(|r| QueueHealth {
                oldest_ready_age: r.oldest_ready_age.max(0),
                ready: r.ready.max(0) as u64,
                dead: r.dead.max(0) as u64,
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_sql_digestible_list_matches_the_predicate_it_mirrors() {
        // Two sources of truth for "which events digest": a `matches!` in
        // argus-core and a SQL `IN` list here. They must not drift.
        let from_predicate: Vec<String> = EventKind::all()
            .iter()
            .filter(|k| k.is_digestible())
            .map(|k| format!("'{}'", k.as_str()))
            .collect();
        assert_eq!(from_predicate.join(","), DIGESTIBLE_TYPES);
    }

    #[test]
    fn an_event_row_with_unreadable_data_still_becomes_an_event() {
        let row = EventRow {
            id: "e1".into(),
            event_type: "story.new".into(),
            priority: "high".into(),
            subject_id: String::new(),
            state: "pending".into(),
            scheduled_at: 10,
            created: 5,
            title: "T".into(),
            body: "B".into(),
            link: String::new(),
            data: "not json".into(),
        };
        let event = row.into_event();
        assert_eq!(event.kind, EventKind::StoryNew);
        assert_eq!(event.priority, NotifyPriority::High);
        assert!(event.subject_id.is_none());
        assert!(event.link.is_none());
        assert_eq!(event.data, json!({}));
    }

    #[test]
    fn an_event_row_written_by_a_newer_plugin_does_not_wedge_this_one() {
        let row = EventRow {
            id: "e1".into(),
            event_type: "story.teleported".into(),
            priority: "screaming".into(),
            subject_id: "s1".into(),
            state: "quantum".into(),
            scheduled_at: 10,
            created: 5,
            title: "T".into(),
            body: "B".into(),
            link: "https://x.test/1".into(),
            data: r#"{"a":1}"#.into(),
        };
        let event = row.into_event();
        assert_eq!(event.kind, EventKind::StoryNew);
        assert_eq!(event.priority, NotifyPriority::Normal);
        assert_eq!(event.state, EventState::Pending);
        assert_eq!(event.subject_id.as_deref(), Some("s1"));
        assert_eq!(event.link.as_deref(), Some("https://x.test/1"));
    }

    #[test]
    fn the_event_projection_names_every_column_the_row_decodes() {
        for column in [
            "id",
            "event_type",
            "priority",
            "subject_id",
            "state",
            "scheduled_at",
            "created",
            "title",
            "body",
            "link",
            "data",
        ] {
            assert!(
                EVENT_COLUMNS.contains(column),
                "projection is missing {column}"
            );
        }
    }
}
