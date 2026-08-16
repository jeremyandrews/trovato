//! Feed and topic configuration: the field contract, and the coercion an admin
//! edit passes through (M3).
//!
//! From M3 on, a feed's and a topic's **configuration** is an Item that an admin
//! edits through the kernel's generic content forms, while a feed's mutable
//! **fetch state** stays in the plugin-owned `argus_feeds` table. `M3-DESIGN.md`
//! argues that split; this module is the pure half of it — the field names both
//! sides agree on, the parsing from an Item's `fields` object, and the
//! validation.
//!
//! # Why validation is coercion
//!
//! The frozen kernel dispatches `tap_item_presave` on the Item save path and
//! **merges whatever `fields` a plugin returns**, then saves unconditionally
//! (`crates/kernel/src/content/item_service.rs`, `ItemService::create`). There is
//! no return value that refuses a write, and `tap_form_validate` — the tap that
//! would refuse one — is on the form path that no route reaches. So
//! [`coerce_feed`] and [`coerce_topic`] clamp what can be clamped and, for the
//! one thing that cannot (a URL that is not a URL), disable the feed and say why
//! in a field the admin can see. Recorded as `G-NO-PRESAVE-VETO`.

use serde_json::Value;

use crate::notify::{ChannelConfig, ChannelKind, EventKind, NotifyPriority, NtfyPriority};

// ---------------------------------------------------------------------------
// Field contract
// ---------------------------------------------------------------------------

/// Machine name of the feed content type.
pub const FEED_TYPE: &str = "argus_feed";
/// Machine name of the topic content type.
pub const TOPIC_TYPE: &str = "argus_topic";

/// Feed field: the URL to fetch.
pub const FIELD_URL: &str = "field_url";
/// Feed field: the owning topic's Item id, as a uuid string.
pub const FIELD_TOPIC: &str = "field_topic";
/// Feed field: seconds between fetches.
pub const FIELD_FETCH_INTERVAL: &str = "field_fetch_interval";
/// Feed field: what the last presave coercion changed, for the admin to read.
pub const FIELD_CONFIG_NOTE: &str = "field_config_note";

/// Topic field: the prompt the decide stage scores an article against.
pub const FIELD_RELEVANCE_PROMPT: &str = "field_relevance_prompt";
/// Topic field: the score at or above which an article is kept (`0..=100`).
pub const FIELD_RELEVANCE_THRESHOLD: &str = "field_relevance_threshold";

/// Topic field (M4): `normal` or `high`. A high-priority topic notifies on every
/// story it produces, whatever the relevance score, and bypasses quiet hours.
pub const FIELD_TOPIC_PRIORITY: &str = "field_notify_priority";

/// Machine name of the notification channel content type (M4).
pub const CHANNEL_TYPE: &str = "argus_notify_channel";

/// Channel field: `ntfy`, `slack` or `webhook`.
pub const FIELD_CHANNEL_KIND: &str = "field_kind";
/// Channel field: the ntfy topic name, or the Slack/webhook URL.
pub const FIELD_CHANNEL_TARGET: &str = "field_target";
/// Channel field: the ntfy server base URL; blank means `https://ntfy.sh`.
pub const FIELD_CHANNEL_SERVER: &str = "field_server";
/// Channel field: a JSON object of extra request headers (webhook only).
pub const FIELD_CHANNEL_HEADERS: &str = "field_headers";
/// Channel field: the lowest priority this channel accepts.
pub const FIELD_CHANNEL_MIN_PRIORITY: &str = "field_min_priority";
/// Channel field: a comma-separated event filter; blank means every event.
pub const FIELD_CHANNEL_EVENTS: &str = "field_events";
/// Channel field: an explicit ntfy ladder position overriding the default map.
pub const FIELD_CHANNEL_NTFY_PRIORITY: &str = "field_ntfy_priority";

/// Floor on a feed's fetch interval. Five minutes is the politeness bound: a
/// public feed publisher is entitled to not be polled harder than this, and an
/// admin who types `1` gets clamped rather than obeyed.
pub const MIN_FETCH_INTERVAL_SECONDS: i64 = 300;

/// Ceiling on a feed's fetch interval (one week). Not a politeness bound — a
/// guard against a typo'd interval that silently parks a feed forever.
pub const MAX_FETCH_INTERVAL_SECONDS: i64 = 7 * 24 * 60 * 60;

/// Interval used when a feed declares none.
pub const DEFAULT_FETCH_INTERVAL_SECONDS: i64 = 900;

