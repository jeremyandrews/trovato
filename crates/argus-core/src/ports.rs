//! Injected ports: the boundary between pure pipeline logic and the host.
//!
//! Every side effect the pipeline needs — talking to a model, fetching a feed,
//! reading/writing article state, enqueuing the next stage — is expressed as a
//! trait here. The `plugins/argus` cdylib implements each trait over kernel host
//! functions; the core's own tests implement them with in-memory fakes. This is
//! the hedge from ARCHITECTURE.md §9.6: if the pure-plugin shape ever hits a
//! hard wall, the same core wraps in a native daemon harness with no rewrite,
//! because nothing in this crate names a host function or a kernel type.

use crate::analyze::Analysis;
use crate::budget::DailySpend;
use crate::cluster::{CandidateArticle, CandidateStory};
use crate::entity::{EntityPlan, EntityRecord, EntityType};
use crate::error::CoreResult;
use crate::model::{PipelineState, Stage};
use crate::summarize::{StoryMember, StorySummary};

// ---------------------------------------------------------------------------
// LLM provider (M1-3)
// ---------------------------------------------------------------------------

/// A chat completion request in core terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    /// Optional model override; `None` uses the provider's configured model
    /// for the operation. The plugin maps this onto `AiRequest.model` so the
    /// decide and analyze stages can route to cheap vs strong models.
    pub model: Option<String>,
    /// Optional system prompt.
    pub system: Option<String>,
    /// The user turn.
    pub user: String,
    /// Optional generation cap.
    pub max_tokens: Option<u32>,
}

/// Token accounting returned by the provider on every call.
///
/// The core records usage per call (the "usage record per call" requirement of
/// M1-3). Dollar cost rides on [`ChatResponse::cost_estimate`], not here, since
/// the host prices a chat call but not a raw embed usage record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Prompt/input tokens.
    pub prompt_tokens: u32,
    /// Generated tokens.
    pub completion_tokens: u32,
    /// Total tokens.
    pub total_tokens: u32,
}

/// A chat completion response.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    /// Generated text.
    pub content: String,
    /// Model that actually served the request.
    pub model: String,
    /// Token usage for this call.
    pub usage: Usage,
    /// Estimated dollar cost of this call, as reported by the host
    /// (`AiResponse.cost_estimate`, G-COST-OPAQUE fixed by p11j). `None` when the
    /// model is unpriced or no pricing is configured — the plugin reads cost from
    /// the response now, not from a kernel-side SQL query.
    pub cost_estimate: Option<f64>,
}

/// An embedding response.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedResponse {
    /// The embedding vector.
    pub vector: Vec<f32>,
    /// Model that produced the vector.
    pub model: String,
    /// Token usage for this call.
    pub usage: Usage,
}

/// A model provider: chat and embed, with per-call usage.
pub trait LlmProvider {
    /// Run a single chat completion.
    fn chat(&self, req: &ChatRequest) -> CoreResult<ChatResponse>;
    /// Produce an embedding for `input`.
    fn embed(&self, input: &str, model: Option<&str>) -> CoreResult<EmbedResponse>;
}

// ---------------------------------------------------------------------------
// Fetcher (M1-5)
// ---------------------------------------------------------------------------

/// Conditional-GET validators persisted on a feed from a previous fetch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionalHeaders {
    /// Prior `ETag` response header, replayed as `If-None-Match`.
    pub etag: Option<String>,
    /// Prior `Last-Modified` response header, replayed as `If-Modified-Since`.
    pub last_modified: Option<String>,
}

/// The result of a conditional feed fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// The server returned 304: nothing changed since the last fetch.
    NotModified,
    /// A fresh body, with any updated validators to persist.
    Fetched {
        /// The response body (feed XML).
        body: String,
        /// New `ETag`, if the server sent one.
        etag: Option<String>,
        /// New `Last-Modified`, if the server sent one.
        last_modified: Option<String>,
    },
}

/// An outbound fetcher. Implementations reassemble the streaming http host into
/// a whole body; a blocked URL surfaces as [`crate::error::CoreError::FetchRefused`].
pub trait Fetcher {
    /// Fetch `url`, replaying `cond` as conditional-GET headers.
    fn fetch(&self, url: &str, cond: &ConditionalHeaders) -> CoreResult<FetchOutcome>;
}

// ---------------------------------------------------------------------------
// Store (M1-2 / M1-6 / M1-7 / M1-9)
// ---------------------------------------------------------------------------

