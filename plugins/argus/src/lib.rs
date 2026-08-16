//! Argus plugin for Trovato: a news-intelligence pipeline built as a pure WASM
//! plugin (ARCHITECTURE.md §9, pure-plugin Option A).
//!
//! Articles are lightweight records (`argus_articles`, declared as a
//! `[[record_types]]` type); stories are kernel Items (`argus_story`); and from
//! M3 a feed's and a topic's **configuration** is an Item too (`argus_feed`,
//! `argus_topic`), because the kernel's content forms are the only surface
//! through which an admin can write anything a plugin owns. A feed's mutable
//! fetch state stays in the plugin-owned `argus_feeds` table, keyed by the feed
//! Item's id. `M3-DESIGN.md` argues both halves of that split.
//!
//! The pipeline runs as queue-v2 jobs: `tap_cron` enqueues due feeds, and the
//! single `tap_queue_worker` self-routes each job to a stage handler. All the
//! stage logic lives in the host-agnostic [`argus_core`] crate; this crate
//! supplies the port implementations over kernel host functions (see
//! [`host_ports`] for the pipeline and [`config_host`] for configuration).

use argus_core::budget::{self, BudgetConfig, DailySpend};
use argus_core::cluster::ClusterConfig;
use argus_core::config;
use argus_core::model::Stage;
use argus_core::pipeline::StageConfig;
use argus_core::ports::{AnalysisStore, EnqueueOpts, JobPayload, JobQueue, Store};
use argus_core::ratelimit::NotifyConfig;
use argus_core::{pipeline, schedule};
use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::{ApiRequest, ApiResponse, MenuRoute};

mod config_host;
mod host_ports;
mod intelligence_ports;
mod item_host;
mod notify_ports;
mod reader_api;
mod reader_ports;
mod story_view;

use host_ports::{HostFetcher, HostProvider, HostQueue, HostStore, host_now};
use notify_ports::{HostNotify, HostTransport};

/// Maximum feeds enqueued per cron tick — bounds work under a backlog; the
/// round-robin cursor gives the rest a turn on later ticks.
const MAX_FEEDS_PER_TICK: usize = 50;

/// Permission to manage feeds and topics.
pub const PERM_ADMINISTER: &str = "administer argus";

/// Permission to read stories. Held by the seeded `argus_reader` role.
pub const PERM_VIEW_STORIES: &str = "view argus stories";

/// Permission to react to and bookmark stories.
///
/// Gates the reader-state write API ([`tap_api`]). M3 seeded and checked it with
/// nothing to check it on, because no kernel surface let a reader write a
/// plugin-owned table; `KERNEL_API_VERSION (0,99)` added one.
pub const PERM_REACT: &str = "react to argus stories";

// ===========================================================================
// Content types, permissions, menu
// ===========================================================================

/// The three Item content types Argus declares.
///
/// `argus_story` is the entity readers search and discuss, so it stays an Item
/// for the kernel's semantic search and comment system; its M2 fields carry what
/// a reader needs beyond the narrative — which reports it was synthesized from
/// (`field_sources`, a JSON array), when the synthesis last ran, its span, and
/// whether it is still accepting articles.
///
/// `argus_feed` and `argus_topic` are M3 additions holding **configuration
/// only**. Articles stay a record type; a feed's fetch state stays a plain
/// table.
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "argus_story".into(),
            label: "Story".into(),
            description: "Aggregated narrative clustered from related articles".into(),
            title_label: None,
            fields: vec![
                FieldDefinition::new("field_summary", FieldType::TextLong)
                    .required()
                    .label("Summary"),
                FieldDefinition::new("field_topic_id", FieldType::Text { max_length: None })
                    .label("Topic"),
                FieldDefinition::new("field_article_count", FieldType::Integer)
                    .label("Article Count"),
                FieldDefinition::new("field_relevance_score", FieldType::Float)
                    .label("Relevance Score"),
                FieldDefinition::new("field_sources", FieldType::TextLong).label("Sources"),
                FieldDefinition::new("field_summary_updated", FieldType::Integer)
                    .label("Summary Updated"),
                FieldDefinition::new("field_first_article", FieldType::Integer)
                    .label("First Article"),
                FieldDefinition::new("field_last_article", FieldType::Integer)
                    .label("Last Article"),
                FieldDefinition::new("field_is_active", FieldType::Boolean).label("Active"),
                // Rendered at M2 sync time so the story page does not have to
                // re-derive it per view.
                FieldDefinition::new("field_entities", FieldType::TextLong).label("Top Entities"),
            ],
        },
        // M3: feed and topic *configuration* is an Item so an admin can manage
        // it through the kernel's generic content forms — the only writable
        // surface the frozen contract offers a plugin (`M3-DESIGN.md`
        // Decision 1). Mutable fetch state stays in `argus_feeds` (Decision 2).
        ContentTypeDefinition {
            machine_name: config::FEED_TYPE.into(),
            label: "Argus Feed".into(),
            description: "An RSS or Atom source Argus polls. Unpublish to pause it.".into(),
            title_label: Some("Feed name".into()),
            fields: vec![
                FieldDefinition::new(config::FIELD_URL, FieldType::Text { max_length: None })
                    .required()
                    .label("Feed URL"),
                // A real reference again (M3 deviation 3, un-deviated at K1).
                // M3 had to make this a plain uuid the admin pasted by hand,
                // because the kernel's reference widget wrote a bare id and the
                // form re-read `{target_id}` — so a saved reference blanked
                // itself on the next edit (G-ITEM-FORM-MISMATCH). The form now
                // reads a bare id, and the edit route resolves the target's
                // title, so the widget round-trips and an admin picks a topic by
                // name. The stored value is unchanged (a uuid string), so
                // `parse_feed` reads it exactly as before and no data migrates.
                FieldDefinition::new(
                    config::FIELD_TOPIC,
                    FieldType::RecordReference(config::TOPIC_TYPE.into()),
                )
                .label("Topic"),
                FieldDefinition::new(config::FIELD_FETCH_INTERVAL, FieldType::Integer)
                    .label("Fetch interval (seconds)"),
                FieldDefinition::new(
                    config::FIELD_CONFIG_NOTE,
                    FieldType::Text { max_length: None },
                )
                .label("Last validation note"),
            ],
        },
        ContentTypeDefinition {
            machine_name: config::TOPIC_TYPE.into(),
            label: "Argus Topic".into(),
            description: "Relevance criteria articles are scored against.".into(),
            title_label: Some("Topic name".into()),
            fields: vec![
                FieldDefinition::new(config::FIELD_RELEVANCE_PROMPT, FieldType::TextLong)
                    .required()
                    .label("Relevance prompt"),
                FieldDefinition::new(config::FIELD_RELEVANCE_THRESHOLD, FieldType::Integer)
                    .label("Keep threshold (0-100)"),
                // M4: `high` notifies on every story this topic produces,
                // whatever its relevance score, and bypasses quiet hours.
                FieldDefinition::new(
                    config::FIELD_TOPIC_PRIORITY,
                    FieldType::Text { max_length: None },
                )
                .label("Notification priority (normal or high)"),
                FieldDefinition::new(
                    config::FIELD_CONFIG_NOTE,
                    FieldType::Text { max_length: None },
                )
                .label("Last validation note"),
            ],
        },
        // M4: a notification channel is configuration, so it is an Item for the
        // same reason a feed is — the kernel's content forms are the only
        // writable surface a plugin gets (`M4-DESIGN.md` Decision 1).
        // Unpublish to pause a channel; there is no second enabled flag to
        // disagree with the publish checkbox.
        ContentTypeDefinition {
            machine_name: config::CHANNEL_TYPE.into(),
            label: "Argus Notification Channel".into(),
            description: "Where Argus sends notifications. Unpublish to pause it.".into(),
            title_label: Some("Channel name".into()),
            fields: vec![
                FieldDefinition::new(
                    config::FIELD_CHANNEL_KIND,
                    FieldType::Text { max_length: None },
                )
                .required()
                .label("Kind (ntfy, slack or webhook)"),
                FieldDefinition::new(
                    config::FIELD_CHANNEL_TARGET,
                    FieldType::Text { max_length: None },
                )
                .required()
                .label("Target (ntfy topic name, or the Slack/webhook URL)"),
                FieldDefinition::new(
                    config::FIELD_CHANNEL_SERVER,
                    FieldType::Text { max_length: None },
                )
                .label("ntfy server (blank for https://ntfy.sh)"),
                FieldDefinition::new(config::FIELD_CHANNEL_HEADERS, FieldType::TextLong)
                    .label("Extra headers, as a JSON object (webhook only)"),
                FieldDefinition::new(
                    config::FIELD_CHANNEL_MIN_PRIORITY,
                    FieldType::Text { max_length: None },
                )
                .label("Minimum priority (normal or high)"),
                FieldDefinition::new(
                    config::FIELD_CHANNEL_EVENTS,
                    FieldType::Text { max_length: None },
                )
                .label("Events, comma separated (blank for all)"),
                FieldDefinition::new(
                    config::FIELD_CHANNEL_NTFY_PRIORITY,
                    FieldType::Text { max_length: None },
                )
                .label("ntfy priority override (min, low, default, high, urgent)"),
                FieldDefinition::new(
                    config::FIELD_CONFIG_NOTE,
                    FieldType::Text { max_length: None },
                )
                .label("Last validation note"),
            ],
        },
    ]
}