/// Relevance threshold used when a topic declares none.
pub const DEFAULT_RELEVANCE_THRESHOLD: u8 = 50;

// ---------------------------------------------------------------------------
// Parsed configuration
// ---------------------------------------------------------------------------

/// A feed's admin-owned configuration, read from its Item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedConfig {
    /// The feed Item's id (uuid string) — also the key of its state row.
    pub id: String,
    /// Display name (the Item's title).
    pub name: String,
    /// URL to fetch.
    pub url: String,
    /// Owning topic's Item id (uuid string); empty when unset.
    pub topic_id: String,
    /// Seconds between fetches, already clamped.
    pub interval_seconds: i64,
}

/// A topic's admin-owned configuration, read from its Item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicConfig {
    /// The topic Item's id (uuid string).
    pub id: String,
    /// Display name (the Item's title).
    pub name: String,
    /// The prompt an article is scored against.
    pub relevance_prompt: String,
    /// Keep threshold, already clamped to `0..=100`.
    pub threshold: u8,
    /// Notification priority for stories on this topic (M4).
    pub notify_priority: NotifyPriority,
}

/// Read a string field from an Item's `fields` object, trimmed.
///
/// Tolerates the field being absent, null, or a non-string: configuration that
/// an admin typed is not a place to be strict about JSON shape, and the caller
/// has a working default for every value.
fn field_str(fields: &Value, name: &str) -> String {
    fields
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Read an integer field from an Item's `fields` object.
///
/// Accepts a JSON number or a numeric string, because the kernel's generic item
/// form posts every text input as a string and an `Integer` field typed into
/// that form arrives as `"900"`, not `900`.
fn field_i64(fields: &Value, name: &str) -> Option<i64> {
    match fields.get(name) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Parse a feed Item (as the item host returns it) into its configuration.
///
/// Returns `None` when the value is not an Item object with an id — a deleted
/// Item, or a `get-item` miss, both of which the caller treats as "no feed".
pub fn parse_feed(item: &Value) -> Option<FeedConfig> {
    let id = item.get("id").and_then(Value::as_str)?.to_string();
    let fields = item.get("fields").cloned().unwrap_or(Value::Null);
    Some(FeedConfig {
        id,
        name: item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        url: field_str(&fields, FIELD_URL),
        topic_id: field_str(&fields, FIELD_TOPIC),
        interval_seconds: clamp_interval(
            field_i64(&fields, FIELD_FETCH_INTERVAL).unwrap_or(DEFAULT_FETCH_INTERVAL_SECONDS),
        ),
    })
}

/// Parse a topic Item into its configuration.
///
/// Returns `None` on the same terms as [`parse_feed`].
pub fn parse_topic(item: &Value) -> Option<TopicConfig> {
    let id = item.get("id").and_then(Value::as_str)?.to_string();
    let fields = item.get("fields").cloned().unwrap_or(Value::Null);
    Some(TopicConfig {
        id,
        name: item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        relevance_prompt: field_str(&fields, FIELD_RELEVANCE_PROMPT),
        threshold: clamp_threshold(
            field_i64(&fields, FIELD_RELEVANCE_THRESHOLD)
                .unwrap_or(i64::from(DEFAULT_RELEVANCE_THRESHOLD)),
        ),
        notify_priority: NotifyPriority::parse(&field_str(&fields, FIELD_TOPIC_PRIORITY)),
    })
}

/// Parse a notification channel Item into its configuration (M4).
///
/// Returns `None` on the same terms as [`parse_feed`], and also when the channel
/// is unusable: an unrecognized kind or an empty target. A channel that cannot
/// address anything is not a channel, and dropping it here is what keeps
/// [`crate::notify::dispatch`] from having to reason about half-configured rows.
pub fn parse_channel(item: &Value) -> Option<ChannelConfig> {
    let id = item.get("id").and_then(Value::as_str)?.to_string();
    let fields = item.get("fields").cloned().unwrap_or(Value::Null);

    let kind = ChannelKind::parse(&field_str(&fields, FIELD_CHANNEL_KIND))?;
    let target = field_str(&fields, FIELD_CHANNEL_TARGET);
    if target.is_empty() {
        return None;
    }

    Some(ChannelConfig {
        id,
        name: item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        kind,
        target,
        server: field_str(&fields, FIELD_CHANNEL_SERVER),
        headers: parse_headers(&field_str(&fields, FIELD_CHANNEL_HEADERS)),
        min_priority: NotifyPriority::parse(&field_str(&fields, FIELD_CHANNEL_MIN_PRIORITY)),
        events: parse_events(&field_str(&fields, FIELD_CHANNEL_EVENTS)).0,
        ntfy_priority: NtfyPriority::parse(&field_str(&fields, FIELD_CHANNEL_NTFY_PRIORITY)),
    })
}

/// Parse a JSON object of request headers, ignoring anything that is not a
/// string-to-string pair.
///
/// Sorted by name so a channel's rendered request is byte-stable across runs,
/// which is what lets the golden payload tests compare whole request bodies.
fn parse_headers(raw: &str) -> Vec<(String, String)> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = map
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Parse a comma-separated event filter into `(recognized, unrecognized)`.
fn parse_events(raw: &str) -> (Vec<EventKind>, Vec<String>) {
    let mut kinds = Vec::new();
    let mut unknown = Vec::new();
    for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match EventKind::from_column(token) {
            Some(kind) if !kinds.contains(&kind) => kinds.push(kind),
            Some(_) => {}
            None => unknown.push(token.to_string()),
        }
    }
    (kinds, unknown)
}

// ---------------------------------------------------------------------------
// Coercion
// ---------------------------------------------------------------------------

/// Clamp a fetch interval into `[MIN, MAX]`.
pub fn clamp_interval(seconds: i64) -> i64 {
    seconds.clamp(MIN_FETCH_INTERVAL_SECONDS, MAX_FETCH_INTERVAL_SECONDS)
}

/// Clamp a relevance threshold into `0..=100`.
pub fn clamp_threshold(value: i64) -> u8 {
    value.clamp(0, 100) as u8
}

/// Normalize a feed URL, or `None` when it is not usable as one.
///
/// Deliberately shallow: this rejects what is obviously not a fetchable URL
/// (empty, no scheme, a scheme that is not http/https, no host) and leaves
/// everything else to the fetch stage and the kernel's own SSRF fence, which is
/// the authority on whether a host may be reached. Duplicating that judgement
/// here would only produce a second, staler opinion.
pub fn normalize_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))?;
    // A host must be present and must not start the path/query straight away.
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty() || host.starts_with('.') || !host.contains(|c: char| c.is_alphanumeric()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// What a presave coercion decided about one feed edit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeedCoercion {
    /// The URL to persist (normalized), or empty when it was unusable.
    pub url: String,
    /// The interval to persist.
    pub interval_seconds: i64,
    /// `true` when the feed must be force-unpublished because its configuration
    /// cannot be fetched.
    pub disable: bool,
    /// Human-readable account of what changed, written to
    /// [`FIELD_CONFIG_NOTE`]. Empty when nothing was altered.
    pub note: String,
}