/// A feed's operational row, as the fetch stage needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    /// Feed row id (uuid string).
    pub id: String,
    /// Feed URL to fetch.
    pub url: String,
    /// Topic this feed feeds into (uuid string).
    pub topic_id: String,
    /// Prior conditional-GET validators.
    pub conditional: ConditionalHeaders,
}

/// A feed's scheduling row, as the cron stage needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedSchedule {
    /// Feed row id (uuid string).
    pub id: String,
    /// Fetch interval in seconds.
    pub interval_seconds: i64,
    /// Last successful/attempted fetch time (unix seconds), or `None` if never.
    pub last_fetched_at: Option<i64>,
}

/// A new article to upsert, in core terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArticle {
    /// Canonical URL (unique dedup key).
    pub url: String,
    /// Headline.
    pub title: String,
    /// Body/summary text.
    pub content: String,
    /// Publication time (unix seconds) if known.
    pub published_at: Option<i64>,
    /// Owning feed id (uuid string).
    pub feed_id: String,
    /// Owning topic id (uuid string).
    pub topic_id: String,
    /// Content hash for near-duplicate detection.
    pub content_hash: String,
}

/// The result of an idempotent article upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertResult {
    /// The article row id (uuid string), whether inserted or pre-existing.
    pub id: String,
    /// `true` only when this call inserted a brand-new row. A re-seen URL
    /// yields `false`, and the caller must NOT re-enqueue it (the at-least-once
    /// replay-safety rule of M1-6).
    pub inserted: bool,
}

/// Everything the decide stage needs about one article and its topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecideContext {
    /// Article headline.
    pub title: String,
    /// Article body.
    pub content: String,
    /// The topic's relevance prompt.
    pub topic_prompt: String,
    /// The topic's keep threshold (`0..=100`).
    pub threshold: u8,
}

/// Article and feed persistence, in domain terms (no SQL leaks into the core).
pub trait Store {
    /// Load a feed's fetch row, or `None` if missing/disabled.
    fn load_feed(&self, feed_id: &str) -> CoreResult<Option<Feed>>;

    /// Load every enabled feed's scheduling row for due-selection.
    fn load_enabled_feeds(&self) -> CoreResult<Vec<FeedSchedule>>;

    /// Idempotently upsert an article on its unique URL.
    fn upsert_article(&self, article: &NewArticle) -> CoreResult<UpsertResult>;

    /// Record a successful fetch: persist new validators and stamp `last_fetched_at`.
    fn record_feed_success(
        &self,
        feed_id: &str,
        cond: &ConditionalHeaders,
        now: i64,
    ) -> CoreResult<()>;

    /// Record a fetch failure: bump `failure_count`, store `last_error`, stamp time.
    fn record_feed_failure(&self, feed_id: &str, error: &str, now: i64) -> CoreResult<()>;

    /// Load the decide context for an article, or `None` if it is gone.
    fn load_decide_context(&self, article_id: &str) -> CoreResult<Option<DecideContext>>;

    /// Record a decide outcome: set state (`Decided` when `keep`, else
    /// `Discarded`) and persist the score and reason regardless.
    fn record_decision(
        &self,
        article_id: &str,
        score: u8,
        reason: &str,
        keep: bool,
    ) -> CoreResult<()>;

    /// Set an article's pipeline state (used by the stub stages and error paths).
    fn set_state(&self, article_id: &str, state: PipelineState) -> CoreResult<()>;

    /// Record a permanent error against an article: state `Error` plus message.
    fn record_article_error(&self, article_id: &str, error: &str) -> CoreResult<()>;

    /// Load the round-robin scheduling cursor (0 if unset).
    fn load_cursor(&self) -> CoreResult<u64>;

    /// Persist the round-robin scheduling cursor.
    fn save_cursor(&self, cursor: u64) -> CoreResult<()>;
}

// ---------------------------------------------------------------------------
// M2: analysis, entities, vectors
// ---------------------------------------------------------------------------

/// Everything the analyze stage needs about one article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzeContext {
    /// Article headline.
    pub title: String,
    /// Article body.
    pub content: String,
}

/// Everything the embed stage needs about one article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedContext {
    /// Article headline.
    pub title: String,
    /// The analyze stage's summary; falls back to the body when absent.
    pub summary: String,
    /// Canonical names of the entities linked to this article.
    pub entities: Vec<String>,
}