/// CRUD permissions for every Argus Item type, plus the reader and administer
/// permissions the seeded roles are built from.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    let mut perms = PermissionDefinition::crud_for_type("argus_story");
    perms.extend(PermissionDefinition::crud_for_type(config::FEED_TYPE));
    perms.extend(PermissionDefinition::crud_for_type(config::TOPIC_TYPE));
    perms.extend(PermissionDefinition::crud_for_type(config::CHANNEL_TYPE));
    perms.push(PermissionDefinition::new(
        PERM_ADMINISTER,
        "Administer Argus feeds and topics",
    ));
    perms.push(PermissionDefinition::new(
        PERM_VIEW_STORIES,
        "View Argus stories",
    ));
    perms.push(PermissionDefinition::new(
        PERM_REACT,
        "React to and bookmark Argus stories",
    ));
    perms
}

/// Navigation.
///
/// Every entry here is a link with a permission, not a handler: the kernel's
/// Navigation entries plus the reader-state write API.
///
/// The `page` entries resolve because the plugin's migrations register them as
/// `url_alias` rows onto gather queries and kernel admin routes.
///
/// The `api` entries are new at `KERNEL_API_VERSION (0,99)`: an entry with
/// `handler_type = "api"` and a `callback` is dispatched to [`tap_api`] with
/// the authenticated user and a live services handle. That is what finally
/// gives `argus_reactions` and `argus_subscriptions` a writer, undoing M3
/// deviation 5 (`G-NO-PLUGIN-HTTP`). Every write route is gated on
/// [`PERM_REACT`], which the kernel checks before dispatch.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::page("/stories", "Stories").permission("access content"),
        MenuRoute::page("/articles", "Articles").permission("access content"),
        MenuRoute::page("/admin/argus/feeds", "Argus feeds")
            .permission(PERM_ADMINISTER)
            .parent("/admin"),
        MenuRoute::page("/admin/argus/topics", "Argus topics")
            .permission(PERM_ADMINISTER)
            .parent("/admin"),
        MenuRoute::page("/admin/argus/channels", "Argus notification channels")
            .permission(PERM_ADMINISTER)
            .parent("/admin"),
        MenuRoute::page("/admin/argus/notifications", "Argus notifications")
            .permission(PERM_ADMINISTER)
            .parent("/admin"),
        // Reader state, writable at last.
        MenuRoute::api("POST", "/argus/story/:id/react", reader_api::CB_REACT)
            .title("React to a story")
            .permission(PERM_REACT),
        MenuRoute::api(
            "GET",
            "/argus/story/:id/reactions",
            reader_api::CB_REACTIONS,
        )
        .title("My reactions to a story")
        .permission(PERM_REACT),
        MenuRoute::api("POST", "/argus/story/:id/read", reader_api::CB_MARK_READ)
            .title("Mark a story read")
            .permission(PERM_VIEW_STORIES),
        MenuRoute::api(
            "PUT",
            "/argus/topic/:id/subscribe",
            reader_api::CB_SUBSCRIBE,
        )
        .title("Subscribe to a topic")
        .permission(PERM_REACT),
    ]
}

/// Serve one reader-state request.
///
/// The kernel has already checked the menu entry's permission, so this only has
/// to route on the callback and do the work.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    reader_api::dispatch(&request).unwrap_or_else(|| ApiResponse::error(404, "unknown callback"))
}