/// Coerce a feed's submitted fields into a fetchable configuration.
///
/// Every adjustment is reported in [`FeedCoercion::note`] rather than applied
/// silently: an admin whose interval was clamped is entitled to know it was,
/// especially given the kernel gives no way to refuse the save and tell them at
/// the time.
pub fn coerce_feed(fields: &Value) -> FeedCoercion {
    let mut notes: Vec<String> = Vec::new();

    let raw_url = field_str(fields, FIELD_URL);
    let (url, disable) = match normalize_url(&raw_url) {
        Some(u) => {
            if u != raw_url {
                notes.push("URL was trimmed.".to_string());
            }
            (u, false)
        }
        None => {
            notes.push(format!(
                "URL {raw_url:?} is not an absolute http(s) URL, so this feed was unpublished. \
                 Fix the URL and publish it again."
            ));
            (String::new(), true)
        }
    };

    let raw_interval = field_i64(fields, FIELD_FETCH_INTERVAL);
    let interval_seconds = clamp_interval(raw_interval.unwrap_or(DEFAULT_FETCH_INTERVAL_SECONDS));
    match raw_interval {
        None => notes.push(format!(
            "Fetch interval was unset, so it defaults to {DEFAULT_FETCH_INTERVAL_SECONDS}s."
        )),
        Some(v) if v != interval_seconds => notes.push(format!(
            "Fetch interval {v}s is outside \
             {MIN_FETCH_INTERVAL_SECONDS}s..={MAX_FETCH_INTERVAL_SECONDS}s, \
             so it was clamped to {interval_seconds}s."
        )),
        Some(_) => {}
    }

    if field_str(fields, FIELD_TOPIC).is_empty() {
        notes.push("No topic is set, so articles from this feed cannot be scored.".to_string());
    }

    FeedCoercion {
        url,
        interval_seconds,
        disable,
        note: notes.join(" "),
    }
}