/// A stored feature vector.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredVector {
    /// The vector itself.
    pub vector: Vec<f32>,
    /// The recipe it was produced with (`lex-v1/256`).
    pub recipe: String,
}

/// What one article's entity resolution actually did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityApplyReport {
    /// Entities created by this article.
    pub created: usize,
    /// Existing entities this article linked to.
    pub linked: usize,
    /// New spellings recorded as aliases.
    pub aliases_added: usize,
}

/// Analysis, entity and vector persistence (M2).
///
/// Split from [`Store`] so the M1 surface stays exactly as it shipped and the
/// M2 additions are legible as additions. The plugin implements both on one
/// type over the same `db` host.
pub trait AnalysisStore {
    /// Load the analyze context for an article, or `None` if it is gone.
    fn load_analyze_context(&self, article_id: &str) -> CoreResult<Option<AnalyzeContext>>;

    /// Persist an analysis: the prose fields, the raw model JSON, `analyzed_at`,
    /// and the `Analyzed` state.
    fn record_analysis(
        &self,
        article_id: &str,
        analysis: &Analysis,
        raw: &str,
        now: i64,
    ) -> CoreResult<()>;

    /// Load existing entities whose match key starts with one of `prefixes`,
    /// restricted to `types`, for fuzzy resolution.
    ///
    /// Implementations must bound the result set — the host output buffer is
    /// 256 KB and an unbounded entity table would overflow it.
    fn load_entity_candidates(
        &self,
        prefixes: &[String],
        types: &[EntityType],
    ) -> CoreResult<Vec<EntityRecord>>;

    /// Apply an entity plan: create rows, link the article, append aliases, and
    /// bump per-entity counters and `last_seen_at`.
    fn apply_entity_plan(
        &self,
        article_id: &str,
        plan: &EntityPlan,
        now: i64,
    ) -> CoreResult<EntityApplyReport>;

    /// Load the embed context for an article, or `None` if it is gone.
    fn load_embed_context(&self, article_id: &str) -> CoreResult<Option<EmbedContext>>;

    /// Store (or replace) an article's feature vector.
    fn save_vector(
        &self,
        article_id: &str,
        vector: &[f32],
        recipe: &str,
        now: i64,
    ) -> CoreResult<()>;

    /// Load an article's stored vector, or `None` if it has not been embedded.
    fn load_vector(&self, article_id: &str) -> CoreResult<Option<StoredVector>>;

    /// Add one AI call to the day's spend for `stage`.
    ///
    /// `cost` is the host's per-call estimate; `None` increments the day's
    /// unpriced-call counter instead of the dollar total, because unpriced is
    /// unknown, not free.
    fn record_cost(&self, day: &str, stage: Stage, cost: Option<f64>, now: i64) -> CoreResult<()>;

    /// Today's spend across every stage.
    fn load_daily_spend(&self, day: &str) -> CoreResult<DailySpend>;

    /// Null out the body text of terminal articles published before `cutoff`,
    /// keeping metadata and scores. Returns how many rows were reclaimed.
    fn purge_article_content(&self, cutoff: i64, now: i64, limit: usize) -> CoreResult<u64>;
}

// ---------------------------------------------------------------------------
// M2: stories
// ---------------------------------------------------------------------------

/// The article being clustered, as the store hands it to the cluster stage.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterContext {
    /// Topic the article belongs to.
    pub topic_id: Option<String>,
    /// Feed the article came from.
    pub feed_id: Option<String>,
    /// Publication time (unix seconds).
    pub published_at: Option<i64>,
    /// Relevance score from the decide stage.
    pub relevance_score: Option<i32>,
    /// How many times clustering has already deferred this article.
    pub waits: u32,
}

/// The seed for a brand-new story.
#[derive(Debug, Clone, PartialEq)]
pub struct StorySeed {
    /// Title for the story Item until the first summarize runs.
    pub title: String,
    /// Topic the story belongs to.
    pub topic_id: Option<String>,
    /// The founding article's vector, which becomes the initial centroid.
    pub centroid: Vec<f32>,
    /// The recipe that vector was built with.
    pub recipe: String,
    /// The founding article's publication time.
    pub published_at: Option<i64>,
    /// The founding article's relevance score.
    pub relevance_score: Option<i32>,
}