// ===========================================================================
// Reader surface and admin validation (M3)
// ===========================================================================

/// Render the story page fragment, and record that this reader saw the story.
///
/// The kernel appends whatever this returns to the item page's children
/// verbatim, so [`story_view::render`] escapes everything it interpolates.
///
/// The read-state write rides along here because this is the only tap the
/// kernel dispatches on a reader's request with both an authenticated user and
/// a services handle — `ItemService::load_for_view` builds the tap state from
/// the viewer and the shared services. A view is idempotent to record, so
/// writing on a GET is safe (`M3-DESIGN.md` Decision 5).
#[plugin_tap]
pub fn tap_item_view(input: serde_json::Value) -> String {
    if input.get("type").and_then(serde_json::Value::as_str) != Some("argus_story") {
        return String::new();
    }

    // Best-effort throughout: a failed reader-state read or write must never
    // cost a reader the page they asked for, so each is logged and swallowed
    // rather than propagated.
    let mut reactions = Vec::new();
    if let Some(story_id) = input.get("id").and_then(serde_json::Value::as_str) {
        let user_id = host::current_user_id();
        if !user_id.is_empty() && !is_nil_uuid(&user_id) {
            match host_now() {
                Ok(now) => {
                    if let Err(e) = reader_ports::record_view(&user_id, story_id, now) {
                        host::log(
                            "warning",
                            "argus",
                            &format!("tap_item_view: read state for {story_id}: {e}"),
                        );
                    }
                }
                Err(e) => host::log(
                    "warning",
                    "argus",
                    &format!("tap_item_view: clock read failed: {e}"),
                ),
            }
            match reader_ports::load_reactions(&user_id, story_id) {
                Ok(held) => reactions = held,
                Err(e) => host::log(
                    "warning",
                    "argus",
                    &format!("tap_item_view: reactions for {story_id}: {e}"),
                ),
            }
        }
    }

    story_view::render(&input, &reactions)
}

