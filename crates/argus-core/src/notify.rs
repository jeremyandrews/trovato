//! Notification channels, payload rendering and dispatch (M4).
//!
//! Everything here is host-agnostic. The plugin supplies one [`Transport`] over
//! the kernel `http` host; the core decides *what* to send, *where*, and *how it
//! is shaped*, and records what each channel did.
//!
//! # The three channels
//!
//! - **ntfy** — publish to a topic on `ntfy.sh` or a self-hosted server, with
//!   the five-level priority ladder ([`NtfyPriority`]).
//! - **Slack** — an incoming webhook, posted as `text` plus one block section so
//!   it renders as a card rather than a wall of prose.
//! - **generic webhook** — a configurable URL and headers carrying the
//!   standardized [`webhook_payload`] envelope.
//!
//! A fourth, APNS, is deliberately absent until there is an iOS app to send to.
//! [`ChannelKind`] is where it lands, and [`Notification`] already carries
//! everything a push payload needs (title, body, deep link, priority).
//!
//! # Isolation is the whole point of [`dispatch`]
//!
//! One channel failing must never stop another. `dispatch` therefore returns a
//! [`ChannelOutcome`] **per channel** and never short-circuits: a refused URL, a
//! 500 from Slack and a delivered ntfy publish all coexist in one result.
//!
//! # Blocked versus failed
//!
//! The distinction is the retry decision, and it matters more here than
//! elsewhere because an operator's webhook URL is exactly the kind of thing that
//! is wrong permanently. A URL the kernel's SSRF fence refuses, or a 4xx that is
//! not a rate limit, is [`DeliveryState::Blocked`] — recorded, surfaced, never
//! retried. A timeout, a 429 or a 5xx is [`DeliveryState::Failed`] and earns a
//! delayed re-enqueue.

use serde_json::{Value, json};

use crate::error::{CoreError, CoreResult};

// ---------------------------------------------------------------------------
// Priorities
// ---------------------------------------------------------------------------

/// How loud a notification is.
///
/// [`NotifyPriority::High`] is a bypass, not a queue jump: it skips debounce,
/// quiet hours and digest collapse (`M4-DESIGN.md` Decision 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum NotifyPriority {
    /// The ordinary case.
    #[default]
    Normal,
    /// A high-priority topic, or an operator alert that has already fired.
    High,
}

impl NotifyPriority {
    /// The lowercase value persisted in the `priority` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyPriority::Normal => "normal",
            NotifyPriority::High => "high",
        }
    }

    /// Parse a persisted or admin-entered priority, defaulting to
    /// [`NotifyPriority::Normal`] for anything unrecognized.
    ///
    /// Lenient on purpose: this reads an admin's free-text field as well as a
    /// column, and a typo must not silence a channel.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "high" | "urgent" | "critical" => NotifyPriority::High,
            _ => NotifyPriority::Normal,
        }
    }
}

/// ntfy's five-level priority ladder.
///
/// The names are ntfy's own; the numbers are what the `Priority` header and the
/// JSON `priority` key take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NtfyPriority {
    /// 1 — no sound, no vibration, not shown in the notification drawer.
    Min,
    /// 2 — no sound or vibration.
    Low,
    /// 3 — the ordinary notification.
    Default,
    /// 4 — long vibration, bypasses some quiet settings on the device.
    High,
    /// 5 — as loud as ntfy gets.
    Urgent,
}

impl NtfyPriority {
    /// The wire value ntfy expects.
    #[must_use]
    pub fn as_i64(self) -> i64 {
        match self {
            NtfyPriority::Min => 1,
            NtfyPriority::Low => 2,
            NtfyPriority::Default => 3,
            NtfyPriority::High => 4,
            NtfyPriority::Urgent => 5,
        }
    }

    /// The name an admin types into a channel's override field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NtfyPriority::Min => "min",
            NtfyPriority::Low => "low",
            NtfyPriority::Default => "default",
            NtfyPriority::High => "high",
            NtfyPriority::Urgent => "urgent",
        }
    }

    /// Parse an admin-entered override, or `None` when it names no level.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_ascii_lowercase().as_str() {
            "min" | "1" => NtfyPriority::Min,
            "low" | "2" => NtfyPriority::Low,
            "default" | "3" => NtfyPriority::Default,
            "high" | "4" => NtfyPriority::High,
            "urgent" | "max" | "5" => NtfyPriority::Urgent,
            _ => return None,
        })
    }

    /// The default ladder position for an event.
    ///
    /// Operator alerts sit a rung above story traffic at the same priority,
    /// because a pipeline that has stopped is worth more of an interruption
    /// than a story that has started. A digest is quieter than its parts, which
    /// is the entire reason to collapse one.
    #[must_use]
    pub fn for_event(kind: EventKind, priority: NotifyPriority) -> Self {
        match (kind.is_operator_alert(), kind, priority) {
            (true, _, NotifyPriority::High) => NtfyPriority::Urgent,
            (true, _, NotifyPriority::Normal) => NtfyPriority::High,
            (false, EventKind::StoryDigest, _) => NtfyPriority::Low,
            (false, _, NotifyPriority::High) => NtfyPriority::High,
            (false, _, NotifyPriority::Normal) => NtfyPriority::Default,
        }
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// What happened, as the outbox records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A story was summarized for the first time.
    StoryNew,
    /// A story's synthesis materially changed.
    StoryUpdated,
    /// Several story events collapsed into one message.
    StoryDigest,
    /// A feed has failed N consecutive fetches.
    FeedFailing,
    /// The day's AI spend crossed the alert threshold or the daily limit.
    BudgetThreshold,
    /// The plugin's oldest claimable queue job is older than the configured bound.
    QueueStuck,
}