/// A story's operational row.
#[derive(Debug, Clone, PartialEq)]
pub struct StoryRow {
    /// Story id (also the `argus_story` Item id).
    pub id: String,
    /// The running centroid.
    pub centroid: Vec<f32>,
    /// The recipe the centroid was built with.
    pub recipe: String,
    /// Members counted toward the story.
    pub article_count: u32,
    /// Publication time of the newest member.
    pub last_article_at: Option<i64>,
    /// When this story was last summarized.
    pub last_summarized_at: Option<i64>,
    /// The story's current headline (the placeholder until the first
    /// summarize). M4 reads it to name a notification.
    pub title: String,
    /// The story's current narrative, **before** the summarize about to
    /// overwrite it. This is what the M4 change judge compares against, and it
    /// is why the notification decision is made inside `run_summarize` rather
    /// than after it: once the row is rewritten the previous text is gone, and
    /// `save-item` replaces rather than merges (G-ITEM-NO-MERGE), so there is no
    /// history to recover it from.
    pub summary: String,
    /// The topic the story belongs to, for the topic's notification priority.
    pub topic_id: Option<String>,
    /// The story's relevance score, checked against the notify threshold.
    pub relevance_score: Option<i32>,
}

/// Story persistence: the operational row plus the `argus_story` Item (M2).
///
/// Every method here writes both halves where both are affected, because a
/// story that exists as a row but not as an Item is invisible to readers, and
/// an Item with no row can never be summarized.
pub trait StoryStore {
    /// Try to take the exclusive clustering lease until `now + lease_seconds`.
    ///
    /// Clustering is the one stage that must not run concurrently with itself:
    /// two workers scoring the same event at the same moment both see no
    /// candidate story and both create one, permanently splitting a story that
    /// should be single. The kernel cannot express this — `tap_queue_info`
    /// concurrency is collapsed to one per-plugin maximum across every queue
    /// (`crates/kernel/src/cron/mod.rs`, `plugin_concurrency`), so declaring
    /// `cluster: 1` beside `fetch: 4` yields 4 for both — so the mutual
    /// exclusion lives here instead.
    ///
    /// Returns `false` when another worker holds the lease; the caller defers
    /// rather than proceeding. `token` identifies the holder so a release
    /// cannot free somebody else's lease, and the lease expires on its own so a
    /// worker that traps mid-job cannot wedge the stage.
    fn try_acquire_cluster_lease(
        &self,
        token: &str,
        now: i64,
        lease_seconds: i64,
    ) -> CoreResult<bool>;

    /// Release the clustering lease, if `token` still holds it.
    fn release_cluster_lease(&self, token: &str) -> CoreResult<()>;

    /// Load one article's clustering context, or `None` if it is gone.
    fn load_cluster_context(&self, article_id: &str) -> CoreResult<Option<ClusterContext>>;

    /// Active stories that could plausibly host `article_id`: same topic or a
    /// shared entity, inside the publication window, newest first.
    ///
    /// Implementations must apply `limit` — the candidate rows carry vectors and
    /// the host output buffer is 256 KB.
    fn load_candidate_stories(
        &self,
        article_id: &str,
        window_start: i64,
        limit: usize,
    ) -> CoreResult<Vec<CandidateStory>>;

    /// Stored articles that could be the original of `article_id`: a shared
    /// entity, inside the window, from any feed (the caller filters on feed).
    fn load_candidate_articles(
        &self,
        article_id: &str,
        window_start: i64,
        limit: usize,
    ) -> CoreResult<Vec<CandidateArticle>>;

    /// Create a story: save the `argus_story` Item, then insert the operational
    /// row keyed on the Item's id, then file `article_id` into it. Returns the
    /// new story id.
    fn create_story(&self, seed: &StorySeed, article_id: &str, now: i64) -> CoreResult<String>;

    /// File an article into an existing story, folding its vector into the
    /// centroid and refreshing the story's counters and Item fields.
    fn join_story(
        &self,
        story_id: &str,
        article_id: &str,
        vector: &[f32],
        ctx: &ClusterContext,
        now: i64,
    ) -> CoreResult<()>;

    /// Mark an article as a near-duplicate of another, filing it into that
    /// article's story as a source without counting it as a member.
    fn mark_duplicate(
        &self,
        article_id: &str,
        of_article_id: &str,
        story_id: Option<&str>,
        now: i64,
    ) -> CoreResult<()>;

    /// Record that clustering deferred an article: bump its wait counter and
    /// park it in [`PipelineState::Waiting`].
    fn record_wait(&self, article_id: &str, now: i64) -> CoreResult<()>;