/// Coerce an admin's feed or topic edit into a usable configuration.
///
/// `tap_item_presave` can **modify but not refuse**, and it can only modify
/// `fields`: the kernel merges the `fields` object it gets back and ignores
/// everything else, then saves unconditionally (`G-NO-PRESAVE-VETO`). So an
/// interval outside its bounds is clamped and a threshold outside `0..=100` is
/// clamped, both with the change reported in the note field rather than applied
/// silently.
///
/// A URL that is not a URL cannot be refused *and* cannot be parked — presave
/// has no way to unpublish the Item. It is blanked here, and
/// [`config_host::load_enabled_feed_configs`] declines to schedule a feed with
/// no usable URL, so the enforcement lands where it can actually be enforced.
#[plugin_tap]
pub fn tap_item_presave(input: serde_json::Value) -> serde_json::Value {
    let item_type = input
        .get("item_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let fields = input
        .get("fields")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    match item_type {
        t if t == config::FEED_TYPE => {
            let coerced = config::coerce_feed(&fields);
            serde_json::json!({
                "fields": {
                    config::FIELD_URL: coerced.url,
                    config::FIELD_FETCH_INTERVAL: coerced.interval_seconds,
                    config::FIELD_CONFIG_NOTE: coerced.note,
                }
            })
        }
        t if t == config::TOPIC_TYPE => {
            let coerced = config::coerce_topic(&fields);
            serde_json::json!({
                "fields": {
                    config::FIELD_RELEVANCE_THRESHOLD: coerced.threshold,
                    config::FIELD_TOPIC_PRIORITY: coerced.notify_priority,
                    config::FIELD_CONFIG_NOTE: coerced.note,
                }
            })
        }
        t if t == config::CHANNEL_TYPE => {
            let coerced = config::coerce_channel(&fields);
            serde_json::json!({
                "fields": {
                    config::FIELD_CHANNEL_KIND: coerced.kind,
                    config::FIELD_CHANNEL_TARGET: coerced.target,
                    config::FIELD_CHANNEL_SERVER: coerced.server,
                    config::FIELD_CHANNEL_HEADERS: coerced.headers,
                    config::FIELD_CHANNEL_MIN_PRIORITY: coerced.min_priority,
                    config::FIELD_CHANNEL_EVENTS: coerced.events,
                    config::FIELD_CHANNEL_NTFY_PRIORITY: coerced.ntfy_priority,
                    config::FIELD_CONFIG_NOTE: coerced.note,
                }
            })
        }
        _ => serde_json::json!({}),
    }
}

/// Retire a feed's or a channel's state row when its configuration Item is
/// deleted.
///
/// Without this the row outlives the thing it describes: a stale
/// `last_fetched_at` and ETag, or a stale failure streak, keyed by an id nothing
/// resolves any more, which would be handed straight back if the id were ever
/// reused.
#[plugin_tap]
pub fn tap_item_delete(input: serde_json::Value) -> serde_json::Value {
    let item_type = input
        .get("type")
        .or_else(|| input.get("item_type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let table = match item_type {
        t if t == config::FEED_TYPE => "argus_feeds",
        t if t == config::CHANNEL_TYPE => "argus_notify_channels",
        _ => return serde_json::json!({}),
    };
    let Some(id) = input.get("id").and_then(serde_json::Value::as_str) else {
        return serde_json::json!({});
    };
    // The table name is one of two compile-time literals chosen above, never
    // anything the caller supplied.
    match host_ports::exec(
        &format!("DELETE FROM {table} WHERE id = $1::uuid"),
        &[serde_json::json!(id)],
    ) {
        Ok(n) => serde_json::json!({ "state_rows_removed": n, "table": table }),
        Err(e) => {
            host::log(
                "warning",
                "argus",
                &format!("tap_item_delete: {table} state for {id}: {e}"),
            );
            serde_json::json!({ "error": e.to_string() })
        }
    }
}

/// Whether a uuid string is the nil uuid, which is what an anonymous viewer's
/// id resolves to.
fn is_nil_uuid(id: &str) -> bool {
    id.chars().all(|c| c == '0' || c == '-')
}

// ===========================================================================
// Pipeline: scheduling (tap_cron) + stage worker (tap_queue_worker)
// ===========================================================================

/// Queue declarations and concurrency hints. The kernel reads the max
/// concurrency and clamps to `[1, 4]`.
#[plugin_tap]
pub fn tap_queue_info() -> serde_json::Value {
    serde_json::json!([
        { "name": Stage::Fetch.queue_name(), "concurrency": 4 },
        { "name": Stage::Decide.queue_name(), "concurrency": 4 },
        { "name": Stage::Analyze.queue_name(), "concurrency": 2 },
        { "name": Stage::Embed.queue_name(), "concurrency": 2 },
        { "name": Stage::Cluster.queue_name(), "concurrency": 1 },
        { "name": Stage::Summarize.queue_name(), "concurrency": 1 },
        { "name": Stage::Notify.queue_name(), "concurrency": 2 },
    ])
}

/// Enqueue due feeds for fetching, round-robin, once per cron cycle.
///
/// Never panics: `tap_cron` shares one dispatch budget across all plugins, so
/// any failure is logged and reported, not raised.
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    let store = HostStore;
    let queue = HostQueue;
    let now = input.timestamp;

    // M3: carry any surviving M1/M2 configuration rows onto Items before
    // anything reads configuration. A migration cannot do this (the item_type
    // rows a feed Item references are written at runtime, after migrations run —
    // `M3-DESIGN.md` Decision 3), and a failure here must not stop the pipeline,
    // so it is logged and the next cycle retries.
    let backfill = match config_host::backfill_legacy_config() {
        Ok(report) if report.already_done => serde_json::json!({ "already_done": true }),
        Ok(report) => {
            host::log(
                "info",
                "argus",
                &format!(
                    "config backfill carried {} topics and {} feeds onto Items",
                    report.topics, report.feeds
                ),
            );
            serde_json::json!({ "topics": report.topics, "feeds": report.feeds })
        }
        Err(e) => {
            host::log(
                "warning",
                "argus",
                &format!("tap_cron: config backfill failed: {e}"),
            );
            serde_json::json!({ "error": e.to_string() })
        }
    };

    let feeds = match store.load_enabled_feeds() {
        Ok(f) => f,
        Err(e) => {
            host::log(
                "error",
                "argus",
                &format!("tap_cron: load feeds failed: {e}"),
            );
            return serde_json::json!({ "error": e.to_string() });
        }
    };
    let cursor = store.load_cursor().unwrap_or(0);
    let selection = schedule::select_due(&feeds, now, cursor, MAX_FEEDS_PER_TICK);

    let mut enqueued = 0usize;
    for feed_id in &selection.due {
        let job = JobPayload::new(Stage::Fetch, feed_id.clone());
        match queue.enqueue(&job, EnqueueOpts::default()) {
            Ok(()) => enqueued += 1,
            Err(e) => host::log(
                "warning",
                "argus",
                &format!("tap_cron: enqueue {feed_id} failed: {e}"),
            ),
        }
    }
    if let Err(e) = store.save_cursor(selection.next_cursor) {
        host::log(
            "warning",
            "argus",
            &format!("tap_cron: save cursor failed: {e}"),
        );
    }

    // `tap_cron` fires every cycle with a timestamp and no cron key, so a
    // plugin with several periodic duties multiplexes them here. Maintenance
    // failures are logged, never raised: this tap shares one dispatch budget
    // with every other plugin.
    let config = stage_config();
    let maintenance = match pipeline::run_maintenance(&store, &store, &queue, &config, now) {
        Ok(report) => serde_json::json!({
            "stories_retired": report.stories_retired,
            "articles_purged": report.articles_purged,
            "waiting_requeued": report.waiting_requeued,
        }),
        Err(e) => {
            host::log(
                "warning",
                "argus",
                &format!("tap_cron: maintenance failed: {e}"),
            );
            serde_json::json!({ "error": e.to_string() })
        }
    };

    let (spend_report, spend) = report_spend(&config, now);

    // M4: the operator alert pass. Same cadence and the same "log, never raise"
    // discipline as maintenance — `tap_cron` shares one dispatch budget with
    // every other plugin, so a notification failure must not cost every plugin
    // its cron cycle.
    let notify_config = notify_config();
    let alerts = match pipeline::run_alerts(
        &HostNotify,
        &queue,
        &spend,
        &config.budget,
        &notify_config,
        now,
    ) {
        Ok(report) => serde_json::json!({
            "feeds_failing": report.feeds_failing,
            "budget_alerted": report.budget_alerted,
            "queue_alerted": report.queue_alerted,
            "events_recorded": report.events_recorded,
        }),
        Err(e) => {
            host::log("warning", "argus", &format!("tap_cron: alerts failed: {e}"));
            serde_json::json!({ "error": e.to_string() })
        }
    };

    serde_json::json!({
        "enabled_feeds": feeds.len(),
        "due": selection.due.len(),
        "enqueued": enqueued,
        "maintenance": maintenance,
        "spend": spend_report,
        "alerts": alerts,
        "config_backfill": backfill,
    })
}

/// Read today's spend, emit the budget warning if one is due, and return the
/// figures for the cron result.
///
/// This is the operator-readable surface for spend: `tap_cron`'s result JSON is
/// recorded per cycle, so "what did Argus spend today, and on what" is
/// answerable without a SQL client. `unpriced_calls` is reported alongside the
/// dollar figure because an unpriced model is unknown spend, not free spend.
fn report_spend(config: &StageConfig, now: i64) -> (serde_json::Value, DailySpend) {
    use argus_core::budget::{BudgetVerdict, utc_day, verdict};

    let day = utc_day(now);
    let spend = match HostStore.load_daily_spend(&day) {
        Ok(s) => s,
        Err(e) => {
            host::log("warning", "argus", &format!("tap_cron: spend read: {e}"));
            // Zero spend rather than a guess: the alert pass reads this, and an
            // invented figure would either alert on nothing or silence a real
            // overspend. A failed read is "unknown", and unknown does not alert.
            return (
                serde_json::json!({ "error": e.to_string() }),
                DailySpend::default(),
            );
        }
    };
    match verdict(spend.spent_usd, &config.budget) {
        BudgetVerdict::Pause => host::log(
            "warning",
            "argus",
            &format!(
                "daily AI budget reached on {day}: ${:.4} of ${:.4}; analyze and summarize are paused until tomorrow",
                spend.spent_usd, config.budget.daily_limit_usd
            ),
        ),
        BudgetVerdict::Warn => host::log(
            "warning",
            "argus",
            &format!(
                "daily AI spend past the alert threshold on {day}: ${:.4} of ${:.4}",
                spend.spent_usd, config.budget.alert_threshold_usd
            ),
        ),
        BudgetVerdict::Ok => {}
    }
    (
        serde_json::json!({
            "day": day,
            "usd": spend.spent_usd,
            "calls": spend.calls,
            "unpriced_calls": spend.unpriced_calls,
            "by_stage": spend_by_stage(&day),
        }),
        spend,
    )
}

/// Today's spend broken down by stage, for the operator-readable cron result.
fn spend_by_stage(day: &str) -> serde_json::Value {
    #[derive(serde::Deserialize)]
    struct StageSpend {
        stage: String,
        calls: i64,
        unpriced_calls: i64,
        cost_usd: f64,
    }
    let rows: Result<Vec<StageSpend>, _> = host_ports::query_rows(
        "SELECT stage, calls, unpriced_calls, cost_usd FROM argus_cost_daily \
         WHERE day = $1 ORDER BY stage",
        &[serde_json::json!(day)],
    );
    match rows {
        Ok(rows) => serde_json::json!(
            rows.into_iter()
                .map(|r| serde_json::json!({
                    "stage": r.stage,
                    "calls": r.calls,
                    "unpriced_calls": r.unpriced_calls,
                    "usd": r.cost_usd,
                }))
                .collect::<Vec<_>>()
        ),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}

/// The single queue worker for every pipeline stage.
///
/// Self-routes by the payload's `stage` discriminator (the kernel drains by
/// plugin, not by queue, and hands over the bare payload). Retry semantics: a
/// normal return is success (the job row is deleted), so a transient failure
/// must `panic!` to make queue v2 retry with backoff and eventually
/// dead-letter. Permanent outcomes are recorded as terminal state by the stage
/// logic and return normally.
#[plugin_tap]
pub fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    let job: JobPayload = match serde_json::from_value(input) {
        Ok(j) => j,
        Err(e) => {
            // A malformed payload cannot be retried into validity; log and
            // succeed so it does not dead-letter forever.
            host::log(
                "error",
                "argus",
                &format!("tap_queue_worker: bad payload: {e}"),
            );
            return serde_json::json!({ "status": "error", "reason": "bad payload" });
        }
    };

    match run_stage(&job) {
        Ok(value) => value,
        Err(e) if e.is_transient() => {
            host::log(
                "error",
                "argus",
                &format!("stage {:?} transient failure (retrying): {e}", job.stage),
            );
            // Panic → queue v2 counts a failed attempt → backoff/retry/DLQ.
            panic!("argus stage {:?} transient failure: {e}", job.stage);
        }
        Err(e) => {
            host::log(
                "warning",
                "argus",
                &format!("stage {:?} permanent failure: {e}", job.stage),
            );
            serde_json::json!({ "status": "error", "reason": e.to_string() })
        }
    }
}

/// Dispatch one job to its stage handler in [`argus_core::pipeline`].
fn run_stage(job: &JobPayload) -> argus_core::CoreResult<serde_json::Value> {
    match job.stage {
        Stage::Fetch => {
            let now = host_now()?;
            let report = pipeline::run_fetch(&HostFetcher, &HostStore, &HostQueue, &job.id, now)?;
            Ok(serde_json::json!({
                "stage": "fetch",
                "parsed": report.parsed,
                "ingested": report.ingested,
                "not_modified": report.not_modified,
                "feed_flagged": report.feed_flagged,
            }))
        }
        Stage::Decide => {
            let report = pipeline::run_decide(
                &HostProvider,
                &HostStore,
                &HostQueue,
                &job.id,
                decide_model(),
            )?;
            // Decide is not *gated* by the daily budget (see budget.rs for why)
            // but it is *counted*: it runs on every ingested article and is the
            // pipeline's cost floor, so a spend figure that omitted it would
            // understate the day by more than every other stage combined. A
            // missing article means no call was made and nothing to record.
            if !report.missing {
                let now = host_now()?;
                AnalysisStore::record_cost(
                    &HostStore,
                    &budget::utc_day(now),
                    Stage::Decide,
                    report.cost_estimate,
                    now,
                )?;
            }
            Ok(serde_json::json!({
                "stage": "decide",
                "score": report.score,
                "kept": report.kept,
                "missing": report.missing,
                // Cost accounted from the response (G-COST-OPAQUE fixed by p11j);
                // null when the model is unpriced or no provider is configured.
                "cost": report.cost_estimate,
            }))
        }
        Stage::Analyze => {
            let now = host_now()?;
            let report = pipeline::run_analyze(
                &HostProvider,
                &HostStore,
                &HostStore,
                &HostQueue,
                &job.id,
                analyze_model(),
                &stage_config(),
                now,
            )?;
            Ok(serde_json::json!({
                "stage": "analyze",
                "missing": report.missing,
                "paused": report.paused,
                "unparseable": report.unparseable,
                "entities_created": report.entities.created,
                "entities_linked": report.entities.linked,
                "aliases_added": report.entities.aliases_added,
                "cost": report.cost_estimate,
            }))
        }
        Stage::Embed => {
            let now = host_now()?;
            let report = pipeline::run_embed(
                &HostStore,
                &HostStore,
                &HostQueue,
                &HostProvider,
                &job.id,
                &stage_config(),
                now,
            )?;
            Ok(serde_json::json!({
                "stage": "embed",
                "missing": report.missing,
                "empty": report.empty,
            }))
        }
        Stage::Cluster => {
            let now = host_now()?;
            let report = pipeline::run_cluster(
                &HostStore,
                &HostStore,
                &HostQueue,
                &job.id,
                &stage_config(),
                now,
            )?;
            Ok(serde_json::json!({
                "stage": "cluster",
                "missing": report.missing,
                "re_embedded": report.re_embedded,
                "candidates": report.candidates,
                "story_id": report.story_id,
                "decision": report.decision.map(|d| format!("{d:?}")),
            }))
        }
        Stage::Summarize => {
            let now = host_now()?;
            let report = pipeline::run_summarize(
                &HostProvider,
                &HostStore,
                &HostStore,
                &HostQueue,
                &job.id,
                summarize_model(),
                &stage_config(),
                now,
            )?;
            // M4: the notification decision is made here rather than inside
            // run_summarize because the trigger's key must be deterministic in
            // the story and its member count — never in the model's wording —
            // and because a judge call inside the summarize job would be the
            // second AI call in one job.
            let notified = match &report.change {
                Some(change) => pipeline::notify_story_change(
                    &HostNotify,
                    &HostQueue,
                    change,
                    &notify_config(),
                    now,
                )?,
                None => None,
            };
            Ok(serde_json::json!({
                "stage": "summarize",
                "missing": report.missing,
                "paused": report.paused,
                "deferred_seconds": report.deferred_seconds,
                "empty": report.empty,
                "unparseable": report.unparseable,
                "members": report.members,
                "cost": report.cost_estimate,
                "notify_event": notified,
            }))
        }
        Stage::Notify => {
            let now = host_now()?;
            let config = notify_config();
            let report = pipeline::run_notify(
                &HostTransport,
                &HostNotify,
                &HostStore,
                &HostProvider,
                &HostQueue,
                &job.id,
                job.channel.as_deref(),
                judge_model(),
                &config,
                &stage_config().budget,
                now,
            )?;
            Ok(serde_json::json!({
                "stage": "notify",
                "channel": job.channel,
                "missing": report.missing,
                "already_handled": report.already_handled,
                "deferred_seconds": report.deferred_seconds,
                "paused": report.paused,
                "suppressed": report.suppressed,
                "material": report.material,
                "judge_reason": report.judge_reason,
                "digested": report.digested,
                "delivered": report.delivered,
                "failed": report.failed,
                "blocked": report.blocked,
                "skipped": report.skipped,
                "requeued": report.requeued,
                "cost": report.cost_estimate,
            }))
        }
    }
}

/// The model the decide stage routes to (a cheap model). Read from the
/// `argus.decide_model` site variable; empty means "use the provider default".
fn decide_model() -> Option<String> {
    variable_opt("argus.decide_model")
}

/// The model the analyze stage routes to (the strong model). Per-stage routing
/// exists for exactly this: decide runs on every ingested article and analyze
/// only on survivors, so they belong on different models.
fn analyze_model() -> Option<String> {
    variable_opt("argus.analyze_model")
}

/// The model the summarize stage routes to. Falls back to the analyze model,
/// since synthesis is the same class of work as analysis.
fn summarize_model() -> Option<String> {
    variable_opt("argus.summarize_model").or_else(analyze_model)
}

/// The model the notification change judge routes to. Falls back to the decide
/// model: judging whether two paragraphs say the same thing is the same class of
/// cheap, high-volume work as scoring an article's relevance.
fn judge_model() -> Option<String> {
    variable_opt("argus.judge_model").or_else(decide_model)
}

/// Read a boolean site variable. Accepts the spellings an operator actually
/// types; anything else falls back to `default` rather than silently reading as
/// false.
fn variable_bool(name: &str, default: bool) -> bool {
    match variable_opt(name)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

/// Assemble the M4 notification configuration from site variables.
///
/// Same contract as [`stage_config`]: every value has a working default, so an
/// operator who configures nothing gets sensible behaviour — quiet overnight,
/// one notification per story per hour, digests past five, and only stories that
/// scored 70 or better. What they must configure is a *channel*, because a site
/// with no channels notifies nobody and that is the only safe default for
/// something that sends messages.
fn notify_config() -> NotifyConfig {
    let defaults = NotifyConfig::default();
    NotifyConfig {
        debounce_seconds: variable_num("argus.notify_debounce_seconds", defaults.debounce_seconds),
        digest_threshold: variable_num("argus.digest_threshold", defaults.digest_threshold),
        digest_window_seconds: variable_num(
            "argus.digest_window_seconds",
            defaults.digest_window_seconds,
        ),
        quiet_start_hour: variable_num("argus.quiet_hours_start", defaults.quiet_start_hour),
        quiet_end_hour: variable_num("argus.quiet_hours_end", defaults.quiet_end_hour),
        quiet_utc_offset_minutes: variable_num(
            "argus.quiet_hours_utc_offset_minutes",
            defaults.quiet_utc_offset_minutes,
        ),
        quiet_hours_alerts: variable_bool("argus.quiet_hours_alerts", defaults.quiet_hours_alerts),
        notify_threshold: variable_num("argus.notify_threshold", defaults.notify_threshold),
        judge_enabled: variable_bool("argus.notify_judge", defaults.judge_enabled),
        change_ratio: variable_num("argus.notify_change_ratio", defaults.change_ratio),
        retry_base_seconds: variable_num(
            "argus.notify_retry_base_seconds",
            defaults.retry_base_seconds,
        ),
        max_delivery_attempts: variable_num(
            "argus.notify_max_attempts",
            defaults.max_delivery_attempts,
        ),
        alerts_enabled: variable_bool("argus.alerts_enabled", defaults.alerts_enabled),
        feed_failure_threshold: variable_num(
            "argus.feed_failure_threshold",
            defaults.feed_failure_threshold,
        ),
        queue_stuck_seconds: variable_num(
            "argus.queue_stuck_seconds",
            defaults.queue_stuck_seconds,
        ),
    }
}

/// Read a site variable, returning `None` when unset or empty.
fn variable_opt(name: &str) -> Option<String> {
    match host::variables_get(name, "") {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Read a numeric site variable, falling back to `default` when it is unset or
/// unparseable. A malformed variable must not stop the pipeline.
fn variable_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    variable_opt(name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Assemble the M2 stage configuration from site variables.
///
/// Every value has a working default, so an operator who configures nothing
/// still gets a running pipeline; the ones worth setting are the two budget
/// figures, which default to "no limit" precisely so that spend is a decision
/// an operator makes rather than one this plugin makes for them.
fn stage_config() -> StageConfig {
    let defaults = StageConfig::default();
    let embed_model = embed_model();
    // The join threshold is calibrated per vector space: semantic cosine
    // between two re-reports of one event runs far higher than lexical cosine,
    // and semantic cosine between two *unrelated* articles rarely falls as low
    // as the lexical default, so carrying 0.55 onto the semantic route would
    // join nearly everything to everything.
    let default_threshold = if embed_model.is_some() {
        argus_core::cluster::DEFAULT_SEMANTIC_JOIN_THRESHOLD
    } else {
        defaults.cluster.join_threshold
    };
    StageConfig {
        vector_dim: variable_num("argus.vector_dim", defaults.vector_dim),
        entity_threshold: variable_num("argus.entity_match_threshold", defaults.entity_threshold),
        cluster: ClusterConfig {
            join_threshold: variable_num("argus.cluster_threshold", default_threshold),
            near_dup_threshold: variable_num(
                "argus.near_dup_threshold",
                defaults.cluster.near_dup_threshold,
            ),
            window_seconds: variable_num(
                "argus.cluster_window_seconds",
                defaults.cluster.window_seconds,
            ),
            inactive_seconds: variable_num(
                "argus.story_inactive_seconds",
                defaults.cluster.inactive_seconds,
            ),
            max_waits: variable_num("argus.max_cluster_waits", defaults.cluster.max_waits),
        },
        summarize_min_interval: variable_num(
            "argus.summarize_min_interval",
            defaults.summarize_min_interval,
        ),
        article_retention_days: variable_num(
            "argus.article_retention_days",
            defaults.article_retention_days,
        ),
        budget: BudgetConfig {
            daily_limit_usd: variable_num("argus.daily_limit_usd", defaults.budget.daily_limit_usd),
            alert_threshold_usd: variable_num(
                "argus.alert_threshold_usd",
                defaults.budget.alert_threshold_usd,
            ),
        },
        embed_model,
    }
}

/// The embeddings model the embed stage routes to, from `argus.embed_model`.
///
/// Unset keeps M2's deterministic lexical vectors, which need no provider,
/// spend nothing and cannot fail — the right default for a site that has
/// configured no embeddings provider. Setting it switches the stage to real
/// semantic embeddings, which the kernel could not serve at all until
/// `KERNEL_API_VERSION (0,99)` routed `operation: Embedding` to an embeddings
/// endpoint (**G-AI-EMBED-UNROUTED**).
///
/// Switching in either direction changes the stored vector recipe, so existing
/// vectors stop being comparable and the cluster stage re-enqueues an embed job
/// per article rather than mixing two vector spaces.
fn embed_model() -> Option<String> {
    variable_opt("argus.embed_model")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_info_declares_the_story_and_its_configuration_types() {
        let types = __inner_tap_item_info();
        let names: Vec<&str> = types.iter().map(|t| t.machine_name.as_str()).collect();
        // M3 added the two configuration types: an admin manages a feed and a
        // topic through the kernel's content forms, which requires them to be
        // Items (M3-DESIGN.md Decision 1). M4 added a third for the same reason.
        assert_eq!(
            names,
            vec![
                "argus_story",
                "argus_feed",
                "argus_topic",
                "argus_notify_channel"
            ]
        );
    }

    #[test]
    fn the_feed_type_declares_every_field_the_config_reader_looks_for() {
        let types = __inner_tap_item_info();
        let feed = types
            .iter()
            .find(|t| t.machine_name == config::FEED_TYPE)
            .expect("feed type declared");
        let fields: Vec<&str> = feed.fields.iter().map(|f| f.field_name.as_str()).collect();
        for expected in [
            config::FIELD_URL,
            config::FIELD_TOPIC,
            config::FIELD_FETCH_INTERVAL,
            config::FIELD_CONFIG_NOTE,
        ] {
            assert!(fields.contains(&expected), "missing {expected}: {fields:?}");
        }
    }

    /// **M3 deviation 3, un-deviated.** The feed's topic is a real reference
    /// again, now that the kernel's reference widget round-trips
    /// (G-ITEM-FORM-MISMATCH, K1 fix 4). M3 had to ship a plain `Text` uuid
    /// the admin pasted by hand, because a saved reference blanked itself on
    /// the next edit.
    #[test]
    fn the_feeds_topic_is_a_record_reference_again() {
        let types = __inner_tap_item_info();
        let feed = types
            .iter()
            .find(|t| t.machine_name == config::FEED_TYPE)
            .expect("feed type declared");
        let topic = feed
            .fields
            .iter()
            .find(|f| f.field_name == config::FIELD_TOPIC)
            .expect("topic field declared");
        match &topic.field_type {
            FieldType::RecordReference(target) => assert_eq!(target, config::TOPIC_TYPE),
            other => panic!(
                "expected a reference to {}, got {other:?}",
                config::TOPIC_TYPE
            ),
        }
    }

    /// The stored shape is unchanged by the un-deviation — still a bare uuid
    /// string — so `parse_feed` reads it exactly as before and no feed Item
    /// needs migrating.
    #[test]
    fn a_feeds_topic_still_parses_as_a_bare_uuid_string() {
        let feed = config::parse_feed(&serde_json::json!({
            "id": "019ffc00-0000-7000-8000-000000000001",
            "title": "A feed",
            "status": 1,
            "fields": {
                config::FIELD_URL: "https://example.test/feed.xml",
                config::FIELD_TOPIC: "019ffc00-0000-7000-8000-0000000000aa",
            }
        }))
        .expect("feed parses");
        assert_eq!(feed.topic_id, "019ffc00-0000-7000-8000-0000000000aa");
    }

    #[test]
    fn presave_coerces_a_feed_and_ignores_a_foreign_type() {
        let out = __inner_tap_item_presave(serde_json::json!({
            "item_type": config::FEED_TYPE,
            "fields": { config::FIELD_URL: "https://example.test/f", config::FIELD_FETCH_INTERVAL: 1 }
        }));
        assert_eq!(
            out["fields"][config::FIELD_FETCH_INTERVAL],
            config::MIN_FETCH_INTERVAL_SECONDS
        );

        let out = __inner_tap_item_presave(serde_json::json!({
            "item_type": "blog",
            "fields": { config::FIELD_URL: "nonsense" }
        }));
        assert!(out.get("fields").is_none(), "left another type alone");
    }

    #[test]
    fn perm_includes_crud_for_every_declared_type_plus_the_reader_permissions() {
        let perms = __inner_tap_perm();
        let names: Vec<&str> = perms.iter().map(|p| p.name.as_str()).collect();
        for expected in [
            PERM_ADMINISTER,
            PERM_VIEW_STORIES,
            PERM_REACT,
            "create argus_feed content",
            "edit argus_topic content",
        ] {
            assert!(names.contains(&expected), "missing {expected:?}: {names:?}");
        }
        assert!(names.iter().any(|n| n.contains("argus_story")));
    }

    #[test]
    fn menu_covers_the_reader_and_admin_routes() {
        let menu = __inner_tap_menu();
        for path in [
            "/stories",
            "/articles",
            "/admin/argus/feeds",
            "/admin/argus/topics",
        ] {
            assert!(menu.iter().any(|m| m.path == path), "missing {path}");
        }
        // Every admin entry is permission-gated; the reader routes are not.
        for entry in menu.iter().filter(|m| m.path.starts_with("/admin/")) {
            assert_eq!(entry.permission, PERM_ADMINISTER, "{}", entry.path);
        }
    }

    #[test]
    fn queue_info_declares_a_queue_for_every_stage() {
        let info = __inner_tap_queue_info();
        let arr = info.as_array().unwrap();
        // Enumerated from the stage list rather than counted, so a new stage
        // that forgets its queue declaration fails here.
        let declared: Vec<&str> = arr
            .iter()
            .map(|q| q["name"].as_str().expect("queue name"))
            .collect();
        let expected: Vec<&str> = Stage::all().iter().map(|s| s.queue_name()).collect();
        assert_eq!(declared, expected);
    }

    #[test]
    fn worker_rejects_bad_payload_without_panicking() {
        let out = __inner_tap_queue_worker(serde_json::json!({ "not": "a job" }));
        assert_eq!(out["status"], "error");
    }

    #[test]
    fn job_payload_round_trips() {
        let job = JobPayload::new(Stage::Decide, "abc");
        let v = serde_json::to_value(&job).unwrap();
        assert_eq!(v["stage"], "decide");
        assert_eq!(v["id"], "abc");
        assert!(
            v.get("channel").is_none(),
            "an unscoped payload stays byte-compatible with what M1 enqueued"
        );
        let back: JobPayload = serde_json::from_value(v).unwrap();
        assert_eq!(back, job);
    }

    // ---- M4 ---------------------------------------------------------------

    #[test]
    fn a_payload_enqueued_before_m4_still_deserializes() {
        // Queue v2 rows outlive a plugin upgrade, so a job sitting in the queue
        // when this shipped must still be readable.
        let legacy = serde_json::json!({ "stage": "cluster", "id": "art-1" });
        let job: JobPayload = serde_json::from_value(legacy).unwrap();
        assert_eq!(job.stage, Stage::Cluster);
        assert!(job.channel.is_none());
    }

    #[test]
    fn a_channel_scoped_payload_round_trips() {
        let job = JobPayload::for_channel("evt-1", "chan-1");
        let v = serde_json::to_value(&job).unwrap();
        assert_eq!(v["stage"], "notify");
        assert_eq!(v["channel"], "chan-1");
        assert_eq!(serde_json::from_value::<JobPayload>(v).unwrap(), job);
    }

    #[test]
    fn the_channel_type_declares_every_field_the_config_reader_looks_for() {
        let types = __inner_tap_item_info();
        let channel = types
            .iter()
            .find(|t| t.machine_name == config::CHANNEL_TYPE)
            .expect("channel type declared");
        let fields: Vec<&str> = channel
            .fields
            .iter()
            .map(|f| f.field_name.as_str())
            .collect();
        for expected in [
            config::FIELD_CHANNEL_KIND,
            config::FIELD_CHANNEL_TARGET,
            config::FIELD_CHANNEL_SERVER,
            config::FIELD_CHANNEL_HEADERS,
            config::FIELD_CHANNEL_MIN_PRIORITY,
            config::FIELD_CHANNEL_EVENTS,
            config::FIELD_CHANNEL_NTFY_PRIORITY,
            config::FIELD_CONFIG_NOTE,
        ] {
            assert!(fields.contains(&expected), "missing {expected}: {fields:?}");
        }
    }

    #[test]
    fn the_topic_type_declares_its_notification_priority() {
        let types = __inner_tap_item_info();
        let topic = types
            .iter()
            .find(|t| t.machine_name == config::TOPIC_TYPE)
            .expect("topic type declared");
        assert!(
            topic
                .fields
                .iter()
                .any(|f| f.field_name == config::FIELD_TOPIC_PRIORITY)
        );
    }

    #[test]
    fn presave_coerces_a_channel_and_writes_back_every_field_it_owns() {
        // The presave return replaces the fields it names, so a coercion that
        // omitted one would blank it on every save.
        let out = __inner_tap_item_presave(serde_json::json!({
            "item_type": config::CHANNEL_TYPE,
            "fields": {
                config::FIELD_CHANNEL_KIND: "NTFY",
                config::FIELD_CHANNEL_TARGET: "argus-news",
                config::FIELD_CHANNEL_EVENTS: "story.new,story.exploded",
            }
        }));
        assert_eq!(out["fields"][config::FIELD_CHANNEL_KIND], "ntfy");
        assert_eq!(out["fields"][config::FIELD_CHANNEL_EVENTS], "story.new");
        assert!(
            out["fields"][config::FIELD_CONFIG_NOTE]
                .as_str()
                .unwrap()
                .contains("story.exploded")
        );
        for field in [
            config::FIELD_CHANNEL_SERVER,
            config::FIELD_CHANNEL_HEADERS,
            config::FIELD_CHANNEL_MIN_PRIORITY,
            config::FIELD_CHANNEL_NTFY_PRIORITY,
        ] {
            assert!(
                out["fields"].get(field).is_some(),
                "presave must write back {field}"
            );
        }
    }

    #[test]
    fn presave_normalizes_a_topics_notification_priority() {
        let out = __inner_tap_item_presave(serde_json::json!({
            "item_type": config::TOPIC_TYPE,
            "fields": {
                config::FIELD_RELEVANCE_PROMPT: "x",
                config::FIELD_TOPIC_PRIORITY: "HIGH",
            }
        }));
        assert_eq!(out["fields"][config::FIELD_TOPIC_PRIORITY], "high");
    }

    #[test]
    fn perm_covers_the_channel_type() {
        let perms = __inner_tap_perm();
        let names: Vec<&str> = perms.iter().map(|p| p.name.as_str()).collect();
        for expected in [
            "create argus_notify_channel content",
            "edit argus_notify_channel content",
            "delete argus_notify_channel content",
            "view argus_notify_channel content",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn menu_covers_the_notification_admin_routes() {
        let menu = __inner_tap_menu();
        for path in ["/admin/argus/channels", "/admin/argus/notifications"] {
            assert!(menu.iter().any(|m| m.path == path), "missing {path}");
        }
    }

    #[test]
    fn item_delete_only_touches_a_type_that_owns_state() {
        // A story or a topic has no plugin-owned state row keyed by its id, so
        // the tap must leave it alone rather than issue a DELETE against a
        // table that has nothing to do with it.
        for item_type in ["argus_story", config::TOPIC_TYPE, "blog"] {
            let out = __inner_tap_item_delete(serde_json::json!({
                "type": item_type,
                "id": "11111111-1111-4111-8111-111111111111",
            }));
            assert_eq!(out, serde_json::json!({}), "{item_type}");
        }
    }
}