impl EventKind {
    /// The value persisted in the `event_type` column and sent as the payload's
    /// `event` key. Dotted rather than snake_case because it is a public wire
    /// contract that a webhook consumer routes on.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::StoryNew => "story.new",
            EventKind::StoryUpdated => "story.updated",
            EventKind::StoryDigest => "story.digest",
            EventKind::FeedFailing => "alert.feed_failing",
            EventKind::BudgetThreshold => "alert.budget_threshold",
            EventKind::QueueStuck => "alert.queue_stuck",
        }
    }

    /// Parse a persisted `event_type`.
    #[must_use]
    pub fn from_column(raw: &str) -> Option<Self> {
        Some(match raw {
            "story.new" => EventKind::StoryNew,
            "story.updated" => EventKind::StoryUpdated,
            "story.digest" => EventKind::StoryDigest,
            "alert.feed_failing" => EventKind::FeedFailing,
            "alert.budget_threshold" => EventKind::BudgetThreshold,
            "alert.queue_stuck" => EventKind::QueueStuck,
            _ => return None,
        })
    }

    /// Every kind, so a filter or a round-trip test cannot silently miss one.
    #[must_use]
    pub fn all() -> [Self; 6] {
        [
            EventKind::StoryNew,
            EventKind::StoryUpdated,
            EventKind::StoryDigest,
            EventKind::FeedFailing,
            EventKind::BudgetThreshold,
            EventKind::QueueStuck,
        ]
    }

    /// Whether this is an operator alert rather than reader-facing news.
    ///
    /// Operator alerts bypass quiet hours by default and sit a rung higher on
    /// the ntfy ladder.
    #[must_use]
    pub fn is_operator_alert(self) -> bool {
        matches!(
            self,
            EventKind::FeedFailing | EventKind::BudgetThreshold | EventKind::QueueStuck
        )
    }

    /// Whether this kind participates in digest collapse.
    #[must_use]
    pub fn is_digestible(self) -> bool {
        matches!(self, EventKind::StoryNew | EventKind::StoryUpdated)
    }
}

/// A rendered notification, before any channel has shaped it.
///
/// One of these is built from an outbox row and handed to every channel; the
/// channel renderers read it and produce their own [`OutboundRequest`].
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// What happened.
    pub kind: EventKind,
    /// How loud.
    pub priority: NotifyPriority,
    /// One-line headline.
    pub title: String,
    /// The prose body.
    pub body: String,
    /// A deep link to the story or admin page, when one exists.
    pub link: Option<String>,
    /// When the event was recorded (unix seconds).
    pub timestamp: i64,
    /// The story or feed this is about, when it is about one.
    pub subject_id: Option<String>,
    /// Event-specific structured data, carried verbatim into the webhook
    /// envelope so a consumer can act on it without parsing prose.
    pub data: Value,
}

impl Notification {
    /// The body, truncated to `max` characters on a character boundary, with an
    /// ellipsis when it was cut.
    ///
    /// Channels have wildly different length tolerances (an ntfy message is a
    /// phone notification; a webhook body is not), so truncation is the
    /// renderer's decision rather than the event's.
    #[must_use]
    pub fn short_body(&self, max: usize) -> String {
        truncate_chars(&self.body, max)
    }
}

/// Truncate to `max` characters, appending a single-character ellipsis when the
/// input was longer. Character-safe on multibyte input.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", kept.trim_end())
}

// ---------------------------------------------------------------------------
// The outbox
// ---------------------------------------------------------------------------

/// Where an outbox row is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventState {
    /// Recorded, not yet dispatched.
    Pending,
    /// Dispatched (whatever the individual channels made of it).
    Sent,
    /// Deliberately not sent — debounced, or judged immaterial.
    Suppressed,
    /// Folded into another event's digest.
    Digested,
}

impl EventState {
    /// The value persisted in the `state` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EventState::Pending => "pending",
            EventState::Sent => "sent",
            EventState::Suppressed => "suppressed",
            EventState::Digested => "digested",
        }
    }

    /// Parse a persisted state.
    #[must_use]
    pub fn from_column(raw: &str) -> Option<Self> {
        Some(match raw {
            "pending" => EventState::Pending,
            "sent" => EventState::Sent,
            "suppressed" => EventState::Suppressed,
            "digested" => EventState::Digested,
            _ => return None,
        })
    }
}

/// The `data` key carrying the summary a story event is about.
pub const DATA_SUMMARY: &str = "summary";
/// The `data` key carrying the summary a story *had* before this update, which
/// is what the change judge compares against.
pub const DATA_PREVIOUS_SUMMARY: &str = "previous_summary";

/// A notification the pipeline has decided to make, before it is recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    /// What happened.
    pub kind: EventKind,
    /// How loud.
    pub priority: NotifyPriority,
    /// The story or feed this is about.
    pub subject_id: Option<String>,
    /// The idempotency key, unique with `kind`.
    ///
    /// This is what makes an at-least-once redelivery of the job that decided to
    /// notify record *one* event rather than one per delivery. Its content is
    /// per-kind: a story id for a new story, a story id plus a hash of the new
    /// summary for an update, a feed id plus its failure count for an alert.
    pub dedup_key: String,
    /// Headline.
    pub title: String,
    /// Body prose.
    pub body: String,
    /// Deep link.
    pub link: Option<String>,
    /// Structured payload data.
    pub data: Value,
}

/// An outbox row as the notify worker reads it back.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    /// The event row id, which is also the notify job's payload id.
    pub id: String,
    /// What happened.
    pub kind: EventKind,
    /// How loud.
    pub priority: NotifyPriority,
    /// The story or feed this is about.
    pub subject_id: Option<String>,
    /// Where the row is in its life.
    pub state: EventState,
    /// The earliest instant this may be sent (pushed out by quiet hours).
    pub scheduled_at: i64,
    /// When the event was recorded.
    pub created: i64,
    /// Headline.
    pub title: String,
    /// Body prose.
    pub body: String,
    /// Deep link.
    pub link: Option<String>,
    /// Structured payload data.
    pub data: Value,
}