/// What a presave coercion decided about one topic edit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopicCoercion {
    /// The threshold to persist.
    pub threshold: u8,
    /// The notification priority to persist, normalized to `normal` or `high`.
    pub notify_priority: String,
    /// Account of what changed; empty when nothing was altered.
    pub note: String,
}

/// Coerce a topic's submitted fields into a usable configuration.
pub fn coerce_topic(fields: &Value) -> TopicCoercion {
    let mut notes: Vec<String> = Vec::new();

    let raw = field_i64(fields, FIELD_RELEVANCE_THRESHOLD);
    let threshold = clamp_threshold(raw.unwrap_or(i64::from(DEFAULT_RELEVANCE_THRESHOLD)));
    match raw {
        None => notes.push(format!(
            "Relevance threshold was unset, so it defaults to {DEFAULT_RELEVANCE_THRESHOLD}."
        )),
        Some(v) if v != i64::from(threshold) => notes.push(format!(
            "Relevance threshold {v} is outside 0..=100, so it was clamped to {threshold}."
        )),
        Some(_) => {}
    }

    if field_str(fields, FIELD_RELEVANCE_PROMPT).is_empty() {
        notes.push(
            "No relevance prompt is set, so every article scores against an empty brief."
                .to_string(),
        );
    }

    let raw_priority = field_str(fields, FIELD_TOPIC_PRIORITY);
    let notify_priority = NotifyPriority::parse(&raw_priority);
    if !raw_priority.is_empty() && !raw_priority.eq_ignore_ascii_case(notify_priority.as_str()) {
        notes.push(format!(
            "Notification priority {raw_priority:?} is not \"normal\" or \"high\", \
             so it was read as {:?}.",
            notify_priority.as_str()
        ));
    }

    TopicCoercion {
        threshold,
        notify_priority: notify_priority.as_str().to_string(),
        note: notes.join(" "),
    }
}

/// What a presave coercion decided about one notification-channel edit (M4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelCoercion {
    /// The normalized kind, or empty when it named none.
    pub kind: String,
    /// The normalized target, or empty when it is unusable for the kind.
    pub target: String,
    /// The normalized ntfy server base, or empty for the default.
    pub server: String,
    /// The headers to persist, re-serialized, or `{}`.
    pub headers: String,
    /// The normalized priority floor.
    pub min_priority: String,
    /// The event filter, with unrecognized names dropped.
    pub events: String,
    /// The normalized ntfy override, or empty for the default ladder.
    pub ntfy_priority: String,
    /// Account of what changed; empty when nothing was altered.
    pub note: String,
}