    /// Load a story's operational row, or `None` if it is gone.
    fn load_story(&self, story_id: &str) -> CoreResult<Option<StoryRow>>;

    /// Load a story's members for the summarize prompt, newest first.
    fn load_story_members(&self, story_id: &str, limit: usize) -> CoreResult<Vec<StoryMember>>;

    /// Write a synthesized summary onto the story Item and its row: title,
    /// summary, the `sources` json, `summary_updated_at` and relevance.
    fn record_story_summary(
        &self,
        story_id: &str,
        summary: &StorySummary,
        members: &[StoryMember],
        now: i64,
    ) -> CoreResult<()>;

    /// Clear a story's pending-summarize flag without summarizing it.
    fn clear_summarize_pending(&self, story_id: &str) -> CoreResult<()>;

    /// Retire stories whose newest member is older than `cutoff`. Returns how
    /// many were retired.
    fn deactivate_stale_stories(&self, cutoff: i64, now: i64) -> CoreResult<u64>;

    /// Articles parked in [`PipelineState::Waiting`] since before `cutoff`, for
    /// the maintenance pass to re-enqueue.
    fn load_waiting_articles(&self, cutoff: i64, limit: usize) -> CoreResult<Vec<String>>;
}

// ---------------------------------------------------------------------------
// M4: notifications
// ---------------------------------------------------------------------------

/// A feed that has failed its last `n` fetches, as the alert pass sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingFeed {
    /// The feed Item's id.
    pub id: String,
    /// The feed's name, for the alert body.
    pub name: String,
    /// Consecutive failures.
    pub failure_count: u32,
    /// The last error recorded against it.
    pub last_error: String,
}

/// How the plugin's own queue looks right now.
///
/// Read from `plugin_queue` — a kernel table — because queue v2 exposes no host
/// function a plugin can ask (`M4-DESIGN.md` Decision 9, G-QUEUE-NO-INTROSPECTION).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueHealth {
    /// Age in seconds of the oldest claimable job whose delay has expired, or
    /// `0` when there is nothing waiting.
    pub oldest_ready_age: i64,
    /// Jobs eligible to run right now.
    pub ready: u64,
    /// Jobs that have dead-lettered.
    pub dead: u64,
}

/// The notification outbox, its deliveries, and the operational reads the alert
/// pass needs (M4).
///
/// Split from the other stores for the same reason [`AnalysisStore`] was: the
/// M1–M3 surfaces stay exactly as they shipped and the M4 additions read as
/// additions. The plugin implements all four on one type over the same `db` host.
pub trait NotifyStore {
    /// Record a notification decision, returning the new event's id — or `None`
    /// when `(kind, dedup_key)` already exists.
    ///
    /// `None` is the normal outcome of an at-least-once redelivery, not an
    /// error: the decision was already made and the caller must not enqueue a
    /// second dispatch for it.
    fn record_event(&self, event: &crate::notify::NewEvent, now: i64)
    -> CoreResult<Option<String>>;

    /// Load an outbox row, or `None` if it is gone.
    fn load_event(&self, event_id: &str) -> CoreResult<Option<crate::notify::StoredEvent>>;

    /// Every enabled (published) channel, in a stable order.
    fn load_channels(&self) -> CoreResult<Vec<crate::notify::ChannelConfig>>;

    /// One channel by id, or `None` when it is gone or has been disabled since
    /// the job that targets it was enqueued.
    fn load_channel(&self, channel_id: &str) -> CoreResult<Option<crate::notify::ChannelConfig>>;

    /// When a notification about `subject_id` was last **sent**, for debouncing.
    ///
    /// Scoped to the subject rather than the event kind: a story that notified
    /// as new and then updates twice in ten minutes has been notified about
    /// once, whatever the kinds involved.
    fn last_sent_at(&self, subject_id: &str) -> CoreResult<Option<i64>>;

    /// A topic's notification priority, from its configuration Item.
    fn topic_priority(&self, topic_id: &str) -> CoreResult<crate::notify::NotifyPriority>;

    /// How many *other* pending, due, normal-priority digestible events were
    /// created at or after `window_start`.
    ///
    /// Read before [`NotifyStore::claim_digest`] so the collapse only happens
    /// when it will actually reach the threshold: claiming is destructive, and
    /// claiming four events that never become a digest would silently swallow
    /// four notifications.
    fn pending_digestible(
        &self,
        head_event_id: &str,
        window_start: i64,
        now: i64,
    ) -> CoreResult<usize>;