impl StoredEvent {
    /// The notification this row renders to.
    #[must_use]
    pub fn notification(&self) -> Notification {
        Notification {
            kind: self.kind,
            priority: self.priority,
            title: self.title.clone(),
            body: self.body.clone(),
            link: self.link.clone(),
            timestamp: self.created,
            subject_id: self.subject_id.clone(),
            data: self.data.clone(),
        }
    }

    /// A string from the row's `data`, or `""`.
    #[must_use]
    pub fn data_str(&self, key: &str) -> &str {
        self.data
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Most stories named in a digest body. Past this the digest is a list nobody
/// reads, so the remainder is counted rather than named.
pub const MAX_DIGEST_TITLES: usize = 8;

/// Build the notification for a digest: `head` is the due event that becomes the
/// digest, `folded` are the others collapsed into it (excluding `head`).
///
/// The digest takes the *lowest* priority of its parts, which is always
/// `Normal` in practice because high-priority events bypass collapse entirely
/// (`M4-DESIGN.md` Decision 7).
#[must_use]
pub fn digest_notification(head: &StoredEvent, folded: &[StoredEvent], now: i64) -> Notification {
    let total = folded.len() + 1;
    let mut titles: Vec<&str> = std::iter::once(head.title.as_str())
        .chain(folded.iter().map(|e| e.title.as_str()))
        .collect();
    let overflow = titles.len().saturating_sub(MAX_DIGEST_TITLES);
    titles.truncate(MAX_DIGEST_TITLES);

    let mut body = titles
        .iter()
        .map(|t| format!("\u{2022} {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    if overflow > 0 {
        body.push_str(&format!("\n\u{2022} and {overflow} more"));
    }

    let subjects: Vec<&str> = std::iter::once(head)
        .chain(folded.iter())
        .filter_map(|e| e.subject_id.as_deref())
        .collect();

    Notification {
        kind: EventKind::StoryDigest,
        priority: NotifyPriority::Normal,
        title: format!("{total} new stories"),
        body,
        // A digest is about several stories, so it links to the list rather than
        // to any one of them. The plugin supplies the site-relative path.
        link: None,
        timestamp: now,
        subject_id: None,
        data: json!({
            "count": total,
            "story_ids": subjects,
            "titles": titles,
        }),
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// The transport a channel speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// ntfy.sh (or a self-hosted ntfy server).
    Ntfy,
    /// A Slack incoming webhook.
    Slack,
    /// A generic JSON webhook.
    Webhook,
}

impl ChannelKind {
    /// The value persisted in the channel Item's kind field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Ntfy => "ntfy",
            ChannelKind::Slack => "slack",
            ChannelKind::Webhook => "webhook",
        }
    }

    /// Parse an admin-entered kind.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_ascii_lowercase().as_str() {
            "ntfy" => ChannelKind::Ntfy,
            "slack" => ChannelKind::Slack,
            "webhook" | "http" | "generic" => ChannelKind::Webhook,
            _ => return None,
        })
    }

    /// Every kind, for the admin-facing list of legal values.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [ChannelKind::Ntfy, ChannelKind::Slack, ChannelKind::Webhook]
    }
}

/// The default ntfy server when a channel names none.
pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";

/// One configured notification channel, as read from its Item.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelConfig {
    /// The channel Item's id.
    pub id: String,
    /// The Item's title, used in logs and per-channel error reporting.
    pub name: String,
    /// Which transport.
    pub kind: ChannelKind,
    /// ntfy topic name, or the Slack/webhook URL.
    pub target: String,
    /// ntfy server base URL. Ignored by the other kinds.
    pub server: String,
    /// Extra request headers (webhook only; an ntfy or Slack channel ignores
    /// them, because both have a fixed content type and no auth story a header
    /// map improves).
    pub headers: Vec<(String, String)>,
    /// The lowest priority this channel accepts.
    pub min_priority: NotifyPriority,
    /// Event kinds this channel accepts; empty means every kind.
    pub events: Vec<EventKind>,
    /// An explicit ntfy ladder position, overriding [`NtfyPriority::for_event`].
    pub ntfy_priority: Option<NtfyPriority>,
}

impl ChannelConfig {
    /// Whether this channel wants `notification`.
    ///
    /// Two independent filters, both of which an operator sets per channel: a
    /// priority floor (an on-call phone takes only the loud ones) and an event
    /// allowlist (a `#news` Slack channel does not want budget alerts).
    #[must_use]
    pub fn accepts(&self, notification: &Notification) -> bool {
        if notification.priority < self.min_priority {
            return false;
        }
        // A digest inherits acceptance from the story events it collapsed: a
        // channel that takes `story.new` must not be surprised into missing the
        // digest that stands in for five of them.
        if self.events.is_empty() {
            return true;
        }
        if self.events.contains(&notification.kind) {
            return true;
        }
        notification.kind == EventKind::StoryDigest
            && (self.events.contains(&EventKind::StoryNew)
                || self.events.contains(&EventKind::StoryUpdated))
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// One outbound HTTP POST, fully rendered.
///
/// This is what gets persisted on the delivery row before the send is attempted,
/// which is what makes the end-to-end test's golden assertion an assertion about
/// the real pipeline rather than about a fixture (`M4-DESIGN.md` Decision 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequest {
    /// Absolute URL to POST to.
    pub url: String,
    /// Request headers, including the content type.
    pub headers: Vec<(String, String)>,
    /// Request body.
    pub body: String,
}

/// What a transport got back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body, retained only for the error message on a failure.
    pub body: String,
}