/// Coerce a notification channel's submitted fields into a usable configuration.
///
/// Same shape and same reason as [`coerce_feed`]: presave can modify but not
/// refuse (G-NO-PRESAVE-VETO), so anything unusable is blanked with the reason
/// written where the administrator will read it, and
/// [`parse_channel`] then declines to load a channel with no kind or no target.
/// The enforcement lands one layer away from the mistake, which is the best the
/// frozen contract allows.
pub fn coerce_channel(fields: &Value) -> ChannelCoercion {
    let mut notes: Vec<String> = Vec::new();

    let raw_kind = field_str(fields, FIELD_CHANNEL_KIND);
    let kind = ChannelKind::parse(&raw_kind);
    if kind.is_none() {
        let legal: Vec<&str> = ChannelKind::all().iter().map(|k| k.as_str()).collect();
        notes.push(format!(
            "Kind {raw_kind:?} is not one of {}, so this channel will not be used.",
            legal.join(", ")
        ));
    }

    let raw_target = field_str(fields, FIELD_CHANNEL_TARGET);
    let target = match kind {
        // An ntfy target is a topic name, not a URL: no scheme, no slashes.
        Some(ChannelKind::Ntfy) => {
            if raw_target.contains('/') || raw_target.contains(':') {
                notes.push(format!(
                    "ntfy target {raw_target:?} looks like a URL. Put the server in the \
                     server field and only the topic name here; this channel will not be used."
                ));
                String::new()
            } else {
                raw_target.clone()
            }
        }
        Some(_) => match normalize_url(&raw_target) {
            Some(url) => url,
            None => {
                notes.push(format!(
                    "Target {raw_target:?} is not an absolute http(s) URL, \
                     so this channel will not be used."
                ));
                String::new()
            }
        },
        None => String::new(),
    };

    let raw_server = field_str(fields, FIELD_CHANNEL_SERVER);
    let server = if raw_server.is_empty() {
        String::new()
    } else {
        match normalize_url(&raw_server) {
            Some(url) => url,
            None => {
                notes.push(format!(
                    "Server {raw_server:?} is not an absolute http(s) URL, \
                     so the default ntfy server is used instead."
                ));
                String::new()
            }
        }
    };

    let raw_headers = field_str(fields, FIELD_CHANNEL_HEADERS);
    let headers = if raw_headers.is_empty() {
        String::new()
    } else {
        let parsed = parse_headers(&raw_headers);
        if parsed.is_empty() {
            notes.push(
                "Headers must be a JSON object of string values, so they were cleared.".to_string(),
            );
            String::new()
        } else {
            let map: serde_json::Map<String, Value> = parsed
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect();
            Value::Object(map).to_string()
        }
    };

    let raw_min = field_str(fields, FIELD_CHANNEL_MIN_PRIORITY);
    let min_priority = NotifyPriority::parse(&raw_min);
    if !raw_min.is_empty() && !raw_min.eq_ignore_ascii_case(min_priority.as_str()) {
        notes.push(format!(
            "Minimum priority {raw_min:?} is not \"normal\" or \"high\", so it was read as {:?}.",
            min_priority.as_str()
        ));
    }

    let (kinds, unknown) = parse_events(&field_str(fields, FIELD_CHANNEL_EVENTS));
    if !unknown.is_empty() {
        let legal: Vec<&str> = EventKind::all().iter().map(|k| k.as_str()).collect();
        notes.push(format!(
            "Dropped unrecognized event name(s) {}. Legal names are {}.",
            unknown.join(", "),
            legal.join(", ")
        ));
    }
    let events = kinds
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(",");

    let raw_ntfy = field_str(fields, FIELD_CHANNEL_NTFY_PRIORITY);
    let ntfy_priority = match NtfyPriority::parse(&raw_ntfy) {
        Some(p) => p.as_str().to_string(),
        None => {
            if !raw_ntfy.is_empty() {
                notes.push(format!(
                    "ntfy priority {raw_ntfy:?} is not one of min, low, default, high, urgent, \
                     so the default mapping is used."
                ));
            }
            String::new()
        }
    };

    ChannelCoercion {
        kind: kind.map(|k| k.as_str().to_string()).unwrap_or_default(),
        target,
        server,
        headers,
        min_priority: min_priority.as_str().to_string(),
        events,
        ntfy_priority,
        note: notes.join(" "),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_feed_item() {
        let item = json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "title": "Ars Technica",
            "fields": {
                FIELD_URL: "https://arstechnica.com/feed/",
                FIELD_TOPIC: "22222222-2222-4222-8222-222222222222",
                FIELD_FETCH_INTERVAL: 1800,
            }
        });
        let feed = parse_feed(&item).expect("feed parses");
        assert_eq!(feed.name, "Ars Technica");
        assert_eq!(feed.url, "https://arstechnica.com/feed/");
        assert_eq!(feed.interval_seconds, 1800);
    }

    #[test]
    fn parses_an_integer_field_posted_as_a_string() {
        // The kernel's generic item form posts every text input as a string, so
        // an Integer field arrives as "1800" rather than 1800.
        let item = json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "title": "Feed",
            "fields": { FIELD_URL: "https://example.com/f", FIELD_FETCH_INTERVAL: "1800" }
        });
        assert_eq!(parse_feed(&item).expect("parses").interval_seconds, 1800);
    }

    #[test]
    fn feed_without_an_id_is_not_a_feed() {
        assert!(parse_feed(&json!({ "title": "orphan" })).is_none());
        assert!(parse_feed(&Value::Null).is_none());
    }

    #[test]
    fn missing_interval_falls_back_to_the_default() {
        let item = json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "fields": { FIELD_URL: "https://example.com/f" }
        });
        assert_eq!(
            parse_feed(&item).expect("parses").interval_seconds,
            DEFAULT_FETCH_INTERVAL_SECONDS
        );
    }

    #[test]
    fn parses_a_topic_item_and_clamps_its_threshold() {
        let item = json!({
            "id": "22222222-2222-4222-8222-222222222222",
            "title": "Infrastructure",
            "fields": {
                FIELD_RELEVANCE_PROMPT: "Datacentre and network operations",
                FIELD_RELEVANCE_THRESHOLD: 140,
            }
        });
        let topic = parse_topic(&item).expect("topic parses");
        assert_eq!(topic.name, "Infrastructure");
        assert_eq!(topic.threshold, 100);
    }

    #[test]
    fn normalizes_only_plausible_http_urls() {
        assert_eq!(
            normalize_url("  https://example.com/feed  ").as_deref(),
            Some("https://example.com/feed")
        );
        assert_eq!(
            normalize_url("http://example.com").as_deref(),
            Some("http://example.com")
        );
        assert!(normalize_url("").is_none());
        assert!(normalize_url("example.com/feed").is_none());
        assert!(normalize_url("ftp://example.com/feed").is_none());
        assert!(normalize_url("file:///etc/passwd").is_none());
        assert!(normalize_url("https://").is_none());
        assert!(normalize_url("https:///feed").is_none());
    }

    #[test]
    fn clamps_the_interval_at_both_ends() {
        assert_eq!(clamp_interval(1), MIN_FETCH_INTERVAL_SECONDS);
        assert_eq!(clamp_interval(i64::MAX), MAX_FETCH_INTERVAL_SECONDS);
        assert_eq!(clamp_interval(-5), MIN_FETCH_INTERVAL_SECONDS);
        assert_eq!(clamp_interval(1800), 1800);
    }

    #[test]
    fn coercion_clamps_an_impolite_interval_and_says_so() {
        let out = coerce_feed(&json!({
            FIELD_URL: "https://example.com/feed",
            FIELD_TOPIC: "22222222-2222-4222-8222-222222222222",
            FIELD_FETCH_INTERVAL: 5,
        }));
        assert_eq!(out.interval_seconds, MIN_FETCH_INTERVAL_SECONDS);
        assert!(!out.disable);
        assert!(out.note.contains("clamped"), "note was {:?}", out.note);
    }

    #[test]
    fn coercion_disables_a_feed_whose_url_is_unusable() {
        // The kernel gives presave no way to refuse the save, so the only honest
        // outcome is a visibly disabled feed with the reason attached.
        let out = coerce_feed(&json!({
            FIELD_URL: "not a url",
            FIELD_TOPIC: "22222222-2222-4222-8222-222222222222",
            FIELD_FETCH_INTERVAL: 900,
        }));
        assert!(out.disable);
        assert!(out.url.is_empty());
        assert!(out.note.contains("unpublished"), "note was {:?}", out.note);
    }

    #[test]
    fn a_clean_feed_edit_is_left_alone() {
        let out = coerce_feed(&json!({
            FIELD_URL: "https://example.com/feed",
            FIELD_TOPIC: "22222222-2222-4222-8222-222222222222",
            FIELD_FETCH_INTERVAL: 1800,
        }));
        assert_eq!(out.url, "https://example.com/feed");
        assert_eq!(out.interval_seconds, 1800);
        assert!(!out.disable);
        assert!(out.note.is_empty(), "note was {:?}", out.note);
    }

    #[test]
    fn coercion_flags_a_feed_with_no_topic() {
        let out = coerce_feed(&json!({
            FIELD_URL: "https://example.com/feed",
            FIELD_FETCH_INTERVAL: 900,
        }));
        assert!(!out.disable, "a topicless feed is fetchable, just unscored");
        assert!(out.note.contains("No topic"), "note was {:?}", out.note);
    }

    #[test]
    fn topic_coercion_clamps_and_reports() {
        let out = coerce_topic(&json!({
            FIELD_RELEVANCE_PROMPT: "Anything about Rust",
            FIELD_RELEVANCE_THRESHOLD: -3,
        }));
        assert_eq!(out.threshold, 0);
        assert!(out.note.contains("clamped"), "note was {:?}", out.note);
    }

    #[test]
    fn a_clean_topic_edit_is_left_alone() {
        let out = coerce_topic(&json!({
            FIELD_RELEVANCE_PROMPT: "Anything about Rust",
            FIELD_RELEVANCE_THRESHOLD: 60,
            FIELD_TOPIC_PRIORITY: "normal",
        }));
        assert_eq!(out.threshold, 60);
        assert_eq!(out.notify_priority, "normal");
        assert!(out.note.is_empty(), "note was {:?}", out.note);
    }

    // ---- M4: topic notification priority ---------------------------------

    #[test]
    fn a_topic_defaults_to_normal_notification_priority() {
        let topic = parse_topic(&json!({
            "id": "22222222-2222-4222-8222-222222222222",
            "fields": { FIELD_RELEVANCE_PROMPT: "x" }
        }))
        .expect("parses");
        assert_eq!(topic.notify_priority, NotifyPriority::Normal);
    }

    #[test]
    fn a_high_priority_topic_is_read_as_such() {
        let topic = parse_topic(&json!({
            "id": "22222222-2222-4222-8222-222222222222",
            "fields": { FIELD_RELEVANCE_PROMPT: "x", FIELD_TOPIC_PRIORITY: " High " }
        }))
        .expect("parses");
        assert_eq!(topic.notify_priority, NotifyPriority::High);
    }

    #[test]
    fn an_unreadable_topic_priority_is_normalized_and_reported() {
        let out = coerce_topic(&json!({
            FIELD_RELEVANCE_PROMPT: "x",
            FIELD_RELEVANCE_THRESHOLD: 60,
            FIELD_TOPIC_PRIORITY: "VERY LOUD",
        }));
        assert_eq!(out.notify_priority, "normal");
        assert!(out.note.contains("VERY LOUD"), "note was {:?}", out.note);
    }

    // ---- M4: channels ----------------------------------------------------

    fn channel_item(fields: Value) -> Value {
        json!({
            "id": "33333333-3333-4333-8333-333333333333",
            "title": "Ops webhook",
            "fields": fields,
        })
    }

    #[test]
    fn parses_a_webhook_channel_with_headers_and_a_filter() {
        let channel = parse_channel(&channel_item(json!({
            FIELD_CHANNEL_KIND: "webhook",
            FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            FIELD_CHANNEL_HEADERS: r#"{"Authorization": "Bearer x", "X-Env": "prod"}"#,
            FIELD_CHANNEL_MIN_PRIORITY: "high",
            FIELD_CHANNEL_EVENTS: "story.new, alert.queue_stuck",
        })))
        .expect("channel parses");

        assert_eq!(channel.name, "Ops webhook");
        assert_eq!(channel.kind, ChannelKind::Webhook);
        assert_eq!(channel.target, "https://ops.example/hook");
        assert_eq!(channel.min_priority, NotifyPriority::High);
        assert_eq!(
            channel.events,
            vec![EventKind::StoryNew, EventKind::QueueStuck]
        );
        // Sorted by name, so a rendered request is byte-stable across runs.
        assert_eq!(
            channel.headers,
            vec![
                ("Authorization".to_string(), "Bearer x".to_string()),
                ("X-Env".to_string(), "prod".to_string()),
            ]
        );
    }

    #[test]
    fn parses_an_ntfy_channel_with_a_server_and_a_priority_override() {
        let channel = parse_channel(&channel_item(json!({
            FIELD_CHANNEL_KIND: "ntfy",
            FIELD_CHANNEL_TARGET: "argus-news",
            FIELD_CHANNEL_SERVER: "https://push.example",
            FIELD_CHANNEL_NTFY_PRIORITY: "urgent",
        })))
        .expect("channel parses");
        assert_eq!(channel.target, "argus-news");
        assert_eq!(channel.server, "https://push.example");
        assert_eq!(channel.ntfy_priority, Some(NtfyPriority::Urgent));
        assert!(channel.events.is_empty(), "no filter means every event");
    }

    #[test]
    fn a_channel_with_no_kind_or_no_target_is_not_loaded() {
        assert!(
            parse_channel(&channel_item(json!({
                FIELD_CHANNEL_KIND: "carrier pigeon",
                FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            })))
            .is_none()
        );
        assert!(
            parse_channel(&channel_item(json!({
                FIELD_CHANNEL_KIND: "webhook",
                FIELD_CHANNEL_TARGET: "  ",
            })))
            .is_none()
        );
        assert!(parse_channel(&json!({ "title": "orphan" })).is_none());
    }

    #[test]
    fn malformed_headers_are_ignored_rather_than_half_applied() {
        let channel = parse_channel(&channel_item(json!({
            FIELD_CHANNEL_KIND: "webhook",
            FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            FIELD_CHANNEL_HEADERS: "not json at all",
        })))
        .expect("channel parses");
        assert!(channel.headers.is_empty());
    }

    #[test]
    fn a_clean_channel_edit_is_left_alone() {
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "webhook",
            FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            FIELD_CHANNEL_MIN_PRIORITY: "normal",
        }));
        assert_eq!(out.kind, "webhook");
        assert_eq!(out.target, "https://ops.example/hook");
        assert_eq!(out.min_priority, "normal");
        assert!(out.note.is_empty(), "note was {:?}", out.note);
    }

    #[test]
    fn an_unknown_channel_kind_is_blanked_and_explained() {
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "telegram",
            FIELD_CHANNEL_TARGET: "https://api.telegram.test/x",
        }));
        assert!(out.kind.is_empty());
        assert!(
            out.target.is_empty(),
            "an unusable kind has no usable target"
        );
        assert!(out.note.contains("telegram"), "note was {:?}", out.note);
        assert!(out.note.contains("will not be used"));
    }

    #[test]
    fn a_webhook_target_that_is_not_a_url_is_blanked_and_explained() {
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "slack",
            FIELD_CHANNEL_TARGET: "hooks.slack.com/services/x",
        }));
        assert!(out.target.is_empty());
        assert!(out.note.contains("absolute http(s) URL"));
    }

    #[test]
    fn an_ntfy_topic_that_is_really_a_url_is_refused_with_the_fix_named() {
        // The most likely administrator mistake by a distance: pasting the whole
        // ntfy.sh URL into the topic field.
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "ntfy",
            FIELD_CHANNEL_TARGET: "https://ntfy.sh/argus",
        }));
        assert!(out.target.is_empty());
        assert!(out.note.contains("server field"), "note was {:?}", out.note);
    }

    #[test]
    fn channel_headers_are_normalized_and_a_bad_object_is_reported() {
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "webhook",
            FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            FIELD_CHANNEL_HEADERS: r#"{"b": "2", "a": "1"}"#,
        }));
        assert_eq!(out.headers, r#"{"a":"1","b":"2"}"#);
        assert!(out.note.is_empty());

        let bad = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "webhook",
            FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            FIELD_CHANNEL_HEADERS: "[1, 2, 3]",
        }));
        assert!(bad.headers.is_empty());
        assert!(bad.note.contains("JSON object"));
    }

    #[test]
    fn an_unknown_event_name_is_dropped_and_the_legal_ones_are_listed() {
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "webhook",
            FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            FIELD_CHANNEL_EVENTS: "story.new, story.exploded, alert.budget_threshold",
        }));
        assert_eq!(out.events, "story.new,alert.budget_threshold");
        assert!(out.note.contains("story.exploded"));
        assert!(out.note.contains("story.digest"), "note was {:?}", out.note);
    }

    #[test]
    fn a_duplicated_event_name_is_listed_once() {
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "webhook",
            FIELD_CHANNEL_TARGET: "https://ops.example/hook",
            FIELD_CHANNEL_EVENTS: "story.new,story.new",
        }));
        assert_eq!(out.events, "story.new");
    }

    #[test]
    fn a_bad_ntfy_priority_override_falls_back_to_the_default_ladder() {
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "ntfy",
            FIELD_CHANNEL_TARGET: "argus",
            FIELD_CHANNEL_NTFY_PRIORITY: "deafening",
        }));
        assert!(out.ntfy_priority.is_empty());
        assert!(out.note.contains("deafening"));
    }

    #[test]
    fn a_coerced_channel_round_trips_back_through_the_parser() {
        // The property that matters: what presave writes is what the loader can
        // read. A coercion that produced fields the parser rejects would leave a
        // channel that looks configured and never fires.
        let out = coerce_channel(&json!({
            FIELD_CHANNEL_KIND: "NTFY",
            FIELD_CHANNEL_TARGET: "argus-news",
            FIELD_CHANNEL_SERVER: "  https://push.example/  ",
            FIELD_CHANNEL_MIN_PRIORITY: "HIGH",
            FIELD_CHANNEL_EVENTS: "story.new",
            FIELD_CHANNEL_NTFY_PRIORITY: "5",
        }));
        let channel = parse_channel(&channel_item(json!({
            FIELD_CHANNEL_KIND: out.kind,
            FIELD_CHANNEL_TARGET: out.target,
            FIELD_CHANNEL_SERVER: out.server,
            FIELD_CHANNEL_HEADERS: out.headers,
            FIELD_CHANNEL_MIN_PRIORITY: out.min_priority,
            FIELD_CHANNEL_EVENTS: out.events,
            FIELD_CHANNEL_NTFY_PRIORITY: out.ntfy_priority,
        })))
        .expect("a coerced channel is a loadable channel");

        assert_eq!(channel.kind, ChannelKind::Ntfy);
        assert_eq!(channel.target, "argus-news");
        assert_eq!(channel.server, "https://push.example/");
        assert_eq!(channel.min_priority, NotifyPriority::High);
        assert_eq!(channel.events, vec![EventKind::StoryNew]);
        assert_eq!(channel.ntfy_priority, Some(NtfyPriority::Urgent));
    }
}