    /// Claim every other pending, due, normal-priority digestible event created
    /// at or after `window_start`, folding it into `head_event_id`.
    ///
    /// Must be one statement that both selects and marks
    /// [`crate::notify::EventState::Digested`], so two workers cannot fold the
    /// same event into two digests — the plugin has no transaction to lean on
    /// (G-DB-NO-TX).
    fn claim_digest(
        &self,
        head_event_id: &str,
        window_start: i64,
        limit: usize,
        now: i64,
    ) -> CoreResult<Vec<crate::notify::StoredEvent>>;

    /// Rewrite an event row as the digest it became: its kind, title, body and
    /// data.
    ///
    /// Written before dispatch so that every later job touching this
    /// event — a per-channel retry, a channel that overflowed
    /// [`crate::ratelimit::MAX_CHANNELS_PER_DISPATCH`] — reloads the digest and
    /// not the single story it started as.
    fn promote_to_digest(
        &self,
        event_id: &str,
        digest: &crate::notify::Notification,
        folded: usize,
        now: i64,
    ) -> CoreResult<()>;

    /// Move an event to a terminal state, with a reason for a suppression.
    fn set_event_state(
        &self,
        event_id: &str,
        state: crate::notify::EventState,
        reason: Option<&str>,
        now: i64,
    ) -> CoreResult<()>;

    /// Push an event's earliest-send instant out to `at` (quiet hours).
    fn reschedule_event(&self, event_id: &str, at: i64) -> CoreResult<()>;

    /// Record what one channel did with one event, bumping its attempt count.
    /// Returns the attempt number this call recorded, starting at 1.
    fn record_delivery(
        &self,
        event_id: &str,
        outcome: &crate::notify::ChannelOutcome,
        now: i64,
    ) -> CoreResult<u32>;

    /// Update a channel's rolling health: consecutive failures and the last
    /// error. Bounded to a counter and one message — a per-channel error *log*
    /// is a table that only ever grows.
    fn note_channel_health(
        &self,
        channel_id: &str,
        ok: bool,
        error: Option<&str>,
        now: i64,
    ) -> CoreResult<()>;

    /// Feeds whose consecutive failure count has reached `threshold`.
    fn failing_feeds(&self, threshold: u32, limit: usize) -> CoreResult<Vec<FailingFeed>>;

    /// The plugin's own queue health.
    fn queue_health(&self, now: i64) -> CoreResult<QueueHealth>;
}

// ---------------------------------------------------------------------------
// Queue (M1-8)
// ---------------------------------------------------------------------------

/// A queue job payload: the stage discriminator plus the row id it operates on.
///
/// The kernel drains a plugin's single `tap_queue_worker` with the bare
/// payload, so [`Stage`] here is how the worker self-routes to a stage handler.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobPayload {
    /// Which stage should process `id`.
    pub stage: Stage,
    /// The row id (article id for most stages, feed id for `Fetch`, a
    /// notification event id for `Notify`).
    pub id: String,
    /// Narrows a [`Stage::Notify`] job to one channel, for a per-channel retry
    /// or for the overflow past [`crate::ratelimit::MAX_CHANNELS_PER_DISPATCH`].
    ///
    /// `#[serde(default)]` and omitted when absent, so every payload already
    /// sitting in a live queue when this shipped deserializes unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

impl JobPayload {
    /// A whole-job payload: this stage, this row, every channel.
    #[must_use]
    pub fn new(stage: Stage, id: impl Into<String>) -> Self {
        Self {
            stage,
            id: id.into(),
            channel: None,
        }
    }

    /// A [`Stage::Notify`] payload narrowed to one channel.
    #[must_use]
    pub fn for_channel(event_id: impl Into<String>, channel_id: impl Into<String>) -> Self {
        Self {
            stage: Stage::Notify,
            id: event_id.into(),
            channel: Some(channel_id.into()),
        }
    }
}

/// Enqueue options mapped onto the kernel `QueueOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnqueueOpts {
    /// Higher drains first.
    pub priority: i32,
    /// Seconds to defer the first attempt.
    pub delay: i64,
}

/// The stage-handoff queue (kernel queue v2: retries, backoff, DLQ, priority).
pub trait JobQueue {
    /// Enqueue the next stage's job.
    fn enqueue(&self, job: &JobPayload, opts: EnqueueOpts) -> CoreResult<()>;
}