/// The outbound side of a notification channel.
///
/// Implemented in the plugin over the kernel `http` host and in the core's tests
/// by an in-memory recorder. Nothing here names a host function, which is what
/// lets the whole dispatch path be tested without a kernel.
pub trait Transport {
    /// POST `req` and return the response.
    ///
    /// # Errors
    ///
    /// [`CoreError::FetchRefused`] for a permanently unusable target (the SSRF
    /// fence, a malformed URL, a body over the host's transfer ceiling);
    /// [`CoreError::Fetch`] for anything worth retrying.
    fn post(&self, req: &OutboundRequest) -> CoreResult<OutboundResponse>;
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Longest ntfy message body. A phone notification that runs past this is not
/// read, it is dismissed.
pub const NTFY_BODY_CHARS: usize = 900;

/// Longest Slack text block.
pub const SLACK_BODY_CHARS: usize = 2800;

/// The wire version of the generic webhook envelope. Bumped only on a breaking
/// change to the payload shape, so a consumer can branch on it.
pub const WEBHOOK_PAYLOAD_VERSION: u32 = 1;

/// The standardized generic-webhook envelope.
///
/// Stable by contract: `event`, `timestamp`, `priority`, `title`, `body` and
/// `data` are what a consumer routes on, and `version` is how they know the
/// shape has not moved under them.
#[must_use]
pub fn webhook_payload(notification: &Notification) -> Value {
    json!({
        "source": "argus",
        "version": WEBHOOK_PAYLOAD_VERSION,
        "event": notification.kind.as_str(),
        "timestamp": notification.timestamp,
        "priority": notification.priority.as_str(),
        "subject_id": notification.subject_id,
        "title": notification.title,
        "body": notification.body,
        "link": notification.link,
        "data": notification.data,
    })
}

/// Join a server base and an ntfy topic into a publish URL.
fn ntfy_url(server: &str, topic: &str) -> String {
    let base = if server.trim().is_empty() {
        DEFAULT_NTFY_SERVER
    } else {
        server.trim()
    };
    format!("{}/{}", base.trim_end_matches('/'), topic.trim())
}

/// Render one notification for one channel.
///
/// # Errors
///
/// [`CoreError::Invalid`] when the channel's configuration cannot address
/// anything — an empty ntfy topic or an empty webhook URL. Permanent: a retry
/// re-reads the same configuration and gets the same answer, so the delivery is
/// recorded as blocked rather than retried.
pub fn render(channel: &ChannelConfig, notification: &Notification) -> CoreResult<OutboundRequest> {
    if channel.target.trim().is_empty() {
        return Err(CoreError::Invalid(format!(
            "channel {:?} ({}) has no target configured",
            channel.name,
            channel.kind.as_str()
        )));
    }
    Ok(match channel.kind {
        ChannelKind::Ntfy => render_ntfy(channel, notification),
        ChannelKind::Slack => render_slack(channel, notification),
        ChannelKind::Webhook => render_webhook(channel, notification),
    })
}

/// Render an ntfy publish.
fn render_ntfy(channel: &ChannelConfig, notification: &Notification) -> OutboundRequest {
    let priority = channel
        .ntfy_priority
        .unwrap_or_else(|| NtfyPriority::for_event(notification.kind, notification.priority));
    let mut body = json!({
        "title": notification.title,
        "message": notification.short_body(NTFY_BODY_CHARS),
        "priority": priority.as_i64(),
        "tags": ntfy_tags(notification.kind),
    });
    if let Some(link) = &notification.link {
        // `click` is what ntfy opens when the notification is tapped, which is
        // the entire point of a story notification on a phone.
        body["click"] = json!(link);
    }
    OutboundRequest {
        url: ntfy_url(&channel.server, &channel.target),
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        body: body.to_string(),
    }
}

/// The ntfy tag (emoji shortcode) for an event kind. Purely cosmetic, and the
/// one place a notification is allowed to be decorative.
fn ntfy_tags(kind: EventKind) -> Vec<&'static str> {
    match kind {
        EventKind::StoryNew => vec!["newspaper"],
        EventKind::StoryUpdated => vec!["pencil2"],
        EventKind::StoryDigest => vec!["books"],
        EventKind::FeedFailing => vec!["warning"],
        EventKind::BudgetThreshold => vec!["moneybag"],
        EventKind::QueueStuck => vec!["rotating_light"],
    }
}

/// Render a Slack incoming-webhook post.
///
/// `text` carries the whole message because it is what Slack shows in the
/// notification preview and in clients that do not render blocks; the block
/// section is the readable version.
fn render_slack(channel: &ChannelConfig, notification: &Notification) -> OutboundRequest {
    let short = notification.short_body(SLACK_BODY_CHARS);
    let mut text = format!("*{}*\n{short}", notification.title);
    if let Some(link) = &notification.link {
        text.push_str(&format!("\n<{link}|Read the story>"));
    }
    let body = json!({
        "text": text,
        "blocks": [{
            "type": "section",
            "text": { "type": "mrkdwn", "text": text },
        }],
    });
    OutboundRequest {
        url: channel.target.trim().to_string(),
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        body: body.to_string(),
    }
}

/// Render a generic webhook post, applying the channel's configured headers.
fn render_webhook(channel: &ChannelConfig, notification: &Notification) -> OutboundRequest {
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    for (k, v) in &channel.headers {
        // An admin-supplied Content-Type replaces the default rather than
        // arriving beside it; a duplicated header is worse than a wrong one.
        if k.eq_ignore_ascii_case("content-type") {
            headers[0] = (k.clone(), v.clone());
        } else {
            headers.push((k.clone(), v.clone()));
        }
    }
    OutboundRequest {
        url: channel.target.trim().to_string(),
        headers,
        body: webhook_payload(notification).to_string(),
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// What happened to one channel's delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    /// The channel accepted it (2xx).
    Delivered,
    /// A transient failure worth retrying (timeout, 429, 5xx).
    Failed,
    /// A permanent refusal: the SSRF fence, a malformed target, an unusable
    /// channel configuration, or a 4xx that is not a rate limit. Never retried.
    Blocked,
    /// The channel did not want this notification (priority floor or event
    /// filter). Recorded so an operator can see *why* nobody was told.
    Skipped,
}

impl DeliveryState {
    /// The value persisted in the `state` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryState::Delivered => "delivered",
            DeliveryState::Failed => "failed",
            DeliveryState::Blocked => "blocked",
            DeliveryState::Skipped => "skipped",
        }
    }

    /// Whether a delivery in this state earns a delayed re-enqueue.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        self == DeliveryState::Failed
    }
}

/// One channel's result.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelOutcome {
    /// The channel Item's id.
    pub channel_id: String,
    /// The channel's name, for the operator-facing error.
    pub channel_name: String,
    /// What happened.
    pub state: DeliveryState,
    /// The status code, when a response was received.
    pub http_status: Option<u16>,
    /// The failure, in an operator's terms.
    pub error: Option<String>,
    /// Exactly what was sent (or would have been). `None` only for a skip,
    /// where nothing was rendered.
    pub request: Option<OutboundRequest>,
}

/// Classify a response status.
///
/// 429 and 5xx are the retryable band. Everything else in 4xx is the operator's
/// configuration being wrong, which retrying will not fix.
fn classify_status(status: u16) -> (DeliveryState, Option<String>) {
    match status {
        200..=299 => (DeliveryState::Delivered, None),
        429 => (
            DeliveryState::Failed,
            Some("rate limited (HTTP 429)".to_string()),
        ),
        500..=599 => (DeliveryState::Failed, Some(format!("HTTP {status}"))),
        _ => (DeliveryState::Blocked, Some(format!("HTTP {status}"))),
    }
}

/// Send one notification to one channel.
///
/// Never returns `Err`: every failure mode is a [`ChannelOutcome`], because the
/// caller's contract is that one channel cannot take down another.
#[must_use]
pub fn deliver<T: Transport + ?Sized>(
    transport: &T,
    channel: &ChannelConfig,
    notification: &Notification,
) -> ChannelOutcome {
    let base = ChannelOutcome {
        channel_id: channel.id.clone(),
        channel_name: channel.name.clone(),
        state: DeliveryState::Skipped,
        http_status: None,
        error: None,
        request: None,
    };

    if !channel.accepts(notification) {
        return ChannelOutcome {
            error: Some(format!(
                "channel does not accept {} at priority {}",
                notification.kind.as_str(),
                notification.priority.as_str()
            )),
            ..base
        };
    }

    let request = match render(channel, notification) {
        Ok(r) => r,
        Err(e) => {
            return ChannelOutcome {
                state: DeliveryState::Blocked,
                error: Some(e.to_string()),
                ..base
            };
        }
    };

    match transport.post(&request) {
        Ok(resp) => {
            let (state, error) = classify_status(resp.status);
            let error = error.map(|e| {
                let detail = truncate_chars(resp.body.trim(), 200);
                if detail.is_empty() {
                    e
                } else {
                    format!("{e}: {detail}")
                }
            });
            ChannelOutcome {
                state,
                http_status: Some(resp.status),
                error,
                request: Some(request),
                ..base
            }
        }
        // A refused target is the operator's URL being unusable — the SSRF
        // fence, a bad scheme, a body over the ceiling. Permanent by
        // construction, so it is surfaced rather than retried.
        Err(e @ CoreError::FetchRefused(_)) | Err(e @ CoreError::Invalid(_)) => ChannelOutcome {
            state: DeliveryState::Blocked,
            error: Some(e.to_string()),
            request: Some(request),
            ..base
        },
        Err(e) => ChannelOutcome {
            state: DeliveryState::Failed,
            error: Some(e.to_string()),
            request: Some(request),
            ..base
        },
    }
}

/// Send one notification to every channel, isolating each from the others.
///
/// Returns one outcome per channel in the order given, always the same length as
/// `channels`, so a caller can record a row per channel without matching up.
#[must_use]
pub fn dispatch<T: Transport + ?Sized>(
    transport: &T,
    channels: &[ChannelConfig],
    notification: &Notification,
) -> Vec<ChannelOutcome> {
    channels
        .iter()
        .map(|c| deliver(transport, c, notification))
        .collect()
}

/// Whether every channel that wanted this notification took it.
///
/// A skip is not a failure — the channel was asked and declined.
#[must_use]
pub fn all_delivered(outcomes: &[ChannelOutcome]) -> bool {
    outcomes
        .iter()
        .all(|o| matches!(o.state, DeliveryState::Delivered | DeliveryState::Skipped))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A [`Transport`] that records what it was handed and replays a script.
    struct Recorder {
        script: RefCell<Vec<CoreResult<OutboundResponse>>>,
        sent: RefCell<Vec<OutboundRequest>>,
    }

    impl Recorder {
        fn new(script: Vec<CoreResult<OutboundResponse>>) -> Self {
            Self {
                script: RefCell::new(script),
                sent: RefCell::new(Vec::new()),
            }
        }

        fn ok() -> Self {
            Self::new(vec![])
        }

        fn sent(&self) -> Vec<OutboundRequest> {
            self.sent.borrow().clone()
        }
    }

    impl Transport for Recorder {
        fn post(&self, req: &OutboundRequest) -> CoreResult<OutboundResponse> {
            self.sent.borrow_mut().push(req.clone());
            let mut script = self.script.borrow_mut();
            if script.is_empty() {
                return Ok(OutboundResponse {
                    status: 200,
                    body: String::new(),
                });
            }
            script.remove(0)
        }
    }

    fn note(kind: EventKind, priority: NotifyPriority) -> Notification {
        Notification {
            kind,
            priority,
            title: "Chip maker posts record quarter".into(),
            body: "Reuters reported record revenue. Bloomberg disagreed on the margin.".into(),
            link: Some("https://news.example/stories/abc".into()),
            timestamp: 1_767_225_600,
            subject_id: Some("abc".into()),
            data: json!({ "article_count": 3 }),
        }
    }

    fn channel(kind: ChannelKind, target: &str) -> ChannelConfig {
        ChannelConfig {
            id: format!("chan-{}", kind.as_str()),
            name: format!("{} channel", kind.as_str()),
            kind,
            target: target.into(),
            server: String::new(),
            headers: Vec::new(),
            min_priority: NotifyPriority::Normal,
            events: Vec::new(),
            ntfy_priority: None,
        }
    }

    // ---- enum contracts --------------------------------------------------

    #[test]
    fn every_event_kind_round_trips_through_its_column_value() {
        for kind in EventKind::all() {
            assert_eq!(
                EventKind::from_column(kind.as_str()),
                Some(kind),
                "{kind:?} does not round-trip"
            );
        }
        assert_eq!(EventKind::from_column("story.exploded"), None);
    }

    #[test]
    fn event_column_values_are_unique() {
        let mut seen: Vec<&str> = EventKind::all().iter().map(|k| k.as_str()).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "two kinds share a column value");
    }

    #[test]
    fn exactly_three_kinds_are_operator_alerts() {
        let alerts: Vec<&str> = EventKind::all()
            .iter()
            .filter(|k| k.is_operator_alert())
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            alerts,
            vec![
                "alert.feed_failing",
                "alert.budget_threshold",
                "alert.queue_stuck"
            ]
        );
    }

    #[test]
    fn every_ntfy_level_round_trips_and_has_its_documented_number() {
        for (level, n) in [
            (NtfyPriority::Min, 1),
            (NtfyPriority::Low, 2),
            (NtfyPriority::Default, 3),
            (NtfyPriority::High, 4),
            (NtfyPriority::Urgent, 5),
        ] {
            assert_eq!(level.as_i64(), n);
            assert_eq!(NtfyPriority::parse(level.as_str()), Some(level));
            assert_eq!(NtfyPriority::parse(&n.to_string()), Some(level));
        }
        assert_eq!(NtfyPriority::parse("loud"), None);
    }

    #[test]
    fn the_ntfy_ladder_orders_alerts_above_stories_and_digests_below_them() {
        assert_eq!(
            NtfyPriority::for_event(EventKind::StoryNew, NotifyPriority::Normal),
            NtfyPriority::Default
        );
        assert_eq!(
            NtfyPriority::for_event(EventKind::StoryNew, NotifyPriority::High),
            NtfyPriority::High
        );
        assert_eq!(
            NtfyPriority::for_event(EventKind::StoryDigest, NotifyPriority::Normal),
            NtfyPriority::Low
        );
        assert_eq!(
            NtfyPriority::for_event(EventKind::QueueStuck, NotifyPriority::Normal),
            NtfyPriority::High
        );
        assert_eq!(
            NtfyPriority::for_event(EventKind::QueueStuck, NotifyPriority::High),
            NtfyPriority::Urgent
        );
    }

    #[test]
    fn priority_parsing_is_lenient_and_defaults_to_normal() {
        assert_eq!(NotifyPriority::parse(" HIGH "), NotifyPriority::High);
        assert_eq!(NotifyPriority::parse("urgent"), NotifyPriority::High);
        assert_eq!(NotifyPriority::parse("nonsense"), NotifyPriority::Normal);
        assert_eq!(NotifyPriority::parse(""), NotifyPriority::Normal);
    }

    #[test]
    fn channel_kind_round_trips_and_accepts_aliases() {
        for kind in ChannelKind::all() {
            assert_eq!(ChannelKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ChannelKind::parse("HTTP"), Some(ChannelKind::Webhook));
        assert_eq!(ChannelKind::parse("carrier pigeon"), None);
    }

    // ---- acceptance ------------------------------------------------------

    #[test]
    fn a_priority_floor_drops_normal_traffic() {
        let mut c = channel(ChannelKind::Ntfy, "argus");
        c.min_priority = NotifyPriority::High;
        assert!(!c.accepts(&note(EventKind::StoryNew, NotifyPriority::Normal)));
        assert!(c.accepts(&note(EventKind::StoryNew, NotifyPriority::High)));
    }

    #[test]
    fn an_event_filter_drops_everything_it_does_not_name() {
        let mut c = channel(ChannelKind::Slack, "https://hooks.slack.test/x");
        c.events = vec![EventKind::FeedFailing];
        assert!(!c.accepts(&note(EventKind::StoryNew, NotifyPriority::Normal)));
        assert!(c.accepts(&note(EventKind::FeedFailing, NotifyPriority::Normal)));
    }

    #[test]
    fn a_digest_is_accepted_by_a_channel_that_wanted_its_parts() {
        // The digest exists to stand in for five story.new events; a channel
        // subscribed to story.new must not lose all five to the collapse.
        let mut c = channel(ChannelKind::Ntfy, "argus");
        c.events = vec![EventKind::StoryNew];
        assert!(c.accepts(&note(EventKind::StoryDigest, NotifyPriority::Normal)));

        c.events = vec![EventKind::BudgetThreshold];
        assert!(!c.accepts(&note(EventKind::StoryDigest, NotifyPriority::Normal)));
    }

    #[test]
    fn an_empty_event_filter_accepts_every_kind() {
        let c = channel(ChannelKind::Webhook, "https://ops.example/hook");
        for kind in EventKind::all() {
            assert!(c.accepts(&note(kind, NotifyPriority::Normal)), "{kind:?}");
        }
    }

    // ---- rendering: golden payloads --------------------------------------

    #[test]
    fn ntfy_renders_topic_url_title_message_priority_and_click() {
        let c = channel(ChannelKind::Ntfy, "argus-news");
        let req = render(&c, &note(EventKind::StoryNew, NotifyPriority::Normal)).unwrap();
        assert_eq!(req.url, "https://ntfy.sh/argus-news");
        let body: Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["title"], "Chip maker posts record quarter");
        assert!(body["message"].as_str().unwrap().contains("Bloomberg"));
        assert_eq!(body["priority"], 3);
        assert_eq!(body["tags"], json!(["newspaper"]));
        assert_eq!(body["click"], "https://news.example/stories/abc");
    }

    #[test]
    fn ntfy_uses_a_self_hosted_server_and_strips_its_trailing_slash() {
        let mut c = channel(ChannelKind::Ntfy, "argus");
        c.server = "https://push.internal.example/".into();
        let req = render(&c, &note(EventKind::StoryNew, NotifyPriority::Normal)).unwrap();
        assert_eq!(req.url, "https://push.internal.example/argus");
    }

    #[test]
    fn an_explicit_ntfy_priority_overrides_the_ladder() {
        let mut c = channel(ChannelKind::Ntfy, "argus");
        c.ntfy_priority = Some(NtfyPriority::Min);
        let req = render(&c, &note(EventKind::QueueStuck, NotifyPriority::High)).unwrap();
        let body: Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["priority"], 1, "the override wins over urgent");
    }

    #[test]
    fn ntfy_truncates_a_long_body_on_a_character_boundary() {
        let mut n = note(EventKind::StoryNew, NotifyPriority::Normal);
        n.body = "é".repeat(4000);
        let req = render(&channel(ChannelKind::Ntfy, "argus"), &n).unwrap();
        let body: Value = serde_json::from_str(&req.body).unwrap();
        let message = body["message"].as_str().unwrap();
        assert_eq!(message.chars().count(), NTFY_BODY_CHARS);
        assert!(message.ends_with('\u{2026}'));
    }

    #[test]
    fn slack_renders_text_blocks_and_a_link() {
        let c = channel(ChannelKind::Slack, "https://hooks.slack.test/T/B/X");
        let req = render(&c, &note(EventKind::StoryNew, NotifyPriority::Normal)).unwrap();
        assert_eq!(req.url, "https://hooks.slack.test/T/B/X");
        let body: Value = serde_json::from_str(&req.body).unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.starts_with("*Chip maker posts record quarter*"));
        assert!(text.contains("<https://news.example/stories/abc|Read the story>"));
        assert_eq!(body["blocks"][0]["text"]["text"], body["text"]);
    }

    #[test]
    fn the_webhook_envelope_is_the_documented_shape() {
        let c = channel(ChannelKind::Webhook, "https://ops.example/hook");
        let req = render(&c, &note(EventKind::StoryUpdated, NotifyPriority::High)).unwrap();
        let body: Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["source"], "argus");
        assert_eq!(body["version"], WEBHOOK_PAYLOAD_VERSION);
        assert_eq!(body["event"], "story.updated");
        assert_eq!(body["priority"], "high");
        assert_eq!(body["timestamp"], 1_767_225_600);
        assert_eq!(body["subject_id"], "abc");
        assert_eq!(body["title"], "Chip maker posts record quarter");
        assert_eq!(body["link"], "https://news.example/stories/abc");
        assert_eq!(body["data"]["article_count"], 3);
        assert_eq!(
            req.headers,
            vec![("Content-Type".to_string(), "application/json".to_string())]
        );
    }

    #[test]
    fn webhook_headers_are_applied_and_a_supplied_content_type_replaces_the_default() {
        let mut c = channel(ChannelKind::Webhook, "https://ops.example/hook");
        c.headers = vec![
            ("Authorization".into(), "Bearer sekrit".into()),
            ("content-type".into(), "application/vnd.argus+json".into()),
        ];
        let req = render(&c, &note(EventKind::StoryNew, NotifyPriority::Normal)).unwrap();
        assert_eq!(
            req.headers,
            vec![
                (
                    "content-type".to_string(),
                    "application/vnd.argus+json".to_string()
                ),
                ("Authorization".to_string(), "Bearer sekrit".to_string()),
            ]
        );
    }

    #[test]
    fn a_notification_with_no_link_omits_the_click_and_the_slack_footer() {
        let mut n = note(EventKind::BudgetThreshold, NotifyPriority::Normal);
        n.link = None;
        let ntfy: Value = serde_json::from_str(
            &render(&channel(ChannelKind::Ntfy, "argus"), &n)
                .unwrap()
                .body,
        )
        .unwrap();
        assert!(ntfy.get("click").is_none());

        let slack: Value = serde_json::from_str(
            &render(
                &channel(ChannelKind::Slack, "https://hooks.slack.test/x"),
                &n,
            )
            .unwrap()
            .body,
        )
        .unwrap();
        assert!(!slack["text"].as_str().unwrap().contains("Read the story"));
    }

    #[test]
    fn an_empty_target_cannot_be_rendered() {
        let err = render(
            &channel(ChannelKind::Ntfy, "   "),
            &note(EventKind::StoryNew, NotifyPriority::Normal),
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)));
        assert!(!err.is_transient());
    }

    // ---- dispatch --------------------------------------------------------

    #[test]
    fn a_delivered_channel_records_its_status_and_what_it_sent() {
        let t = Recorder::ok();
        let out = deliver(
            &t,
            &channel(ChannelKind::Ntfy, "argus"),
            &note(EventKind::StoryNew, NotifyPriority::Normal),
        );
        assert_eq!(out.state, DeliveryState::Delivered);
        assert_eq!(out.http_status, Some(200));
        assert!(out.error.is_none());
        assert_eq!(out.request.unwrap().url, t.sent()[0].url);
    }

    #[test]
    fn one_failing_channel_never_blocks_another() {
        // The middle channel 500s, the third is refused outright; the first and
        // fourth must still be delivered, and every outcome must be present.
        let t = Recorder::new(vec![
            Ok(OutboundResponse {
                status: 200,
                body: String::new(),
            }),
            Ok(OutboundResponse {
                status: 503,
                body: "upstream down".into(),
            }),
            Err(CoreError::FetchRefused("blocked or invalid URL".into())),
            Ok(OutboundResponse {
                status: 204,
                body: String::new(),
            }),
        ]);
        let channels = vec![
            channel(ChannelKind::Ntfy, "argus"),
            channel(ChannelKind::Slack, "https://hooks.slack.test/x"),
            channel(ChannelKind::Webhook, "http://127.0.0.1:9/hook"),
            channel(ChannelKind::Webhook, "https://ops.example/hook"),
        ];
        let outcomes = dispatch(
            &t,
            &channels,
            &note(EventKind::StoryNew, NotifyPriority::Normal),
        );

        assert_eq!(outcomes.len(), 4);
        assert_eq!(outcomes[0].state, DeliveryState::Delivered);
        assert_eq!(outcomes[1].state, DeliveryState::Failed);
        assert!(
            outcomes[1]
                .error
                .as_ref()
                .unwrap()
                .contains("upstream down")
        );
        assert_eq!(outcomes[2].state, DeliveryState::Blocked);
        assert_eq!(outcomes[3].state, DeliveryState::Delivered);
        assert!(!all_delivered(&outcomes));
        assert_eq!(t.sent().len(), 4, "every channel was attempted");
    }

    #[test]
    fn an_ssrf_blocked_target_is_a_clean_per_channel_error_carrying_its_payload() {
        // The scope's explicit requirement: a blocked webhook URL surfaces as a
        // per-channel error, not a worker failure — and the payload that would
        // have been sent is still recorded.
        let t = Recorder::new(vec![Err(CoreError::FetchRefused(
            "blocked or invalid URL (host -32)".into(),
        ))]);
        let out = deliver(
            &t,
            &channel(ChannelKind::Webhook, "http://localhost:8080/hook"),
            &note(EventKind::StoryNew, NotifyPriority::Normal),
        );
        assert_eq!(out.state, DeliveryState::Blocked);
        assert!(!out.state.is_retryable());
        assert!(out.error.as_ref().unwrap().contains("blocked"));
        let sent: Value = serde_json::from_str(&out.request.unwrap().body).unwrap();
        assert_eq!(sent["event"], "story.new");
    }

    #[test]
    fn a_rate_limit_and_a_5xx_are_retryable_but_a_404_is_not() {
        for (status, expected) in [
            (429, DeliveryState::Failed),
            (500, DeliveryState::Failed),
            (503, DeliveryState::Failed),
            (400, DeliveryState::Blocked),
            (403, DeliveryState::Blocked),
            (404, DeliveryState::Blocked),
            (200, DeliveryState::Delivered),
            (204, DeliveryState::Delivered),
        ] {
            let t = Recorder::new(vec![Ok(OutboundResponse {
                status,
                body: String::new(),
            })]);
            let out = deliver(
                &t,
                &channel(ChannelKind::Webhook, "https://ops.example/hook"),
                &note(EventKind::StoryNew, NotifyPriority::Normal),
            );
            assert_eq!(out.state, expected, "status {status}");
        }
    }

    #[test]
    fn a_transient_transport_error_is_failed_not_blocked() {
        let t = Recorder::new(vec![Err(CoreError::Fetch("connection reset".into()))]);
        let out = deliver(
            &t,
            &channel(ChannelKind::Slack, "https://hooks.slack.test/x"),
            &note(EventKind::StoryNew, NotifyPriority::Normal),
        );
        assert_eq!(out.state, DeliveryState::Failed);
        assert!(out.state.is_retryable());
    }

    #[test]
    fn a_skipped_channel_is_never_posted_to_and_says_why() {
        let mut c = channel(ChannelKind::Ntfy, "argus");
        c.min_priority = NotifyPriority::High;
        let t = Recorder::ok();
        let out = deliver(&t, &c, &note(EventKind::StoryNew, NotifyPriority::Normal));
        assert_eq!(out.state, DeliveryState::Skipped);
        assert!(out.request.is_none());
        assert!(out.error.as_ref().unwrap().contains("does not accept"));
        assert!(t.sent().is_empty());
        // A skip is not a failure: nobody needed telling.
        assert!(all_delivered(&[out]));
    }

    #[test]
    fn dispatching_to_no_channels_is_vacuously_delivered() {
        let t = Recorder::ok();
        let outcomes = dispatch(&t, &[], &note(EventKind::StoryNew, NotifyPriority::Normal));
        assert!(outcomes.is_empty());
        assert!(all_delivered(&outcomes));
    }

    #[test]
    fn an_unconfigured_channel_blocks_without_a_network_call() {
        let t = Recorder::ok();
        let out = deliver(
            &t,
            &channel(ChannelKind::Webhook, ""),
            &note(EventKind::StoryNew, NotifyPriority::Normal),
        );
        assert_eq!(out.state, DeliveryState::Blocked);
        assert!(t.sent().is_empty());
    }
}
