//! Stage orchestration wired over the injected ports.
//!
//! Each `run_*` function is the pure body of one pipeline stage: it reads and
//! writes through the [`Store`], talks to the model through the [`LlmProvider`],
//! fetches through the [`Fetcher`], and hands off through the [`JobQueue`]. The
//! plugin's single `tap_queue_worker` self-routes a job's [`Stage`] to the
//! matching function here.
//!
//! # The retry contract
//!
//! Queue v2 treats a normal worker return as success (the row is deleted) and a
//! panic as a failed attempt (retry → backoff → dead-letter). These functions
//! encode that as their `Result`:
//!
//! - `Ok(_)` — a terminal outcome that must NOT be retried (ingested, decided,
//!   discarded, article/feed gone, malformed feed/model output already recorded).
//! - `Err(e)` — always transient by construction ([`crate::error::CoreError::is_transient`]
//!   holds for every propagated error). The worker maps this to a `panic!` so
//!   the kernel retries with backoff and eventually dead-letters.
//!
//! Permanent failures never propagate: they are converted into a recorded
//! terminal state and an `Ok`.

use crate::analyze::{Analysis, analyze};
use crate::budget::{BudgetConfig, BudgetVerdict, DailySpend, seconds_until_next_day, utc_day};
use crate::cluster::{ClusterConfig, ClusterDecision, IncomingArticle};
use crate::decide::decide;
use crate::dedup::content_hash;
use crate::embed::{feature_vector, recipe_id};
use crate::entity;
use crate::error::CoreResult;
use crate::feed::parse_feed;
use crate::judge;
use crate::model::{PipelineState, Stage};
use crate::notify::{
    self, ChannelConfig, DeliveryState, EventKind, EventState, NewEvent, Notification,
    NotifyPriority, Transport,
};
use crate::ports::{
    AnalysisStore, ClusterContext, ConditionalHeaders, EmbedContext, EnqueueOpts,
    EntityApplyReport, FetchOutcome, Fetcher, JobPayload, JobQueue, LlmProvider, NewArticle,
    NotifyStore, Store, StorySeed, StoryStore,
};
use crate::ratelimit::{self, NotifyConfig, SendVerdict};
use crate::summarize::{self, StorySummary};

/// Priority for decide jobs enqueued off a fetch (drained ahead of default).
pub const DECIDE_PRIORITY: i32 = 10;
/// Priority for analyze jobs enqueued off a survivor.
pub const ANALYZE_PRIORITY: i32 = 5;
/// Priority for embed jobs. Cheap and CPU-only, so they drain behind the AI
/// stages rather than ahead of them.
pub const EMBED_PRIORITY: i32 = 3;
/// Priority for cluster jobs.
pub const CLUSTER_PRIORITY: i32 = 2;
/// Priority for summarize jobs. Lowest: a story is worth summarizing once its
/// members have settled, and deferring costs nothing but freshness.
pub const SUMMARIZE_PRIORITY: i32 = 1;

/// Most candidate stories loaded for one clustering decision.
///
/// Candidate rows carry a full vector each, and they come back through the
/// 256 KB host output buffer, so this is a hard correctness bound and not a
/// performance knob. At the default dimension of 256 a candidate is roughly
/// 2 KB of JSON, so 64 leaves comfortable headroom.
pub const MAX_STORY_CANDIDATES: usize = 64;

/// Most candidate articles loaded for near-duplicate detection. Same bound,
/// same reason.
pub const MAX_ARTICLE_CANDIDATES: usize = 64;

/// Most story members loaded for one summarize call.
pub const MAX_STORY_MEMBERS: usize = 50;

/// Most articles whose content one retention pass reclaims. Bounds the work a
/// single cron tick does, so a large backlog drains over several ticks instead
/// of blowing the tap's shared dispatch budget.
pub const MAX_PURGE_PER_PASS: usize = 500;

/// Most deferred articles one maintenance pass re-enqueues.
pub const MAX_WAITING_PER_PASS: usize = 200;

/// What a fetch-stage run did (for logging and tests).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchReport {
    /// The server returned 304 and nothing was ingested.
    pub not_modified: bool,
    /// Number of articles parsed from the body.
    pub parsed: usize,
    /// Number of brand-new articles inserted (and hence enqueued to decide).
    pub ingested: usize,
    /// Whether the fetch failed permanently and the feed was flagged.
    pub feed_flagged: bool,
}

/// What a decide-stage run did.
#[derive(Debug, Clone, PartialEq)]
pub struct DecideReport {
    /// The article was missing (already deleted); nothing to do.
    pub missing: bool,
    /// The relevance score recorded (`None` when missing or unparseable).
    pub score: Option<u8>,
    /// Whether the article was kept (enqueued to analyze).
    pub kept: bool,
    /// Host-reported dollar cost of the decide call (G-COST-OPAQUE, p11j), read
    /// from the response rather than a kernel-side SQL query. `None` when the
    /// article was missing, the model output was unparseable (no successful
    /// call), or the model is unpriced.
    pub cost_estimate: Option<f64>,
}

/// Run the fetch stage for one feed: conditional GET, parse, idempotent ingest,
/// enqueue survivors to decide, and track per-feed success/failure.
///
/// See the module retry contract: a malformed feed or a refused (SSRF-blocked /
/// 4xx) URL is a *permanent* per-feed failure — it is recorded and returns
/// `Ok` (the worker does not crash and the job is not retried). A transient
/// network failure records the failure *and* propagates `Err` so the queue
/// retries with backoff.
///
/// # Errors
///
/// Propagates transient [`crate::error::CoreError`]s (network fetch, store, queue) for retry.
pub fn run_fetch<F: Fetcher + ?Sized, S: Store + ?Sized, Q: JobQueue + ?Sized>(
    fetcher: &F,
    store: &S,
    queue: &Q,
    feed_id: &str,
    now: i64,
) -> CoreResult<FetchReport> {
    let Some(feed) = store.load_feed(feed_id)? else {
        // Feed disabled or deleted between scheduling and drain — drop the job.
        return Ok(FetchReport::default());
    };

    let outcome = match fetcher.fetch(&feed.url, &feed.conditional) {
        Ok(o) => o,
        Err(e) if e.is_transient() => {
            // Track the failure at the feed level, then let the queue retry.
            store.record_feed_failure(feed_id, &e.to_string(), now)?;
            return Err(e);
        }
        Err(e) => {
            // Permanent: refused (SSRF/4xx). Flag the feed, do not retry-storm.
            store.record_feed_failure(feed_id, &e.to_string(), now)?;
            return Ok(FetchReport {
                feed_flagged: true,
                ..Default::default()
            });
        }
    };

    let (body, new_cond) = match outcome {
        FetchOutcome::NotModified => {
            // Nothing changed; just stamp the fetch time, keeping validators.
            store.record_feed_success(feed_id, &feed.conditional, now)?;
            return Ok(FetchReport {
                not_modified: true,
                ..Default::default()
            });
        }
        FetchOutcome::Fetched {
            body,
            etag,
            last_modified,
        } => (
            body,
            ConditionalHeaders {
                etag,
                last_modified,
            },
        ),
    };

    let parsed = match parse_feed(&body) {
        Ok(p) => p,
        Err(e) => {
            // Malformed feed body — permanent. Flag and stop.
            store.record_feed_failure(feed_id, &e.to_string(), now)?;
            return Ok(FetchReport {
                feed_flagged: true,
                ..Default::default()
            });
        }
    };

    let mut ingested = 0usize;
    for art in &parsed {
        let new = NewArticle {
            url: art.url.clone(),
            title: art.title.clone(),
            content: art.content.clone(),
            published_at: art.published_at,
            feed_id: feed.id.clone(),
            topic_id: feed.topic_id.clone(),
            content_hash: content_hash(&art.title, &art.content),
        };
        let res = store.upsert_article(&new)?;
        // Only a brand-new row is enqueued — the at-least-once replay-safety
        // rule (M1-6): a re-seen URL updates nothing and is not re-enqueued.
        if res.inserted {
            ingested += 1;
            queue.enqueue(
                &JobPayload::new(Stage::Decide, res.id),
                EnqueueOpts {
                    priority: DECIDE_PRIORITY,
                    delay: 0,
                },
            )?;
        }
    }

    store.record_feed_success(feed_id, &new_cond, now)?;
    Ok(FetchReport {
        parsed: parsed.len(),
        ingested,
        ..Default::default()
    })
}

/// Run the decide stage for one article: exactly one model call, defensively
/// parse the score, record the decision, and enqueue survivors to analyze.
///
/// A malformed-but-received model response is permanent: the article is marked
/// `Discarded` with the parse error as its reason and the function returns
/// `Ok` (no retry). A provider failure is transient and propagates for retry.
///
/// # Errors
///
/// Propagates transient provider/store/queue [`crate::error::CoreError`]s for retry.
pub fn run_decide<P: LlmProvider + ?Sized, S: Store + ?Sized, Q: JobQueue + ?Sized>(
    provider: &P,
    store: &S,
    queue: &Q,
    article_id: &str,
    decide_model: Option<String>,
) -> CoreResult<DecideReport> {
    let Some(ctx) = store.load_decide_context(article_id)? else {
        return Ok(DecideReport {
            missing: true,
            score: None,
            kept: false,
            cost_estimate: None,
        });
    };

    match decide(
        provider,
        decide_model,
        &ctx.topic_prompt,
        ctx.threshold,
        &ctx.title,
        &ctx.content,
    ) {
        Ok((decision, keep, cost_estimate)) => {
            store.record_decision(article_id, decision.score, &decision.reason, keep)?;
            if keep {
                queue.enqueue(
                    &JobPayload::new(Stage::Analyze, article_id),
                    EnqueueOpts {
                        priority: ANALYZE_PRIORITY,
                        delay: 0,
                    },
                )?;
            }
            Ok(DecideReport {
                missing: false,
                score: Some(decision.score),
                kept: keep,
                cost_estimate,
            })
        }
        Err(e) if e.is_transient() => Err(e),
        Err(e) => {
            // Permanent parse failure: discard with the reason, no retry.
            store.record_decision(
                article_id,
                0,
                &format!("unparseable model output: {e}"),
                false,
            )?;
            Ok(DecideReport {
                missing: false,
                score: None,
                kept: false,
                cost_estimate: None,
            })
        }
    }
}

// ===========================================================================
// M2: analyze → extract → embed → cluster → summarize
// ===========================================================================

/// Tunables for the M2 stages, read from site config by the plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct StageConfig {
    /// Feature-vector dimension.
    pub vector_dim: usize,
    /// Fuzzy entity-match threshold.
    pub entity_threshold: f64,
    /// Clustering tunables.
    pub cluster: ClusterConfig,
    /// Minimum seconds between two summaries of one story.
    pub summarize_min_interval: i64,
    /// Days after which a terminal article's body text is reclaimed.
    pub article_retention_days: i64,
    /// Budget thresholds.
    pub budget: BudgetConfig,
    /// Embeddings model for the **semantic** route, from `argus.embed_model`.
    ///
    /// `None` keeps M2's deterministic lexical vectors. `Some(model)` sends the
    /// article's text to a real embeddings endpoint through the `ai-request`
    /// host, which only became possible at `KERNEL_API_VERSION (0,99)`
    /// (`G-AI-EMBED-UNROUTED`). Opt-in rather than the default because a site
    /// with no embeddings provider configured must keep working, and the
    /// lexical route needs no provider, spends nothing and cannot fail.
    pub embed_model: Option<String>,
}

impl Default for StageConfig {
    fn default() -> Self {
        Self {
            vector_dim: crate::embed::DEFAULT_DIMENSION,
            entity_threshold: entity::DEFAULT_MATCH_THRESHOLD,
            cluster: ClusterConfig::default(),
            summarize_min_interval: summarize::DEFAULT_MIN_INTERVAL_SECONDS,
            article_retention_days: 180,
            budget: BudgetConfig::default(),
            embed_model: None,
        }
    }
}

impl StageConfig {
    /// The recipe a vector stored under this configuration carries, and the
    /// only recipe the cluster stage will compare against.
    ///
    /// One function so the writer ([`run_embed`]) and the reader
    /// ([`run_cluster`]) cannot disagree. Switching `embed_model` on or off
    /// changes the recipe, so vectors from the other route are *skipped* by the
    /// existing recipe check rather than compared across incompatible spaces —
    /// the cutover is visible instead of quietly wrong, and the cluster stage
    /// re-enqueues an embed job for each stale article.
    #[must_use]
    pub fn vector_recipe(&self) -> String {
        match self.embed_model {
            Some(ref model) => crate::embed::semantic_recipe_id(model),
            None => recipe_id(crate::embed::clamp_dimension(self.vector_dim)),
        }
    }
}

/// What an analyze-stage run did.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzeReport {
    /// The article was gone; nothing to do.
    pub missing: bool,
    /// The daily budget is exhausted; the job was deferred to the next UTC day
    /// and no model call was made.
    pub paused: bool,
    /// The model answered but the answer carried no analysis; the article was
    /// flagged and will not be retried.
    pub unparseable: bool,
    /// Entities the extract step created and linked.
    pub entities: EntityApplyReport,
    /// Host-reported cost of the analyze call.
    pub cost_estimate: Option<f64>,
}

/// What an embed-stage run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmbedReport {
    /// The article was gone.
    pub missing: bool,
    /// The vector came out all-zero — the article had no usable tokens, so it
    /// will match nothing and start a story of its own.
    pub empty: bool,
}

/// What a cluster-stage run did.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterReport {
    /// The article was gone.
    pub missing: bool,
    /// The article had no comparable vector; an embed job was re-enqueued.
    pub re_embedded: bool,
    /// Another worker held the clustering lease; the job was re-enqueued.
    pub lease_busy: bool,
    /// The decision taken, when one was.
    pub decision: Option<ClusterDecision>,
    /// The story the article ended up in, if any.
    pub story_id: Option<String>,
    /// Candidate stories considered.
    pub candidates: usize,
}

/// How long one worker may hold the clustering lease.
///
/// Comfortably longer than a cluster job (two bounded queries and a few
/// writes), comfortably shorter than the 150 s background epoch, so a worker
/// killed by the epoch deadline cannot hold the stage past its own death.
pub const CLUSTER_LEASE_SECONDS: i64 = 30;

/// How long a cluster job waits before retrying a busy lease.
pub const CLUSTER_LEASE_RETRY_SECONDS: i64 = 2;

/// What a summarize-stage run did.
#[derive(Debug, Clone, PartialEq)]
pub struct SummarizeReport {
    /// The story was gone.
    pub missing: bool,
    /// The daily budget is exhausted; deferred to the next UTC day.
    pub paused: bool,
    /// The rate limit deferred this run; the job was re-enqueued for the
    /// instant the story becomes eligible.
    pub deferred_seconds: i64,
    /// The story had no members to summarize.
    pub empty: bool,
    /// The model answered but carried no summary; the story keeps its old one.
    pub unparseable: bool,
    /// Members described to the model.
    pub members: usize,
    /// Host-reported cost of the summarize call.
    pub cost_estimate: Option<f64>,
    /// What this synthesis changed, when it changed anything — the input to the
    /// M4 notification trigger.
    ///
    /// Carried out of the stage rather than acted on inside it because the
    /// previous narrative is only readable *before* the write, and because a
    /// notification decision that made a second AI call inside the summarize job
    /// would break the one-call-per-job rule the 150 s epoch imposes.
    pub change: Option<StoryChange>,
}

/// A story as it stood before and after one summarize, for the notify trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryChange {
    /// The story's id.
    pub story_id: String,
    /// `true` when this was the story's first synthesis.
    pub is_new: bool,
    /// The narrative the story had before this run. Empty for a new story.
    pub previous_summary: String,
    /// The story's new headline.
    pub title: String,
    /// The story's new narrative.
    pub summary: String,
    /// The topic the story belongs to, whose priority may make this loud.
    pub topic_id: Option<String>,
    /// The story's relevance score, checked against the notify threshold.
    pub relevance_score: Option<i32>,
    /// Members counted toward the story after this run.
    ///
    /// Also the notification's idempotency key: a redelivered summarize job
    /// re-synthesizes the same member set and therefore derives the same key, so
    /// a replay cannot notify twice even though the model's wording may differ.
    pub article_count: usize,
}

/// What one maintenance pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MaintenanceReport {
    /// Stories retired for going idle.
    pub stories_retired: u64,
    /// Articles whose body text was reclaimed.
    pub articles_purged: u64,
    /// Deferred articles re-enqueued for clustering.
    pub waiting_requeued: usize,
}

/// Check the daily budget and, when it is exhausted, defer `stage`'s job to the
/// next UTC day.
///
/// Returns `Ok(true)` when the caller must stop. Deferring rather than failing
/// is deliberate: a paused stage that panicked would burn its five queue
/// attempts against a limit that will not move for hours and dead-letter work
/// that was never wrong.
///
/// # Errors
///
/// Propagates transient store/queue failures for retry.
fn budget_gate<A: AnalysisStore + ?Sized, Q: JobQueue + ?Sized>(
    store: &A,
    queue: &Q,
    stage: Stage,
    id: &str,
    config: &BudgetConfig,
    now: i64,
) -> CoreResult<(bool, DailySpend)> {
    let day = utc_day(now);
    let spend = store.load_daily_spend(&day)?;
    match crate::budget::verdict(spend.spent_usd, config) {
        BudgetVerdict::Pause => {
            queue.enqueue(
                &JobPayload::new(stage, id),
                EnqueueOpts {
                    priority: 0,
                    delay: seconds_until_next_day(now),
                },
            )?;
            Ok((true, spend))
        }
        BudgetVerdict::Ok | BudgetVerdict::Warn => Ok((false, spend)),
    }
}

/// Resolve an analysis's entities against the store and apply the plan.
///
/// Pure decision, impure application: the store hands back candidates, the
/// [`entity`] module decides link-versus-create, and the store applies the
/// result. An article with no entities skips both round trips.
///
/// # Errors
///
/// Propagates transient store failures for retry.
fn extract_entities<A: AnalysisStore + ?Sized>(
    store: &A,
    article_id: &str,
    analysis: &Analysis,
    threshold: f64,
    now: i64,
) -> CoreResult<EntityApplyReport> {
    let normalized = entity::normalize_all(&analysis.entities);
    if normalized.is_empty() {
        return Ok(EntityApplyReport::default());
    }
    let prefixes = entity::candidate_prefixes(&normalized);
    let types: Vec<_> = {
        let mut t: Vec<_> = normalized.iter().map(|e| e.entity_type).collect();
        t.sort_unstable();
        t.dedup();
        t
    };
    let candidates = store.load_entity_candidates(&prefixes, &types)?;
    let plan = entity::resolve(&normalized, &candidates, threshold);
    store.apply_entity_plan(article_id, &plan, now)
}

/// Run the analyze stage for one article: one model call, entity extraction
/// from the same response, and handoff to embed.
///
/// The extract step is inside this job rather than beside it because it needs
/// no model call and the entities are already in hand — a separate stage would
/// buy a queue round trip and a second load of the same row for nothing.
///
/// # Errors
///
/// Propagates transient provider/store/queue failures for retry. A model
/// response that carries no analysis is permanent: the article is flagged and
/// the function returns `Ok`.
pub fn run_analyze<
    P: LlmProvider + ?Sized,
    S: Store + ?Sized,
    A: AnalysisStore + ?Sized,
    Q: JobQueue + ?Sized,
>(
    provider: &P,
    store: &S,
    analysis_store: &A,
    queue: &Q,
    article_id: &str,
    model: Option<String>,
    config: &StageConfig,
    now: i64,
) -> CoreResult<AnalyzeReport> {
    let empty = AnalyzeReport {
        missing: false,
        paused: false,
        unparseable: false,
        entities: EntityApplyReport::default(),
        cost_estimate: None,
    };

    let (paused, _) = budget_gate(
        analysis_store,
        queue,
        Stage::Analyze,
        article_id,
        &config.budget,
        now,
    )?;
    if paused {
        return Ok(AnalyzeReport {
            paused: true,
            ..empty
        });
    }

    let Some(ctx) = analysis_store.load_analyze_context(article_id)? else {
        return Ok(AnalyzeReport {
            missing: true,
            ..empty
        });
    };

    let outcome = match analyze(provider, model, &ctx.title, &ctx.content) {
        Ok(o) => o,
        Err(e) if e.is_transient() => return Err(e),
        Err(e) => {
            // The call succeeded and the answer was unusable. Retrying the same
            // prompt against the same model is not expected to help, so the
            // article is flagged rather than requeued.
            store.record_article_error(article_id, &e.to_string())?;
            return Ok(AnalyzeReport {
                unparseable: true,
                ..empty
            });
        }
    };

    // Cost is recorded before anything else can fail, so a crash between the
    // call and the write cannot lose spend that was actually incurred.
    analysis_store.record_cost(&utc_day(now), Stage::Analyze, outcome.cost_estimate, now)?;
    analysis_store.record_analysis(article_id, &outcome.analysis, &outcome.raw, now)?;

    let entities = extract_entities(
        analysis_store,
        article_id,
        &outcome.analysis,
        config.entity_threshold,
        now,
    )?;

    queue.enqueue(
        &JobPayload::new(Stage::Embed, article_id),
        EnqueueOpts {
            priority: EMBED_PRIORITY,
            delay: 0,
        },
    )?;

    Ok(AnalyzeReport {
        entities,
        cost_estimate: outcome.cost_estimate,
        ..empty
    })
}

/// Run the embed stage for one article: produce its vector, store it, and hand
/// off to cluster.
///
/// Two routes, selected by [`StageConfig::embed_model`]:
///
/// - **Semantic** (`Some(model)`): the article's text goes to a real embeddings
///   endpoint through the provider port. Possible only from
///   `KERNEL_API_VERSION (0,99)`, where the host began routing
///   `operation: Embedding` to `/embeddings` instead of posting it to
///   `/chat/completions` with an empty `messages` array
///   (**G-AI-EMBED-UNROUTED**) — the gap that made M2 ship the other route.
/// - **Lexical** (`None`, the default): the deterministic hashing-trick vector
///   from [`crate::embed`]. No provider, no cost, cannot fail.
///
/// The two are stored under different recipes ([`StageConfig::vector_recipe`]),
/// so switching routes retires the old vectors visibly rather than comparing
/// across incompatible spaces.
///
/// A provider failure on the semantic route **propagates** rather than falling
/// back to a lexical vector. A silent fallback would write a vector under the
/// semantic recipe that is not a semantic vector, which is precisely the class
/// of quiet wrongness the recipe field exists to prevent; the queue's retry is
/// the right answer to a transient provider.
///
/// # Errors
///
/// Propagates transient store/queue/provider failures for retry.
pub fn run_embed<
    S: Store + ?Sized,
    A: AnalysisStore + ?Sized,
    Q: JobQueue + ?Sized,
    P: LlmProvider + ?Sized,
>(
    store: &S,
    analysis_store: &A,
    queue: &Q,
    provider: &P,
    article_id: &str,
    config: &StageConfig,
    now: i64,
) -> CoreResult<EmbedReport> {
    let Some(ctx) = analysis_store.load_embed_context(article_id)? else {
        return Ok(EmbedReport {
            missing: true,
            empty: false,
        });
    };

    let vector = match config.embed_model {
        Some(ref model) => {
            let mut vector = provider
                .embed(&semantic_embed_text(&ctx), Some(model))?
                .vector;
            // Providers overwhelmingly return unit vectors, but nothing in the
            // API promises it and `fold_centroid` averages members — so
            // normalize rather than assume.
            crate::embed::l2_normalize(&mut vector);
            vector
        }
        None => feature_vector(&ctx.title, &ctx.summary, &ctx.entities, config.vector_dim),
    };

    let empty = vector.is_empty() || vector.iter().all(|x| *x == 0.0);
    analysis_store.save_vector(article_id, &vector, &config.vector_recipe(), now)?;
    store.set_state(article_id, PipelineState::Embedded)?;

    queue.enqueue(
        &JobPayload::new(Stage::Cluster, article_id),
        EnqueueOpts {
            priority: CLUSTER_PRIORITY,
            delay: 0,
        },
    )?;

    Ok(EmbedReport {
        missing: false,
        empty,
    })
}

/// The text handed to an embeddings model for one article.
///
/// Title, then the analyze stage's summary, then the linked entity names. The
/// lexical route weights those three sources explicitly (entities heaviest); an
/// embedding model does its own weighting, so the job here is only to give it
/// every signal in a stable order. Entities are included because two reports of
/// one event often share names before they share phrasing.
fn semantic_embed_text(ctx: &EmbedContext) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(2 + ctx.entities.len());
    if !ctx.title.trim().is_empty() {
        parts.push(ctx.title.trim());
    }
    if !ctx.summary.trim().is_empty() {
        parts.push(ctx.summary.trim());
    }
    for entity in &ctx.entities {
        if !entity.trim().is_empty() {
            parts.push(entity.trim());
        }
    }
    parts.join("\n")
}

/// Run the cluster stage for one article: score it against nearby stories and
/// file it — join, create, defer, or mark as a re-report.
///
/// # Errors
///
/// Propagates transient store/queue failures for retry.
pub fn run_cluster<A: AnalysisStore + ?Sized, T: StoryStore + ?Sized, Q: JobQueue + ?Sized>(
    analysis_store: &A,
    stories: &T,
    queue: &Q,
    article_id: &str,
    config: &StageConfig,
    now: i64,
) -> CoreResult<ClusterReport> {
    let base = ClusterReport {
        missing: false,
        re_embedded: false,
        lease_busy: false,
        decision: None,
        story_id: None,
        candidates: 0,
    };

    // Take the exclusive clustering lease before reading any candidate. Without
    // it, two workers scoring the same event concurrently each see no candidate
    // story and each create one — a split that nothing later merges. Deferring
    // costs a couple of seconds; splitting a story is permanent.
    if !stories.try_acquire_cluster_lease(article_id, now, CLUSTER_LEASE_SECONDS)? {
        queue.enqueue(
            &JobPayload::new(Stage::Cluster, article_id),
            EnqueueOpts {
                priority: CLUSTER_PRIORITY,
                delay: CLUSTER_LEASE_RETRY_SECONDS,
            },
        )?;
        return Ok(ClusterReport {
            lease_busy: true,
            ..base
        });
    }

    let outcome = run_cluster_locked(
        analysis_store,
        stories,
        queue,
        article_id,
        config,
        now,
        &base,
    );
    // Release on every path, including the error path: a failed job that kept
    // the lease would stall every other article until the lease expired.
    stories.release_cluster_lease(article_id)?;
    outcome
}

/// The body of [`run_cluster`], run while holding the clustering lease.
fn run_cluster_locked<A: AnalysisStore + ?Sized, T: StoryStore + ?Sized, Q: JobQueue + ?Sized>(
    analysis_store: &A,
    stories: &T,
    queue: &Q,
    article_id: &str,
    config: &StageConfig,
    now: i64,
    base: &ClusterReport,
) -> CoreResult<ClusterReport> {
    let base = base.clone();
    let Some(ctx) = stories.load_cluster_context(article_id)? else {
        return Ok(ClusterReport {
            missing: true,
            ..base
        });
    };

    let want_recipe = config.vector_recipe();
    let stored = analysis_store.load_vector(article_id)?;
    let Some(stored) = stored.filter(|v| v.recipe == want_recipe) else {
        // No vector, or one built with a recipe that is no longer comparable —
        // the dimension changed under us, or the site switched between the
        // lexical and semantic routes. Rebuild rather than guess.
        queue.enqueue(
            &JobPayload::new(Stage::Embed, article_id),
            EnqueueOpts {
                priority: EMBED_PRIORITY,
                delay: 0,
            },
        )?;
        return Ok(ClusterReport {
            re_embedded: true,
            ..base
        });
    };

    let window_start = crate::cluster::window_start(now, config.cluster.window_seconds);
    let candidate_stories =
        stories.load_candidate_stories(article_id, window_start, MAX_STORY_CANDIDATES)?;
    let candidate_articles =
        stories.load_candidate_articles(article_id, window_start, MAX_ARTICLE_CANDIDATES)?;

    let incoming = IncomingArticle {
        id: article_id.to_string(),
        vector: stored.vector.clone(),
        recipe: stored.recipe,
        topic_id: ctx.topic_id.clone(),
        feed_id: ctx.feed_id.clone(),
        published_at: ctx.published_at,
        relevance_score: ctx.relevance_score,
        waits: ctx.waits,
    };

    let decision = crate::cluster::decide(
        &incoming,
        &candidate_stories,
        &candidate_articles,
        &config.cluster,
    );

    let story_id = apply_cluster_decision(
        stories,
        queue,
        article_id,
        &incoming.vector,
        &incoming.recipe,
        &ctx,
        &decision,
        now,
    )?;

    Ok(ClusterReport {
        decision: Some(decision),
        story_id,
        candidates: candidate_stories.len(),
        ..base
    })
}

/// Persist one clustering decision and enqueue any summarize it implies.
///
/// # Errors
///
/// Propagates transient store/queue failures for retry.
fn apply_cluster_decision<T: StoryStore + ?Sized, Q: JobQueue + ?Sized>(
    stories: &T,
    queue: &Q,
    article_id: &str,
    vector: &[f32],
    recipe: &str,
    ctx: &ClusterContext,
    decision: &ClusterDecision,
    now: i64,
) -> CoreResult<Option<String>> {
    match decision {
        ClusterDecision::Join { story_id, .. } => {
            stories.join_story(story_id, article_id, vector, ctx, now)?;
            enqueue_summarize(queue, story_id)?;
            Ok(Some(story_id.clone()))
        }
        ClusterDecision::Create => {
            let seed = StorySeed {
                // A placeholder until the first summarize names the story. It
                // is deliberately obvious rather than plausible, so an
                // un-summarized story is never mistaken for a summarized one.
                title: "Developing story".to_string(),
                topic_id: ctx.topic_id.clone(),
                centroid: vector.to_vec(),
                // The article's own recipe, verbatim. Deriving it from the
                // vector's *length* worked only by coincidence on the lexical
                // route (where the dimension is the recipe) and produced a
                // `lex-v1/<n>` story centroid for a `sem-v1/<model>` article on
                // the semantic route — a recipe mismatch that silently
                // disqualified every candidate, so every article started its
                // own story.
                recipe: recipe.to_string(),
                published_at: ctx.published_at,
                relevance_score: ctx.relevance_score,
            };
            let story_id = stories.create_story(&seed, article_id, now)?;
            enqueue_summarize(queue, &story_id)?;
            Ok(Some(story_id))
        }
        ClusterDecision::Wait { .. } => {
            stories.record_wait(article_id, now)?;
            Ok(None)
        }
        ClusterDecision::Duplicate {
            of_article_id,
            story_id,
            ..
        } => {
            stories.mark_duplicate(article_id, of_article_id, story_id.as_deref(), now)?;
            // A duplicate changes the story's source list, so the story is
            // worth re-summarizing — the rate limit decides when.
            if let Some(id) = story_id {
                enqueue_summarize(queue, id)?;
            }
            Ok(story_id.clone())
        }
    }
}

/// Enqueue a summarize job for a story.
///
/// De-duplication is the store's `summarize_pending` flag plus the rate limit,
/// not a queue-side check: queue v2 exposes no "is this job already pending"
/// query, so a burst of joins does enqueue several jobs and the first one to
/// run does the work while the rest find the story not yet due and defer.
fn enqueue_summarize<Q: JobQueue + ?Sized>(queue: &Q, story_id: &str) -> CoreResult<()> {
    queue.enqueue(
        &JobPayload::new(Stage::Summarize, story_id),
        EnqueueOpts {
            priority: SUMMARIZE_PRIORITY,
            delay: 0,
        },
    )
}

/// Run the summarize stage for one story: synthesize its members into one
/// narrative and write it onto the story Item.
///
/// # Errors
///
/// Propagates transient provider/store/queue failures for retry. An
/// unparseable response is permanent: the story keeps its previous summary.
pub fn run_summarize<
    P: LlmProvider + ?Sized,
    A: AnalysisStore + ?Sized,
    T: StoryStore + ?Sized,
    Q: JobQueue + ?Sized,
>(
    provider: &P,
    analysis_store: &A,
    stories: &T,
    queue: &Q,
    story_id: &str,
    model: Option<String>,
    config: &StageConfig,
    now: i64,
) -> CoreResult<SummarizeReport> {
    let base = SummarizeReport {
        missing: false,
        paused: false,
        deferred_seconds: 0,
        empty: false,
        unparseable: false,
        members: 0,
        cost_estimate: None,
        change: None,
    };

    let Some(story) = stories.load_story(story_id)? else {
        return Ok(SummarizeReport {
            missing: true,
            ..base
        });
    };

    // Rate limit before the budget check: a deferred story costs nothing and
    // should not be counted against a limit it never touched.
    let wait =
        summarize::wait_seconds(now, story.last_summarized_at, config.summarize_min_interval);
    if wait > 0 {
        queue.enqueue(
            &JobPayload::new(Stage::Summarize, story_id),
            EnqueueOpts {
                priority: SUMMARIZE_PRIORITY,
                delay: wait,
            },
        )?;
        return Ok(SummarizeReport {
            deferred_seconds: wait,
            ..base
        });
    }

    let (paused, _) = budget_gate(
        analysis_store,
        queue,
        Stage::Summarize,
        story_id,
        &config.budget,
        now,
    )?;
    if paused {
        return Ok(SummarizeReport {
            paused: true,
            ..base
        });
    }

    let members = stories.load_story_members(story_id, MAX_STORY_MEMBERS)?;
    if members.is_empty() {
        stories.clear_summarize_pending(story_id)?;
        return Ok(SummarizeReport {
            empty: true,
            ..base
        });
    }

    let (summary, cost): (StorySummary, Option<f64>) =
        match summarize::summarize(provider, model, &members) {
            Ok((s, c)) => (s, c),
            Err(e) if e.is_transient() => return Err(e),
            Err(_) => {
                analysis_store.record_cost(&utc_day(now), Stage::Summarize, None, now)?;
                stories.clear_summarize_pending(story_id)?;
                return Ok(SummarizeReport {
                    unparseable: true,
                    members: members.len(),
                    ..base
                });
            }
        };

    analysis_store.record_cost(&utc_day(now), Stage::Summarize, cost, now)?;

    // Captured before the write: `record_story_summary` overwrites the row and
    // `save-item` replaces the whole Item (G-ITEM-NO-MERGE), so the previous
    // narrative exists nowhere else once the next line runs.
    let change = StoryChange {
        story_id: story_id.to_string(),
        is_new: story.last_summarized_at.is_none(),
        previous_summary: story.summary.clone(),
        title: summary.title.clone(),
        summary: summary.summary.clone(),
        topic_id: story.topic_id.clone(),
        relevance_score: story.relevance_score,
        article_count: members.len(),
    };

    stories.record_story_summary(story_id, &summary, &members, now)?;

    Ok(SummarizeReport {
        members: members.len(),
        cost_estimate: cost,
        change: Some(change),
        ..base
    })
}

// ---------------------------------------------------------------------------
// M4: notification triggers, dispatch and operator alerts
// ---------------------------------------------------------------------------

/// Priority for notify jobs. Above embed and cluster (a notification nobody
/// receives for ten minutes is a notification nobody wanted) and below analyze,
/// because a notification that jumped ahead of the stages producing the *next*
/// notification would be a false economy (`M4-DESIGN.md` Decision 7).
pub const NOTIFY_PRIORITY: i32 = 4;

/// Most events one digest folds. A window that produced more than this is a
/// spike, and the digest says "and N more" rather than growing without bound.
pub const MAX_DIGEST_FOLD: usize = 50;

/// Most failing feeds one alert pass reports on.
pub const MAX_FAILING_FEEDS_PER_PASS: usize = 20;

/// What one notify-stage run did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NotifyReport {
    /// The event row was gone.
    pub missing: bool,
    /// The event had already been sent, suppressed or digested.
    pub already_handled: bool,
    /// The job was re-enqueued this many seconds out (quiet hours, or a
    /// scheduled time not yet reached).
    pub deferred_seconds: i64,
    /// The daily budget is exhausted; the judge call deferred to the next day.
    pub paused: bool,
    /// Why the event was not sent, when it was not.
    pub suppressed: Option<String>,
    /// What the change judge decided, for a story update.
    pub material: Option<bool>,
    /// The judge's justification.
    pub judge_reason: String,
    /// Other events folded into this one as a digest.
    pub digested: usize,
    /// Channels that took it.
    pub delivered: usize,
    /// Channels that failed transiently.
    pub failed: usize,
    /// Channels that refused it permanently.
    pub blocked: usize,
    /// Channels that did not want it.
    pub skipped: usize,
    /// Channel-scoped jobs enqueued (retries plus dispatch overflow).
    pub requeued: usize,
    /// Host-reported cost of the judge call.
    pub cost_estimate: Option<f64>,
}

/// Record the notification a summarize implies, and enqueue its dispatch.
///
/// Returns the new event's id, or `None` when the story does not qualify or the
/// event already exists (an at-least-once redelivery of the summarize job).
///
/// The qualification is the scope's: a high-priority topic notifies whatever the
/// score, and everything else must reach the configured relevance floor. Most
/// stories reach neither, which is the reason a busy pipeline is not a busy
/// phone.
///
/// # Errors
///
/// Propagates transient store/queue failures for retry.
pub fn notify_story_change<N: NotifyStore + ?Sized, Q: JobQueue + ?Sized>(
    notify: &N,
    queue: &Q,
    change: &StoryChange,
    config: &NotifyConfig,
    now: i64,
) -> CoreResult<Option<String>> {
    let topic_priority = match &change.topic_id {
        Some(topic_id) if !topic_id.is_empty() => notify.topic_priority(topic_id)?,
        _ => NotifyPriority::Normal,
    };
    let Some(priority) = ratelimit::story_priority(topic_priority, change.relevance_score, config)
    else {
        return Ok(None);
    };

    let kind = if change.is_new {
        EventKind::StoryNew
    } else {
        EventKind::StoryUpdated
    };

    // Deterministic in the story and its member count, never in the model's
    // wording: a redelivered summarize job re-synthesizes the same members and
    // must land on the same key even though the prose may differ.
    let dedup_key = if change.is_new {
        change.story_id.clone()
    } else {
        format!("{}:{}", change.story_id, change.article_count)
    };

    let event = NewEvent {
        kind,
        priority,
        subject_id: Some(change.story_id.clone()),
        dedup_key,
        title: change.title.clone(),
        body: change.summary.clone(),
        link: None,
        data: serde_json::json!({
            notify::DATA_SUMMARY: change.summary,
            notify::DATA_PREVIOUS_SUMMARY: change.previous_summary,
            "story_id": change.story_id,
            "article_count": change.article_count,
            "relevance_score": change.relevance_score,
            "topic_id": change.topic_id,
        }),
    };

    let Some(event_id) = notify.record_event(&event, now)? else {
        return Ok(None);
    };
    queue.enqueue(
        &JobPayload::new(Stage::Notify, event_id.clone()),
        EnqueueOpts {
            priority: NOTIFY_PRIORITY,
            delay: 0,
        },
    )?;
    Ok(Some(event_id))
}

/// Deliver one event to one channel, recording the outcome and scheduling a
/// retry when the failure was transient and attempts remain.
///
/// Shared by the whole-event dispatch and by the channel-scoped retry job, so
/// the two cannot drift apart on what counts as retryable.
#[allow(clippy::too_many_arguments)]
// Three ports, the event, the channel, the rendered notification, the
// configuration, the clock and the report to accumulate into. Every one of them
// is genuinely needed here and bundling them would only move the argument list
// into a struct definition.
fn deliver_and_record<T: Transport + ?Sized, N: NotifyStore + ?Sized, Q: JobQueue + ?Sized>(
    transport: &T,
    notify: &N,
    queue: &Q,
    event_id: &str,
    channel: &ChannelConfig,
    notification: &Notification,
    config: &NotifyConfig,
    now: i64,
    report: &mut NotifyReport,
) -> CoreResult<()> {
    let outcome = notify::deliver(transport, channel, notification);
    let attempts = notify.record_delivery(event_id, &outcome, now)?;

    match outcome.state {
        DeliveryState::Delivered => {
            report.delivered += 1;
            notify.note_channel_health(&channel.id, true, None, now)?;
        }
        DeliveryState::Skipped => report.skipped += 1,
        DeliveryState::Blocked => {
            report.blocked += 1;
            notify.note_channel_health(&channel.id, false, outcome.error.as_deref(), now)?;
        }
        DeliveryState::Failed => {
            report.failed += 1;
            notify.note_channel_health(&channel.id, false, outcome.error.as_deref(), now)?;
            // A WASM plugin cannot sleep, so backoff is the queue's `delay` on a
            // channel-scoped re-enqueue (`M4-DESIGN.md` Decision 4).
            if ratelimit::may_retry(attempts, config) {
                queue.enqueue(
                    &JobPayload::for_channel(event_id, &channel.id),
                    EnqueueOpts {
                        priority: NOTIFY_PRIORITY,
                        delay: ratelimit::retry_delay(attempts.saturating_sub(1), config),
                    },
                )?;
                report.requeued += 1;
            }
        }
    }
    Ok(())
}

/// Run the notify stage for one outbox event.
///
/// With `channel` set the job is narrowed to one channel — a retry, or a channel
/// that overflowed [`ratelimit::MAX_CHANNELS_PER_DISPATCH`] — and every gate
/// below is skipped, because the decision to send was already made for the whole
/// event and re-litigating it would drop the retry.
///
/// Makes **at most one** AI call (the story-update judge), holding the
/// one-call-per-job rule.
///
/// # Errors
///
/// Propagates transient provider/store/queue failures for retry. Every
/// per-channel failure is recorded rather than propagated: one channel must
/// never take down another, and a worker that panicked would re-send to the
/// channels that had already succeeded.
#[allow(clippy::too_many_arguments)]
// Six ports and two configurations. Collapsing them into a context struct would
// hide which stage touches which port, which is the property the port split
// exists to make visible; every other stage in this module takes them the same
// way.
pub fn run_notify<
    T: Transport + ?Sized,
    N: NotifyStore + ?Sized,
    A: AnalysisStore + ?Sized,
    P: LlmProvider + ?Sized,
    Q: JobQueue + ?Sized,
>(
    transport: &T,
    notify: &N,
    analysis_store: &A,
    provider: &P,
    queue: &Q,
    event_id: &str,
    channel_id: Option<&str>,
    model: Option<String>,
    config: &NotifyConfig,
    budget: &BudgetConfig,
    now: i64,
) -> CoreResult<NotifyReport> {
    let mut report = NotifyReport::default();

    let Some(event) = notify.load_event(event_id)? else {
        report.missing = true;
        return Ok(report);
    };

    // ---- the channel-scoped path: a retry or an overflow ------------------
    if let Some(channel_id) = channel_id {
        let Some(channel) = notify.load_channel(channel_id)? else {
            // Disabled or deleted between enqueue and delivery. Not an error:
            // an operator turning a channel off is entitled to have it stop.
            report.suppressed = Some(format!("channel {channel_id} is no longer enabled"));
            return Ok(report);
        };
        deliver_and_record(
            transport,
            notify,
            queue,
            event_id,
            &channel,
            &event.notification(),
            config,
            now,
            &mut report,
        )?;
        return Ok(report);
    }

    // ---- gates ------------------------------------------------------------
    if event.state != EventState::Pending {
        report.already_handled = true;
        return Ok(report);
    }

    if event.scheduled_at > now {
        let delay = event.scheduled_at - now;
        queue.enqueue(
            &JobPayload::new(Stage::Notify, event_id),
            EnqueueOpts {
                priority: NOTIFY_PRIORITY,
                delay,
            },
        )?;
        report.deferred_seconds = delay;
        return Ok(report);
    }

    // ---- did anything actually change? ------------------------------------
    if event.kind == EventKind::StoryUpdated {
        let previous = event.data_str(notify::DATA_PREVIOUS_SUMMARY).to_string();
        let current = event.data_str(notify::DATA_SUMMARY).to_string();

        let verdict = if config.judge_enabled && !previous.is_empty() {
            let (paused, _) =
                budget_gate(analysis_store, queue, Stage::Notify, event_id, budget, now)?;
            if paused {
                report.paused = true;
                return Ok(report);
            }
            match judge::judge(provider, model, &previous, &current) {
                Ok((verdict, cost)) => {
                    analysis_store.record_cost(&utc_day(now), Stage::Notify, cost, now)?;
                    report.cost_estimate = cost;
                    verdict
                }
                Err(e) if e.is_transient() => return Err(e),
                Err(_) => {
                    // The call was made and priced, so it is counted; the answer
                    // was unusable, so the deterministic fallback decides rather
                    // than the story going unnotified on a parse failure.
                    analysis_store.record_cost(&utc_day(now), Stage::Notify, None, now)?;
                    judge::verdict_without_judge(&previous, &current, config.change_ratio)
                }
            }
        } else {
            judge::verdict_without_judge(&previous, &current, config.change_ratio)
        };

        report.material = Some(verdict.material);
        report.judge_reason.clone_from(&verdict.reason);
        if !verdict.material {
            let reason = format!("no material change: {}", verdict.reason);
            notify.set_event_state(event_id, EventState::Suppressed, Some(&reason), now)?;
            report.suppressed = Some(reason);
            return Ok(report);
        }
    }

    // ---- rate limiting ----------------------------------------------------
    let last_sent = match &event.subject_id {
        Some(subject) => notify.last_sent_at(subject)?,
        None => None,
    };
    match ratelimit::verdict(now, event.kind, event.priority, last_sent, config) {
        SendVerdict::Suppress { reason } => {
            notify.set_event_state(event_id, EventState::Suppressed, Some(reason), now)?;
            report.suppressed = Some(reason.to_string());
            return Ok(report);
        }
        SendVerdict::Defer { until, reason } => {
            notify.reschedule_event(event_id, until)?;
            let delay = (until - now).max(1);
            queue.enqueue(
                &JobPayload::new(Stage::Notify, event_id),
                EnqueueOpts {
                    priority: NOTIFY_PRIORITY,
                    delay,
                },
            )?;
            report.deferred_seconds = delay;
            report.suppressed = Some(reason.to_string());
            return Ok(report);
        }
        SendVerdict::Send => {}
    }

    // ---- digest collapse ---------------------------------------------------
    let mut notification = event.notification();
    if event.kind.is_digestible() && event.priority == NotifyPriority::Normal && config.has_digest()
    {
        let window_start = ratelimit::digest_window_start(now, config);
        // Counted before claiming: claiming is destructive, and claiming four
        // events that never reach the threshold would swallow four
        // notifications.
        let pending = notify.pending_digestible(event_id, window_start, now)?;
        if ratelimit::should_digest(pending + 1, config) {
            let folded = notify.claim_digest(event_id, window_start, MAX_DIGEST_FOLD, now)?;
            if !folded.is_empty() {
                notification = notify::digest_notification(&event, &folded, now);
                notify.promote_to_digest(event_id, &notification, folded.len(), now)?;
                report.digested = folded.len();
            }
        }
    }

    // ---- dispatch ----------------------------------------------------------
    let channels = notify.load_channels()?;
    let split = channels.len().min(ratelimit::MAX_CHANNELS_PER_DISPATCH);
    for channel in &channels[..split] {
        deliver_and_record(
            transport,
            notify,
            queue,
            event_id,
            channel,
            &notification,
            config,
            now,
            &mut report,
        )?;
    }
    // Each channel is one outbound POST with its own transfer budget, so a site
    // with more channels than one job may hold hands the rest to their own jobs
    // rather than risking the background epoch.
    for channel in &channels[split..] {
        queue.enqueue(
            &JobPayload::for_channel(event_id, &channel.id),
            EnqueueOpts {
                priority: NOTIFY_PRIORITY,
                delay: 0,
            },
        )?;
        report.requeued += 1;
    }

    // Marked sent whatever the channels made of it: the event was dispatched,
    // and the per-channel rows are where "did it arrive" is answered. Marking it
    // sent is also what stops a redelivered job from dispatching twice.
    notify.set_event_state(event_id, EventState::Sent, None, now)?;
    Ok(report)
}

/// What one alert pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AlertReport {
    /// Feeds alerted on.
    pub feeds_failing: usize,
    /// `true` when a budget alert was recorded this pass.
    pub budget_alerted: bool,
    /// `true` when a queue-stuck alert was recorded this pass.
    pub queue_alerted: bool,
    /// Alert events actually recorded (the rest were already known).
    pub events_recorded: usize,
}

/// Record the operator alerts the current state justifies, and enqueue them.
///
/// Driven from `tap_cron` beside [`run_maintenance`], because `tap_cron` is the
/// only cadence a plugin gets and a plugin with several periodic duties
/// multiplexes them there.
///
/// Every alert's dedup key is deliberately coarse — a feed plus its failure
/// count, a day plus a verdict, an hour — so a condition that persists for six
/// hours produces a handful of rows rather than one per cron cycle. The debounce
/// in [`ratelimit::verdict`] then thins what is left.
///
/// # Errors
///
/// Propagates transient store/queue failures. The caller is `tap_cron` and must
/// not panic, so it logs rather than retries.
pub fn run_alerts<N: NotifyStore + ?Sized, Q: JobQueue + ?Sized>(
    notify: &N,
    queue: &Q,
    spend: &DailySpend,
    budget: &BudgetConfig,
    config: &NotifyConfig,
    now: i64,
) -> CoreResult<AlertReport> {
    let mut report = AlertReport::default();
    if !config.alerts_enabled {
        return Ok(report);
    }

    let record = |event: NewEvent| -> CoreResult<bool> {
        let Some(event_id) = notify.record_event(&event, now)? else {
            return Ok(false);
        };
        queue.enqueue(
            &JobPayload::new(Stage::Notify, event_id),
            EnqueueOpts {
                priority: NOTIFY_PRIORITY,
                delay: 0,
            },
        )?;
        Ok(true)
    };

    // ---- feeds that have stopped working ----------------------------------
    if config.feed_failure_threshold > 0 {
        let failing =
            notify.failing_feeds(config.feed_failure_threshold, MAX_FAILING_FEEDS_PER_PASS)?;
        report.feeds_failing = failing.len();
        for feed in failing {
            let recorded = record(NewEvent {
                kind: EventKind::FeedFailing,
                priority: NotifyPriority::Normal,
                subject_id: Some(feed.id.clone()),
                // Keyed on the count, so the next failure is a new alert and the
                // same failure is not.
                dedup_key: format!("{}:{}", feed.id, feed.failure_count),
                title: format!("Feed failing: {}", feed.name),
                body: format!(
                    "{} has failed {} consecutive fetches. Last error: {}",
                    feed.name, feed.failure_count, feed.last_error
                ),
                link: None,
                data: serde_json::json!({
                    "feed_id": feed.id,
                    "feed_name": feed.name,
                    "failure_count": feed.failure_count,
                    "last_error": feed.last_error,
                }),
            })?;
            if recorded {
                report.events_recorded += 1;
            }
        }
    }

    // ---- the day's spend ---------------------------------------------------
    let day = utc_day(now);
    let verdict = crate::budget::verdict(spend.spent_usd, budget);
    if verdict != BudgetVerdict::Ok {
        let (label, body) = match verdict {
            BudgetVerdict::Pause => (
                "reached",
                format!(
                    "Argus has spent ${:.4} of its ${:.4} daily limit on {day}. \
                     Analyze and summarize are paused until tomorrow.",
                    spend.spent_usd, budget.daily_limit_usd
                ),
            ),
            _ => (
                "past the alert threshold",
                format!(
                    "Argus has spent ${:.4} on {day}, past its ${:.4} alert threshold.",
                    spend.spent_usd, budget.alert_threshold_usd
                ),
            ),
        };
        // A budget alert is loud: it is the one condition that stops the
        // pipeline spending, and an operator who finds out tomorrow finds out
        // too late.
        let priority = if verdict == BudgetVerdict::Pause {
            NotifyPriority::High
        } else {
            NotifyPriority::Normal
        };
        report.budget_alerted = true;
        if record(NewEvent {
            kind: EventKind::BudgetThreshold,
            priority,
            subject_id: None,
            // At most one warning and one pause alert per UTC day.
            dedup_key: format!("{day}:{label}"),
            title: format!("Argus daily AI budget {label}"),
            body,
            link: None,
            data: serde_json::json!({
                "day": day,
                "spent_usd": spend.spent_usd,
                "calls": spend.calls,
                "unpriced_calls": spend.unpriced_calls,
                "daily_limit_usd": budget.daily_limit_usd,
                "alert_threshold_usd": budget.alert_threshold_usd,
                "verdict": if verdict == BudgetVerdict::Pause { "pause" } else { "warn" },
            }),
        })? {
            report.events_recorded += 1;
        }
    }

    // ---- the queue ---------------------------------------------------------
    if config.queue_stuck_seconds > 0 {
        let health = notify.queue_health(now)?;
        if health.oldest_ready_age > config.queue_stuck_seconds {
            report.queue_alerted = true;
            if record(NewEvent {
                kind: EventKind::QueueStuck,
                priority: NotifyPriority::High,
                subject_id: None,
                // One per hour at most, whatever the cron cadence.
                dedup_key: format!("{}", now / 3_600),
                title: "Argus queue is not draining".into(),
                body: format!(
                    "The oldest eligible Argus job has been waiting {}s ({} ready, {} dead-lettered). \
                     Check that the cron endpoint is being called.",
                    health.oldest_ready_age, health.ready, health.dead
                ),
                link: None,
                data: serde_json::json!({
                    "oldest_ready_age": health.oldest_ready_age,
                    "ready": health.ready,
                    "dead": health.dead,
                    "threshold_seconds": config.queue_stuck_seconds,
                }),
            })? {
                report.events_recorded += 1;
            }
        }
    }

    Ok(report)
}

/// Run one maintenance pass: retire idle stories, reclaim old article bodies,
/// and re-enqueue articles clustering deferred.
///
/// Driven from `tap_cron`, which is the only cadence a plugin gets: `tap_cron`
/// fires every cycle with a timestamp and no cron key, so a plugin with several
/// periodic duties multiplexes them here.
///
/// # Errors
///
/// Propagates transient store/queue failures. Unlike a queue worker, the caller
/// is `tap_cron` and must not panic, so it logs rather than retries.
pub fn run_maintenance<A: AnalysisStore + ?Sized, T: StoryStore + ?Sized, Q: JobQueue + ?Sized>(
    analysis_store: &A,
    stories: &T,
    queue: &Q,
    config: &StageConfig,
    now: i64,
) -> CoreResult<MaintenanceReport> {
    let stories_retired =
        stories.deactivate_stale_stories(now - config.cluster.inactive_seconds, now)?;

    let retention_cutoff = now - config.article_retention_days.max(0) * 86_400;
    let articles_purged =
        analysis_store.purge_article_content(retention_cutoff, now, MAX_PURGE_PER_PASS)?;

    // A deferred article is reconsidered once its neighbourhood has had time to
    // fill in; a second deferral is impossible because the decision is forced
    // to create past `max_waits`.
    let waiting_cutoff = now - config.cluster.inactive_seconds;
    let waiting = stories.load_waiting_articles(waiting_cutoff, MAX_WAITING_PER_PASS)?;
    for article_id in &waiting {
        queue.enqueue(
            &JobPayload::new(Stage::Cluster, article_id.clone()),
            EnqueueOpts {
                priority: CLUSTER_PRIORITY,
                delay: 0,
            },
        )?;
    }

    Ok(MaintenanceReport {
        stories_retired,
        articles_purged,
        waiting_requeued: waiting.len(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::entity::EntityAction;
    use crate::error::CoreError;
    use crate::ports::{
        ChatRequest, ChatResponse, DecideContext, EmbedResponse, Feed, FeedSchedule, UpsertResult,
        Usage,
    };
    use crate::provider::MockProvider;
    use std::cell::RefCell;
    use std::collections::HashMap;

    // ---- in-memory fakes -------------------------------------------------

    #[derive(Default)]
    struct FakeStore {
        feeds: RefCell<HashMap<String, Feed>>,
        articles: RefCell<HashMap<String, StoredArticle>>, // keyed by url
        by_id: RefCell<HashMap<String, String>>,           // id -> url
        next_id: RefCell<u64>,
        feed_success: RefCell<u32>,
        feed_failure: RefCell<u32>,
        cursor: RefCell<u64>,
        // ---- M2 ----
        analyses: RefCell<HashMap<String, (Analysis, String)>>, // article id -> (analysis, raw)
        vectors: RefCell<HashMap<String, crate::ports::StoredVector>>,
        entities: RefCell<Vec<crate::entity::EntityRecord>>,
        aliases: RefCell<HashMap<String, Vec<String>>>, // entity id -> aliases
        links: RefCell<Vec<(String, String)>>,          // (article id, entity id)
        spend: RefCell<HashMap<String, DailySpend>>,    // utc day -> spend
        stories: RefCell<HashMap<String, FakeStory>>,
        cluster_lease: RefCell<Option<(String, i64)>>, // (holder token, expiry)
        next_story: RefCell<u64>,
        purged: RefCell<u64>,
        retired: RefCell<u64>,
    }

    #[derive(Clone)]
    struct StoredArticle {
        id: String,
        ctx: DecideContext,
        state: PipelineState,
        score: Option<u8>,
        reason: String,
        // ---- M2 ----
        topic_id: Option<String>,
        feed_id: Option<String>,
        published_at: Option<i64>,
        story_id: Option<String>,
        waits: u32,
        is_duplicate: bool,
        duplicate_of: Option<String>,
        error: Option<String>,
        content_purged: bool,
    }

    #[derive(Clone)]
    struct FakeStory {
        id: String,
        centroid: Vec<f32>,
        recipe: String,
        topic_id: Option<String>,
        article_count: u32,
        first_article_at: Option<i64>,
        last_article_at: Option<i64>,
        is_active: bool,
        relevance_score: Option<i32>,
        summarize_pending: bool,
        last_summarized_at: Option<i64>,
        item_title: String,
        item_summary: String,
        item_sources: Vec<String>,
    }

    impl Store for FakeStore {
        fn load_feed(&self, feed_id: &str) -> CoreResult<Option<Feed>> {
            Ok(self.feeds.borrow().get(feed_id).cloned())
        }
        fn load_enabled_feeds(&self) -> CoreResult<Vec<FeedSchedule>> {
            Ok(Vec::new())
        }
        fn upsert_article(&self, a: &NewArticle) -> CoreResult<UpsertResult> {
            let mut arts = self.articles.borrow_mut();
            if let Some(existing) = arts.get(&a.url) {
                // Re-seen URL: update nothing, report not-inserted.
                return Ok(UpsertResult {
                    id: existing.id.clone(),
                    inserted: false,
                });
            }
            let mut next = self.next_id.borrow_mut();
            *next += 1;
            let id = format!("art-{next}");
            arts.insert(
                a.url.clone(),
                StoredArticle {
                    id: id.clone(),
                    ctx: DecideContext {
                        title: a.title.clone(),
                        content: a.content.clone(),
                        topic_prompt: "TOPIC".to_string(),
                        threshold: 50,
                    },
                    state: PipelineState::Fetched,
                    score: None,
                    reason: String::new(),
                    topic_id: Some(a.topic_id.clone()),
                    feed_id: Some(a.feed_id.clone()),
                    published_at: a.published_at,
                    story_id: None,
                    waits: 0,
                    is_duplicate: false,
                    duplicate_of: None,
                    error: None,
                    content_purged: false,
                },
            );
            self.by_id.borrow_mut().insert(id.clone(), a.url.clone());
            Ok(UpsertResult { id, inserted: true })
        }
        fn record_feed_success(
            &self,
            _f: &str,
            _c: &ConditionalHeaders,
            _n: i64,
        ) -> CoreResult<()> {
            *self.feed_success.borrow_mut() += 1;
            Ok(())
        }
        fn record_feed_failure(&self, _f: &str, _e: &str, _n: i64) -> CoreResult<()> {
            *self.feed_failure.borrow_mut() += 1;
            Ok(())
        }
        fn load_decide_context(&self, id: &str) -> CoreResult<Option<DecideContext>> {
            let url = self.by_id.borrow().get(id).cloned();
            Ok(url.and_then(|u| self.articles.borrow().get(&u).map(|a| a.ctx.clone())))
        }
        fn record_decision(&self, id: &str, score: u8, reason: &str, keep: bool) -> CoreResult<()> {
            let url = self
                .by_id
                .borrow()
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
            let mut arts = self.articles.borrow_mut();
            let a = arts.get_mut(&url).unwrap();
            a.score = Some(score);
            a.reason = reason.to_string();
            a.state = if keep {
                PipelineState::Decided
            } else {
                PipelineState::Discarded
            };
            Ok(())
        }
        fn set_state(&self, id: &str, state: PipelineState) -> CoreResult<()> {
            let url = self
                .by_id
                .borrow()
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
            self.articles.borrow_mut().get_mut(&url).unwrap().state = state;
            Ok(())
        }
        fn record_article_error(&self, id: &str, e: &str) -> CoreResult<()> {
            self.with_article(id, |a| a.error = Some(e.to_string()))?;
            self.set_state(id, PipelineState::Error)
        }
        fn load_cursor(&self) -> CoreResult<u64> {
            Ok(*self.cursor.borrow())
        }
        fn save_cursor(&self, cursor: u64) -> CoreResult<()> {
            *self.cursor.borrow_mut() = cursor;
            Ok(())
        }
    }

    impl FakeStore {
        /// Mutate one article by id, or fail the way the real store does when
        /// the row is gone.
        fn with_article<F: FnOnce(&mut StoredArticle)>(&self, id: &str, f: F) -> CoreResult<()> {
            let url = self
                .by_id
                .borrow()
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
            let mut arts = self.articles.borrow_mut();
            let a = arts
                .get_mut(&url)
                .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
            f(a);
            Ok(())
        }

        fn article(&self, id: &str) -> Option<StoredArticle> {
            let url = self.by_id.borrow().get(id).cloned()?;
            self.articles.borrow().get(&url).cloned()
        }

        fn story(&self, id: &str) -> Option<FakeStory> {
            self.stories.borrow().get(id).cloned()
        }

        fn story_count(&self) -> usize {
            self.stories.borrow().len()
        }

        fn entity_names(&self) -> Vec<String> {
            let mut names: Vec<String> = self
                .entities
                .borrow()
                .iter()
                .map(|e| e.canonical_name.clone())
                .collect();
            names.sort();
            names
        }

        fn with_feed(self, id: &str, url: &str) -> Self {
            self.feeds.borrow_mut().insert(
                id.to_string(),
                Feed {
                    id: id.to_string(),
                    url: url.to_string(),
                    topic_id: "topic-1".to_string(),
                    conditional: ConditionalHeaders::default(),
                },
            );
            self
        }
        fn state_of(&self, id: &str) -> Option<PipelineState> {
            let url = self.by_id.borrow().get(id).cloned()?;
            self.articles.borrow().get(&url).map(|a| a.state)
        }
        fn score_of(&self, id: &str) -> Option<u8> {
            let url = self.by_id.borrow().get(id).cloned()?;
            self.articles.borrow().get(&url).and_then(|a| a.score)
        }
        fn article_count(&self) -> usize {
            self.articles.borrow().len()
        }
    }

    // ---- M2 store implementations ---------------------------------------

    impl AnalysisStore for FakeStore {
        fn load_analyze_context(
            &self,
            id: &str,
        ) -> CoreResult<Option<crate::ports::AnalyzeContext>> {
            Ok(self.article(id).map(|a| crate::ports::AnalyzeContext {
                title: a.ctx.title,
                content: a.ctx.content,
            }))
        }

        fn record_analysis(
            &self,
            id: &str,
            analysis: &Analysis,
            raw: &str,
            _now: i64,
        ) -> CoreResult<()> {
            self.analyses
                .borrow_mut()
                .insert(id.to_string(), (analysis.clone(), raw.to_string()));
            self.with_article(id, |a| a.state = PipelineState::Analyzed)
        }

        fn load_entity_candidates(
            &self,
            prefixes: &[String],
            types: &[crate::entity::EntityType],
        ) -> CoreResult<Vec<crate::entity::EntityRecord>> {
            Ok(self
                .entities
                .borrow()
                .iter()
                .filter(|e| types.contains(&e.entity_type))
                .filter(|e| prefixes.iter().any(|p| e.match_key.starts_with(p.as_str())))
                .cloned()
                .collect())
        }

        fn apply_entity_plan(
            &self,
            article_id: &str,
            plan: &crate::entity::EntityPlan,
            _now: i64,
        ) -> CoreResult<EntityApplyReport> {
            let mut report = EntityApplyReport::default();
            for action in plan {
                let entity_id = match action {
                    EntityAction::Link {
                        entity_id,
                        new_alias,
                    } => {
                        report.linked += 1;
                        if let Some(alias) = new_alias {
                            let mut aliases = self.aliases.borrow_mut();
                            let list = aliases.entry(entity_id.clone()).or_default();
                            if !list.contains(alias) {
                                list.push(alias.clone());
                                report.aliases_added += 1;
                            }
                        }
                        entity_id.clone()
                    }
                    EntityAction::Create {
                        canonical_name,
                        match_key,
                        entity_type,
                    } => {
                        report.created += 1;
                        let id = format!("ent-{}", self.entities.borrow().len() + 1);
                        self.entities
                            .borrow_mut()
                            .push(crate::entity::EntityRecord {
                                id: id.clone(),
                                canonical_name: canonical_name.clone(),
                                match_key: match_key.clone(),
                                entity_type: *entity_type,
                            });
                        id
                    }
                };
                let link = (article_id.to_string(), entity_id);
                if !self.links.borrow().contains(&link) {
                    self.links.borrow_mut().push(link);
                }
            }
            Ok(report)
        }

        fn load_embed_context(&self, id: &str) -> CoreResult<Option<crate::ports::EmbedContext>> {
            let Some(article) = self.article(id) else {
                return Ok(None);
            };
            let summary = self
                .analyses
                .borrow()
                .get(id)
                .map(|(a, _)| a.summary.clone())
                .unwrap_or_default();
            let entity_ids: Vec<String> = self
                .links
                .borrow()
                .iter()
                .filter(|(aid, _)| aid == id)
                .map(|(_, eid)| eid.clone())
                .collect();
            let entities = self
                .entities
                .borrow()
                .iter()
                .filter(|e| entity_ids.contains(&e.id))
                .map(|e| e.canonical_name.clone())
                .collect();
            Ok(Some(crate::ports::EmbedContext {
                title: article.ctx.title,
                summary: if summary.is_empty() {
                    article.ctx.content
                } else {
                    summary
                },
                entities,
            }))
        }

        fn save_vector(&self, id: &str, vector: &[f32], recipe: &str, _now: i64) -> CoreResult<()> {
            self.vectors.borrow_mut().insert(
                id.to_string(),
                crate::ports::StoredVector {
                    vector: vector.to_vec(),
                    recipe: recipe.to_string(),
                },
            );
            Ok(())
        }

        fn load_vector(&self, id: &str) -> CoreResult<Option<crate::ports::StoredVector>> {
            Ok(self.vectors.borrow().get(id).cloned())
        }

        fn record_cost(
            &self,
            day: &str,
            _stage: Stage,
            cost: Option<f64>,
            _now: i64,
        ) -> CoreResult<()> {
            let mut spend = self.spend.borrow_mut();
            let entry = spend.entry(day.to_string()).or_default();
            entry.calls += 1;
            match cost {
                Some(c) => entry.spent_usd += c,
                None => entry.unpriced_calls += 1,
            }
            Ok(())
        }

        fn load_daily_spend(&self, day: &str) -> CoreResult<DailySpend> {
            Ok(self.spend.borrow().get(day).copied().unwrap_or_default())
        }

        fn purge_article_content(&self, cutoff: i64, _now: i64, limit: usize) -> CoreResult<u64> {
            let mut purged = 0u64;
            for article in self.articles.borrow_mut().values_mut() {
                if purged as usize >= limit {
                    break;
                }
                if article.state.is_terminal()
                    && !article.content_purged
                    && article.published_at.is_some_and(|p| p < cutoff)
                {
                    article.ctx.content = String::new();
                    article.content_purged = true;
                    purged += 1;
                }
            }
            *self.purged.borrow_mut() += purged;
            Ok(purged)
        }
    }

    impl StoryStore for FakeStore {
        fn try_acquire_cluster_lease(
            &self,
            token: &str,
            now: i64,
            lease_seconds: i64,
        ) -> CoreResult<bool> {
            let mut lease = self.cluster_lease.borrow_mut();
            match lease.as_ref() {
                Some((holder, expiry)) if holder != token && *expiry > now => Ok(false),
                _ => {
                    *lease = Some((token.to_string(), now + lease_seconds));
                    Ok(true)
                }
            }
        }

        fn release_cluster_lease(&self, token: &str) -> CoreResult<()> {
            let mut lease = self.cluster_lease.borrow_mut();
            if lease.as_ref().is_some_and(|(holder, _)| holder == token) {
                *lease = None;
            }
            Ok(())
        }

        fn load_cluster_context(&self, id: &str) -> CoreResult<Option<ClusterContext>> {
            Ok(self.article(id).map(|a| ClusterContext {
                topic_id: a.topic_id,
                feed_id: a.feed_id,
                published_at: a.published_at,
                relevance_score: a.score.map(i32::from),
                waits: a.waits,
            }))
        }

        fn load_candidate_stories(
            &self,
            _article_id: &str,
            window_start: i64,
            limit: usize,
        ) -> CoreResult<Vec<crate::cluster::CandidateStory>> {
            let mut out: Vec<_> = self
                .stories
                .borrow()
                .values()
                .filter(|s| s.is_active)
                .filter(|s| s.last_article_at.is_none_or(|t| t >= window_start))
                .map(|s| crate::cluster::CandidateStory {
                    id: s.id.clone(),
                    centroid: s.centroid.clone(),
                    recipe: s.recipe.clone(),
                    topic_id: s.topic_id.clone(),
                    last_article_at: s.last_article_at,
                    article_count: s.article_count,
                })
                .collect();
            out.sort_by_key(|s| s.id.clone());
            out.truncate(limit);
            Ok(out)
        }

        fn load_candidate_articles(
            &self,
            article_id: &str,
            window_start: i64,
            limit: usize,
        ) -> CoreResult<Vec<crate::cluster::CandidateArticle>> {
            let vectors = self.vectors.borrow();
            let mut out: Vec<_> = self
                .articles
                .borrow()
                .values()
                .filter(|a| a.id != article_id && !a.is_duplicate)
                .filter(|a| a.published_at.is_none_or(|t| t >= window_start))
                .filter_map(|a| {
                    vectors
                        .get(&a.id)
                        .map(|v| crate::cluster::CandidateArticle {
                            id: a.id.clone(),
                            vector: v.vector.clone(),
                            recipe: v.recipe.clone(),
                            feed_id: a.feed_id.clone(),
                            story_id: a.story_id.clone(),
                            published_at: a.published_at,
                        })
                })
                .collect();
            out.sort_by_key(|a| a.id.clone());
            out.truncate(limit);
            Ok(out)
        }

        fn create_story(&self, seed: &StorySeed, article_id: &str, now: i64) -> CoreResult<String> {
            let mut next = self.next_story.borrow_mut();
            *next += 1;
            let id = format!("story-{next}");
            drop(next);
            self.stories.borrow_mut().insert(
                id.clone(),
                FakeStory {
                    id: id.clone(),
                    centroid: seed.centroid.clone(),
                    recipe: seed.recipe.clone(),
                    topic_id: seed.topic_id.clone(),
                    article_count: 1,
                    first_article_at: seed.published_at,
                    last_article_at: seed.published_at,
                    is_active: true,
                    relevance_score: seed.relevance_score,
                    summarize_pending: true,
                    last_summarized_at: None,
                    item_title: seed.title.clone(),
                    item_summary: String::new(),
                    item_sources: Vec::new(),
                },
            );
            let sid = id.clone();
            self.with_article(article_id, |a| {
                a.story_id = Some(sid.clone());
                a.state = PipelineState::Complete;
            })?;
            let _ = now;
            Ok(id)
        }

        fn join_story(
            &self,
            story_id: &str,
            article_id: &str,
            vector: &[f32],
            ctx: &ClusterContext,
            _now: i64,
        ) -> CoreResult<()> {
            {
                let mut stories = self.stories.borrow_mut();
                let story = stories
                    .get_mut(story_id)
                    .ok_or_else(|| CoreError::NotFound(story_id.to_string()))?;
                story.centroid =
                    crate::embed::fold_centroid(&story.centroid, story.article_count, vector);
                story.article_count += 1;
                story.summarize_pending = true;
                if let Some(at) = ctx.published_at {
                    story.last_article_at =
                        Some(story.last_article_at.map_or(at, |cur| cur.max(at)));
                    story.first_article_at =
                        Some(story.first_article_at.map_or(at, |cur| cur.min(at)));
                }
                if let Some(score) = ctx.relevance_score {
                    story.relevance_score =
                        Some(story.relevance_score.map_or(score, |cur| cur.max(score)));
                }
            }
            let sid = story_id.to_string();
            self.with_article(article_id, |a| {
                a.story_id = Some(sid.clone());
                a.state = PipelineState::Complete;
            })
        }

        fn mark_duplicate(
            &self,
            article_id: &str,
            of_article_id: &str,
            story_id: Option<&str>,
            _now: i64,
        ) -> CoreResult<()> {
            let of = of_article_id.to_string();
            let sid = story_id.map(str::to_string);
            self.with_article(article_id, |a| {
                a.is_duplicate = true;
                a.duplicate_of = Some(of.clone());
                a.story_id = sid.clone();
                a.state = PipelineState::Complete;
            })?;
            if let Some(sid) = story_id
                && let Some(story) = self.stories.borrow_mut().get_mut(sid)
            {
                // A duplicate joins the source list without being counted.
                story.summarize_pending = true;
            }
            Ok(())
        }

        fn record_wait(&self, article_id: &str, _now: i64) -> CoreResult<()> {
            self.with_article(article_id, |a| {
                a.waits += 1;
                a.state = PipelineState::Waiting;
            })
        }

        fn load_story(&self, story_id: &str) -> CoreResult<Option<crate::ports::StoryRow>> {
            Ok(self
                .stories
                .borrow()
                .get(story_id)
                .map(|s| crate::ports::StoryRow {
                    id: s.id.clone(),
                    centroid: s.centroid.clone(),
                    recipe: s.recipe.clone(),
                    article_count: s.article_count,
                    last_article_at: s.last_article_at,
                    last_summarized_at: s.last_summarized_at,
                    title: s.item_title.clone(),
                    summary: s.item_summary.clone(),
                    topic_id: s.topic_id.clone(),
                    relevance_score: s.relevance_score,
                }))
        }

        fn load_story_members(
            &self,
            story_id: &str,
            limit: usize,
        ) -> CoreResult<Vec<crate::summarize::StoryMember>> {
            let analyses = self.analyses.borrow();
            let mut out: Vec<_> = self
                .articles
                .borrow()
                .values()
                .filter(|a| a.story_id.as_deref() == Some(story_id))
                .map(|a| crate::summarize::StoryMember {
                    article_id: a.id.clone(),
                    title: a.ctx.title.clone(),
                    summary: analyses
                        .get(&a.id)
                        .map(|(an, _)| an.summary.clone())
                        .unwrap_or_default(),
                    source: a.feed_id.clone().unwrap_or_default(),
                    published_at: a.published_at,
                    is_duplicate: a.is_duplicate,
                })
                .collect();
            out.sort_by_key(|m| m.article_id.clone());
            out.truncate(limit);
            Ok(out)
        }

        fn record_story_summary(
            &self,
            story_id: &str,
            summary: &StorySummary,
            members: &[crate::summarize::StoryMember],
            now: i64,
        ) -> CoreResult<()> {
            let mut stories = self.stories.borrow_mut();
            let story = stories
                .get_mut(story_id)
                .ok_or_else(|| CoreError::NotFound(story_id.to_string()))?;
            story.item_title = summary.title.clone();
            story.item_summary = summary.summary.clone();
            story.item_sources = members.iter().map(|m| m.article_id.clone()).collect();
            story.last_summarized_at = Some(now);
            story.summarize_pending = false;
            Ok(())
        }

        fn clear_summarize_pending(&self, story_id: &str) -> CoreResult<()> {
            if let Some(story) = self.stories.borrow_mut().get_mut(story_id) {
                story.summarize_pending = false;
            }
            Ok(())
        }

        fn deactivate_stale_stories(&self, cutoff: i64, _now: i64) -> CoreResult<u64> {
            let mut retired = 0u64;
            for story in self.stories.borrow_mut().values_mut() {
                if story.is_active && story.last_article_at.is_some_and(|t| t < cutoff) {
                    story.is_active = false;
                    retired += 1;
                }
            }
            *self.retired.borrow_mut() += retired;
            Ok(retired)
        }

        fn load_waiting_articles(&self, _cutoff: i64, limit: usize) -> CoreResult<Vec<String>> {
            let mut out: Vec<String> = self
                .articles
                .borrow()
                .values()
                .filter(|a| a.state == PipelineState::Waiting)
                .map(|a| a.id.clone())
                .collect();
            out.sort();
            out.truncate(limit);
            Ok(out)
        }
    }

    #[derive(Default)]
    struct RecordingQueue {
        jobs: RefCell<Vec<(JobPayload, EnqueueOpts)>>,
    }
    impl JobQueue for RecordingQueue {
        fn enqueue(&self, job: &JobPayload, opts: EnqueueOpts) -> CoreResult<()> {
            self.jobs.borrow_mut().push((job.clone(), opts));
            Ok(())
        }
    }
    impl RecordingQueue {
        fn stages(&self) -> Vec<Stage> {
            self.jobs.borrow().iter().map(|(j, _)| j.stage).collect()
        }
        fn count(&self, stage: Stage) -> usize {
            self.jobs
                .borrow()
                .iter()
                .filter(|(j, _)| j.stage == stage)
                .count()
        }
        fn last_delay(&self, stage: Stage) -> Option<i64> {
            self.jobs
                .borrow()
                .iter()
                .rev()
                .find(|(j, _)| j.stage == stage)
                .map(|(_, o)| o.delay)
        }
        fn ids(&self, stage: Stage) -> Vec<String> {
            self.jobs
                .borrow()
                .iter()
                .filter(|(j, _)| j.stage == stage)
                .map(|(j, _)| j.id.clone())
                .collect()
        }
        fn len(&self) -> usize {
            self.jobs.borrow().len()
        }
    }

    struct ScriptedFetcher(FetchOutcome);
    impl Fetcher for ScriptedFetcher {
        fn fetch(&self, _url: &str, _c: &ConditionalHeaders) -> CoreResult<FetchOutcome> {
            Ok(self.0.clone())
        }
    }
    struct RefusingFetcher;
    impl Fetcher for RefusingFetcher {
        fn fetch(&self, _url: &str, _c: &ConditionalHeaders) -> CoreResult<FetchOutcome> {
            Err(CoreError::FetchRefused("blocked private IP".to_string()))
        }
    }
    struct FlakyFetcher;
    impl Fetcher for FlakyFetcher {
        fn fetch(&self, _url: &str, _c: &ConditionalHeaders) -> CoreResult<FetchOutcome> {
            Err(CoreError::Fetch("connection reset".to_string()))
        }
    }

    const RSS_TWO: &str = r#"<rss version="2.0"><channel>
      <item><title>One</title><link>https://x.test/1</link><description>b</description></item>
      <item><title>Two</title><link>https://x.test/2</link><description>b</description></item>
    </channel></rss>"#;

    fn embed_unused() -> impl LlmProvider {
        struct P;
        impl LlmProvider for P {
            fn chat(&self, _r: &ChatRequest) -> CoreResult<ChatResponse> {
                Ok(ChatResponse {
                    content: String::new(),
                    model: "x".into(),
                    usage: Usage::default(),
                    cost_estimate: None,
                })
            }
            fn embed(&self, _i: &str, _m: Option<&str>) -> CoreResult<EmbedResponse> {
                Ok(EmbedResponse {
                    vector: vec![],
                    model: "x".into(),
                    usage: Usage::default(),
                })
            }
        }
        P
    }

    // ---- fetch stage -----------------------------------------------------

    #[test]
    fn fetch_ingests_and_enqueues_new_articles() {
        let store = FakeStore::default().with_feed("feed-1", "https://x.test/rss");
        let queue = RecordingQueue::default();
        let fetcher = ScriptedFetcher(FetchOutcome::Fetched {
            body: RSS_TWO.to_string(),
            etag: Some("etag-1".into()),
            last_modified: None,
        });
        let report = run_fetch(&fetcher, &store, &queue, "feed-1", 1000).unwrap();
        assert_eq!(report.parsed, 2);
        assert_eq!(report.ingested, 2);
        assert_eq!(store.article_count(), 2);
        assert_eq!(queue.len(), 2);
        assert!(queue.stages().iter().all(|s| *s == Stage::Decide));
        assert_eq!(*store.feed_success.borrow(), 1);
    }

    #[test]
    fn fetch_replay_is_idempotent() {
        // M1-6: at-least-once replay of the same fetch job leaves one row each
        // and does not re-enqueue.
        let store = FakeStore::default().with_feed("feed-1", "https://x.test/rss");
        let queue = RecordingQueue::default();
        let fetcher = ScriptedFetcher(FetchOutcome::Fetched {
            body: RSS_TWO.to_string(),
            etag: None,
            last_modified: None,
        });
        run_fetch(&fetcher, &store, &queue, "feed-1", 1000).unwrap();
        let after_first = store.article_count();
        let report = run_fetch(&fetcher, &store, &queue, "feed-1", 2000).unwrap();
        assert_eq!(store.article_count(), after_first, "no duplicate rows");
        assert_eq!(report.ingested, 0, "re-seen URLs are not ingested");
        assert_eq!(queue.len(), 2, "no re-enqueue on replay");
    }

    #[test]
    fn fetch_not_modified_touches_only() {
        let store = FakeStore::default().with_feed("feed-1", "https://x.test/rss");
        let queue = RecordingQueue::default();
        let fetcher = ScriptedFetcher(FetchOutcome::NotModified);
        let report = run_fetch(&fetcher, &store, &queue, "feed-1", 1000).unwrap();
        assert!(report.not_modified);
        assert_eq!(store.article_count(), 0);
        assert_eq!(*store.feed_success.borrow(), 1);
    }

    #[test]
    fn fetch_refused_flags_feed_no_crash() {
        // M1-5: an SSRF-blocked URL is a clean per-feed error, not a crash/retry.
        let store = FakeStore::default().with_feed("feed-1", "http://169.254.169.254/");
        let queue = RecordingQueue::default();
        let report = run_fetch(&RefusingFetcher, &store, &queue, "feed-1", 1000).unwrap();
        assert!(report.feed_flagged);
        assert_eq!(*store.feed_failure.borrow(), 1);
        assert_eq!(*store.feed_success.borrow(), 0);
    }

    #[test]
    fn fetch_transient_records_and_retries() {
        let store = FakeStore::default().with_feed("feed-1", "https://x.test/rss");
        let queue = RecordingQueue::default();
        let err = run_fetch(&FlakyFetcher, &store, &queue, "feed-1", 1000).unwrap_err();
        assert!(err.is_transient(), "network failure must retry");
        assert_eq!(*store.feed_failure.borrow(), 1);
    }

    #[test]
    fn fetch_missing_feed_drops() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let fetcher = ScriptedFetcher(FetchOutcome::NotModified);
        let report = run_fetch(&fetcher, &store, &queue, "gone", 1000).unwrap();
        assert_eq!(report, FetchReport::default());
    }

    // ---- decide stage ----------------------------------------------------

    fn seed_one_article(store: &FakeStore) -> String {
        let res = store
            .upsert_article(&NewArticle {
                url: "https://x.test/a".into(),
                title: "T".into(),
                content: "body body".into(),
                published_at: None,
                feed_id: "feed-1".into(),
                topic_id: "topic-1".into(),
                content_hash: "h".into(),
            })
            .unwrap();
        res.id
    }

    #[test]
    fn decide_keeps_survivor_and_enqueues_analyze() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once(r#"{"score": 90, "reason": "hot"}"#);
        let report = run_decide(&provider, &store, &queue, &id, Some("cheap".into())).unwrap();
        assert_eq!(report.score, Some(90));
        assert!(report.kept);
        assert_eq!(store.state_of(&id), Some(PipelineState::Decided));
        assert_eq!(store.score_of(&id), Some(90));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.stages(), vec![Stage::Analyze]);
        assert_eq!(provider.chat_calls(), 1);
    }

    #[test]
    fn decide_threads_cost_estimate_into_the_report() {
        // G-COST-OPAQUE: the host prices the call and reports cost on the
        // response; `decide` threads it into the `DecideReport` so the worker
        // accounts cost from the response, not a kernel-side SQL query.
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider =
            MockProvider::chat_once(r#"{"score": 90, "reason": "hot"}"#).with_chat_cost(0.0125);
        let report = run_decide(&provider, &store, &queue, &id, Some("cheap".into())).unwrap();
        assert_eq!(
            report.cost_estimate,
            Some(0.0125),
            "the host-reported cost must reach the DecideReport"
        );

        // An unpriced model yields None (the honest "unknown", matching the smoke
        // run's $0.00 with no provider configured).
        let store2 = FakeStore::default();
        let id2 = seed_one_article(&store2);
        let queue2 = RecordingQueue::default();
        let unpriced = MockProvider::chat_once(r#"{"score": 90, "reason": "hot"}"#);
        let report2 = run_decide(&unpriced, &store2, &queue2, &id2, None).unwrap();
        assert_eq!(
            report2.cost_estimate, None,
            "unpriced model reports no cost"
        );
    }

    #[test]
    fn decide_discards_below_threshold_stores_score() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once(r#"{"score": 12, "reason": "off"}"#);
        let report = run_decide(&provider, &store, &queue, &id, None).unwrap();
        assert!(!report.kept);
        assert_eq!(store.state_of(&id), Some(PipelineState::Discarded));
        assert_eq!(
            store.score_of(&id),
            Some(12),
            "score stored even when discarded"
        );
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn decide_malformed_discards_no_retry() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once("I think it's relevant.");
        let report = run_decide(&provider, &store, &queue, &id, None).unwrap();
        assert!(!report.kept);
        assert_eq!(store.state_of(&id), Some(PipelineState::Discarded));
        assert_eq!(provider.chat_calls(), 1);
    }

    #[test]
    fn decide_provider_error_propagates_for_retry() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = MockProvider::new(vec![crate::provider::Scripted::ChatError("down".into())]);
        let err = run_decide(&provider, &store, &queue, &id, None).unwrap_err();
        assert!(err.is_transient());
        // State unchanged (still Fetched) so the retry re-decides cleanly.
        assert_eq!(store.state_of(&id), Some(PipelineState::Fetched));
    }

    #[test]
    fn decide_missing_article_drops() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once(r#"{"score": 90}"#);
        let report = run_decide(&provider, &store, &queue, "gone", None).unwrap();
        assert!(report.missing);
        assert_eq!(provider.chat_calls(), 0);
    }

    // ---- M2: analyze -----------------------------------------------------

    /// A well-formed analyze response naming two entities.
    const ANALYSIS_JSON: &str = r#"{
        "summary": "Nvidia reported record datacenter revenue this quarter.",
        "critical_analysis": "The piece takes company guidance at face value.",
        "fallacy_analysis": "none identified",
        "source_analysis": "Only the CFO is quoted.",
        "entities": [{"name": "Nvidia", "type": "company"},
                     {"name": "Jensen Huang", "type": "person"}]
    }"#;

    /// Seed an article with the fields the M2 stages read.
    fn seed_article(
        store: &FakeStore,
        url: &str,
        title: &str,
        feed: &str,
        published: i64,
    ) -> String {
        let res = store
            .upsert_article(&NewArticle {
                url: url.into(),
                title: title.into(),
                content: format!("{title} body text"),
                published_at: Some(published),
                feed_id: feed.into(),
                topic_id: "topic-1".into(),
                content_hash: "h".into(),
            })
            .unwrap();
        res.id
    }

    fn cfg() -> StageConfig {
        StageConfig::default()
    }

    #[test]
    fn analyze_stores_prose_extracts_entities_and_hands_off_to_embed() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once(ANALYSIS_JSON).with_chat_cost(0.003);

        let report = run_analyze(
            &provider,
            &store,
            &store,
            &queue,
            &id,
            Some("strong".into()),
            &cfg(),
            1000,
        )
        .unwrap();

        assert_eq!(provider.chat_calls(), 1, "exactly one AI call per analyze");
        assert_eq!(report.cost_estimate, Some(0.003));
        assert_eq!(report.entities.created, 2);
        assert_eq!(report.entities.linked, 0);
        assert_eq!(store.state_of(&id), Some(PipelineState::Analyzed));
        assert_eq!(store.entity_names(), vec!["Jensen Huang", "Nvidia"]);
        assert_eq!(queue.stages(), vec![Stage::Embed]);
        // The model's own words are kept beside the parsed fields.
        assert_eq!(
            store.analyses.borrow().get(&id).map(|(_, raw)| raw.clone()),
            Some(ANALYSIS_JSON.to_string())
        );
    }

    #[test]
    fn analyze_links_an_entity_a_previous_article_created() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let first = seed_article(&store, "https://x.test/1", "One", "feed-1", 100);
        let second = seed_article(&store, "https://x.test/2", "Two", "feed-2", 200);

        for id in [&first, &second] {
            run_analyze(
                &MockProvider::chat_once(ANALYSIS_JSON),
                &store,
                &store,
                &queue,
                id,
                None,
                &cfg(),
                1000,
            )
            .unwrap();
        }

        assert_eq!(
            store.entities.borrow().len(),
            2,
            "the second article reuses the first article's entities"
        );
        assert_eq!(store.links.borrow().len(), 4, "both articles link to both");
    }

    #[test]
    fn analyze_merges_a_respelled_entity_as_an_alias() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let first = seed_article(&store, "https://x.test/1", "One", "feed-1", 100);
        let second = seed_article(&store, "https://x.test/2", "Two", "feed-2", 200);

        run_analyze(
            &MockProvider::chat_once(
                r#"{"summary": "s", "entities": [{"name": "OpenAI", "type": "company"}]}"#,
            ),
            &store,
            &store,
            &queue,
            &first,
            None,
            &cfg(),
            1000,
        )
        .unwrap();
        let report = run_analyze(
            &MockProvider::chat_once(
                r#"{"summary": "s", "entities": [{"name": "Open A.I.", "type": "company"}]}"#,
            ),
            &store,
            &store,
            &queue,
            &second,
            None,
            &cfg(),
            1000,
        )
        .unwrap();

        assert_eq!(report.entities.created, 0);
        assert_eq!(report.entities.linked, 1);
        assert_eq!(report.entities.aliases_added, 1);
        assert_eq!(
            store.entities.borrow().len(),
            1,
            "one entity, two spellings"
        );
    }

    #[test]
    fn analyze_records_a_transient_provider_failure_as_retryable() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = MockProvider::new(vec![crate::provider::Scripted::ChatError("503".into())]);

        let err =
            run_analyze(&provider, &store, &store, &queue, &id, None, &cfg(), 1000).unwrap_err();
        assert!(err.is_transient());
        // State untouched, so the retry re-analyzes cleanly and no cost was
        // recorded for a call that produced nothing.
        assert_eq!(store.state_of(&id), Some(PipelineState::Fetched));
        assert_eq!(store.load_daily_spend(&utc_day(1000)).unwrap().calls, 0);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn analyze_flags_an_unusable_response_without_retrying() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once("I read it and it was interesting.");

        let report =
            run_analyze(&provider, &store, &store, &queue, &id, None, &cfg(), 1000).unwrap();
        assert!(report.unparseable);
        assert_eq!(store.state_of(&id), Some(PipelineState::Error));
        assert!(store.article(&id).unwrap().error.is_some());
        assert_eq!(queue.len(), 0, "a flagged article does not advance");
    }

    #[test]
    fn analyze_on_a_missing_article_makes_no_call() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once(ANALYSIS_JSON);
        let report = run_analyze(
            &provider,
            &store,
            &store,
            &queue,
            "gone",
            None,
            &cfg(),
            1000,
        )
        .unwrap();
        assert!(report.missing);
        assert_eq!(provider.chat_calls(), 0);
    }

    // ---- M2: budget ------------------------------------------------------

    #[test]
    fn analyze_pauses_and_defers_to_the_next_day_past_the_limit() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let now = 1_767_225_600 + 3_600; // 2026-01-01T01:00:00Z
        let mut config = cfg();
        config.budget = BudgetConfig {
            daily_limit_usd: 1.0,
            alert_threshold_usd: 0.5,
        };
        // Spend the day's budget.
        store
            .record_cost(&utc_day(now), Stage::Analyze, Some(1.5), now)
            .unwrap();

        let provider = MockProvider::chat_once(ANALYSIS_JSON);
        let report =
            run_analyze(&provider, &store, &store, &queue, &id, None, &config, now).unwrap();

        assert!(report.paused);
        assert_eq!(provider.chat_calls(), 0, "a paused stage spends nothing");
        assert_eq!(queue.stages(), vec![Stage::Analyze], "the job is deferred");
        let delay = queue.last_delay(Stage::Analyze).unwrap();
        assert_eq!(
            utc_day(now + delay),
            "2026-01-02",
            "deferred into the next UTC day, where the budget resets"
        );
        assert_eq!(store.state_of(&id), Some(PipelineState::Fetched));
    }

    #[test]
    fn analyze_runs_under_the_limit_and_at_a_warning_level() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let mut config = cfg();
        config.budget = BudgetConfig {
            daily_limit_usd: 10.0,
            alert_threshold_usd: 0.5,
        };
        store
            .record_cost(&utc_day(1000), Stage::Analyze, Some(2.0), 1000)
            .unwrap();

        let report = run_analyze(
            &MockProvider::chat_once(ANALYSIS_JSON).with_chat_cost(0.01),
            &store,
            &store,
            &queue,
            &id,
            None,
            &config,
            1000,
        )
        .unwrap();
        assert!(!report.paused, "warn level still runs");
        assert_eq!(store.state_of(&id), Some(PipelineState::Analyzed));
    }

    #[test]
    fn an_unpriced_call_counts_as_unknown_not_free() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        run_analyze(
            &MockProvider::chat_once(ANALYSIS_JSON), // no cost configured
            &store,
            &store,
            &queue,
            &id,
            None,
            &cfg(),
            1000,
        )
        .unwrap();
        let spend = store.load_daily_spend(&utc_day(1000)).unwrap();
        assert_eq!(spend.calls, 1);
        assert_eq!(spend.unpriced_calls, 1);
        assert_eq!(spend.spent_usd, 0.0);
    }

    // ---- M2: embed -------------------------------------------------------

    #[test]
    fn embed_stores_a_vector_and_hands_off_to_cluster() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        run_analyze(
            &MockProvider::chat_once(ANALYSIS_JSON),
            &store,
            &store,
            &queue,
            &id,
            None,
            &cfg(),
            1000,
        )
        .unwrap();

        let report = run_embed(&store, &store, &queue, &embed_unused(), &id, &cfg(), 1000).unwrap();
        assert!(!report.missing && !report.empty);
        assert_eq!(store.state_of(&id), Some(PipelineState::Embedded));
        let stored = store.load_vector(&id).unwrap().unwrap();
        assert_eq!(stored.recipe, recipe_id(crate::embed::DEFAULT_DIMENSION));
        assert_eq!(stored.vector.len(), crate::embed::DEFAULT_DIMENSION);
        assert_eq!(queue.count(Stage::Cluster), 1);
    }

    #[test]
    fn embed_makes_no_model_call_and_costs_nothing() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        run_embed(&store, &store, &queue, &embed_unused(), &id, &cfg(), 1000).unwrap();
        assert_eq!(
            store.load_daily_spend(&utc_day(1000)).unwrap().calls,
            0,
            "the lexical embed route spends nothing"
        );
    }

    #[test]
    fn embed_on_a_missing_article_is_a_no_op() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let report = run_embed(
            &store,
            &store,
            &queue,
            &embed_unused(),
            "gone",
            &cfg(),
            1000,
        )
        .unwrap();
        assert!(report.missing);
        assert_eq!(queue.len(), 0);
    }

    // ---- K1 fix 2: the semantic embed route ------------------------------

    /// A provider that answers `embed` with a fixed vector and records what it
    /// was asked to embed. `chat` is not part of this stage.
    struct FakeEmbedder {
        vector: Vec<f32>,
        model: &'static str,
        seen: std::cell::RefCell<Vec<(String, Option<String>)>>,
    }

    impl FakeEmbedder {
        fn new(vector: Vec<f32>) -> Self {
            Self {
                vector,
                model: "text-embedding-3-small",
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl LlmProvider for FakeEmbedder {
        fn chat(&self, _r: &ChatRequest) -> CoreResult<ChatResponse> {
            panic!("the embed stage must not make a chat call");
        }
        fn embed(&self, input: &str, model: Option<&str>) -> CoreResult<EmbedResponse> {
            self.seen
                .borrow_mut()
                .push((input.to_string(), model.map(str::to_string)));
            Ok(EmbedResponse {
                vector: self.vector.clone(),
                model: self.model.into(),
                usage: Usage::default(),
            })
        }
    }

    /// A provider whose embeddings endpoint is down.
    struct FailingEmbedder;
    impl LlmProvider for FailingEmbedder {
        fn chat(&self, _r: &ChatRequest) -> CoreResult<ChatResponse> {
            panic!("the embed stage must not make a chat call");
        }
        fn embed(&self, _i: &str, _m: Option<&str>) -> CoreResult<EmbedResponse> {
            Err(CoreError::Provider("embeddings endpoint down".into()))
        }
    }

    fn semantic_cfg(model: &str) -> StageConfig {
        StageConfig {
            embed_model: Some(model.to_string()),
            ..cfg()
        }
    }

    #[test]
    fn the_semantic_route_stores_the_providers_vector_under_its_own_recipe() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let provider = FakeEmbedder::new(vec![3.0, 0.0, 4.0]);
        let config = semantic_cfg("text-embedding-3-small");

        let report = run_embed(&store, &store, &queue, &provider, &id, &config, 1000).unwrap();
        assert!(!report.missing && !report.empty);

        let stored = store.load_vector(&id).unwrap().unwrap();
        assert_eq!(
            stored.recipe,
            crate::embed::semantic_recipe_id("text-embedding-3-small"),
            "a semantic vector must not wear the lexical recipe"
        );
        // Normalized on the way in: 3-4-5 triangle → 0.6, 0.0, 0.8.
        assert!((stored.vector[0] - 0.6).abs() < 1e-5, "{:?}", stored.vector);
        assert!((stored.vector[2] - 0.8).abs() < 1e-5, "{:?}", stored.vector);
        assert_eq!(queue.count(Stage::Cluster), 1);

        // The model asked for is the one configured, and it was given the
        // article's text — not an empty string, which is what the pre-(1,1)
        // host's chat route handed back.
        let seen = provider.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1.as_deref(), Some("text-embedding-3-small"));
        assert!(!seen[0].0.trim().is_empty(), "nothing was embedded");
    }

    #[test]
    fn the_semantic_route_embeds_title_summary_and_entities() {
        let ctx = EmbedContext {
            title: "Quake hits the coast".into(),
            summary: "A magnitude 6 quake struck early Tuesday.".into(),
            entities: vec!["Pacific Coast".into(), "USGS".into()],
        };
        let text = semantic_embed_text(&ctx);
        assert!(text.contains("Quake hits the coast"));
        assert!(text.contains("magnitude 6"));
        assert!(text.contains("Pacific Coast") && text.contains("USGS"));
    }

    #[test]
    fn a_provider_failure_retries_rather_than_writing_a_lexical_vector() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();

        let err = run_embed(
            &store,
            &store,
            &queue,
            &FailingEmbedder,
            &id,
            &semantic_cfg("m"),
            1000,
        )
        .expect_err("a provider failure is transient, not a reason to fake a vector");
        assert!(matches!(err, CoreError::Provider(_)), "got {err:?}");

        assert!(
            store.load_vector(&id).unwrap().is_none(),
            "nothing may be stored under the semantic recipe that is not a semantic vector"
        );
        assert_eq!(queue.count(Stage::Cluster), 0);
    }

    #[test]
    fn switching_routes_retires_the_old_vectors_instead_of_mixing_spaces() {
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();

        // Embedded lexically...
        run_embed(&store, &store, &queue, &embed_unused(), &id, &cfg(), 1000).unwrap();
        let lexical = store.load_vector(&id).unwrap().unwrap();
        assert!(lexical.recipe.starts_with("lex-v1/"));

        // ...then the site switches to the semantic route. The cluster stage
        // must not compare the stale vector; it re-enqueues an embed job.
        let config = semantic_cfg("text-embedding-3-small");
        let report = run_cluster(&store, &store, &queue, &id, &config, 1000).unwrap();
        assert!(
            report.re_embedded,
            "a vector from the other route is not comparable, got {report:?}"
        );
        assert!(report.decision.is_none());
    }

    #[test]
    fn a_story_centroid_carries_the_articles_recipe_not_its_length() {
        // The latent bug the semantic route exposed: the story seed derived its
        // recipe from `vector.len()`, which is `lex-v1/<dim>` — right by
        // coincidence on the lexical route (where the dimension *is* the
        // recipe), and wrong on every other route. A `lex-v1/4` centroid never
        // matches a `sem-v1/<model>` article, so every article silently started
        // its own story and clustering did nothing at all.
        let store = FakeStore::default();
        let id = seed_one_article(&store);
        let queue = RecordingQueue::default();
        let config = semantic_cfg("text-embedding-3-small");

        run_embed(
            &store,
            &store,
            &queue,
            &FakeEmbedder::new(vec![1.0, 0.0, 0.0, 0.0]),
            &id,
            &config,
            1000,
        )
        .unwrap();
        run_cluster(&store, &store, &queue, &id, &config, 1000).unwrap();

        let recipes: Vec<String> = store
            .stories
            .borrow()
            .values()
            .map(|s| s.recipe.clone())
            .collect();
        assert_eq!(recipes.len(), 1, "the first article creates a story");
        assert_eq!(
            recipes[0],
            crate::embed::semantic_recipe_id("text-embedding-3-small"),
            "the centroid must be comparable to the article that seeded it"
        );
    }

    #[test]
    fn the_recipe_is_the_same_string_for_the_writer_and_the_reader() {
        // The bug this guards: run_embed writing one recipe and run_cluster
        // expecting another would re-enqueue an embed job forever.
        let lexical = cfg();
        assert_eq!(
            lexical.vector_recipe(),
            recipe_id(crate::embed::clamp_dimension(lexical.vector_dim))
        );
        let semantic = semantic_cfg("some-model");
        assert_eq!(
            semantic.vector_recipe(),
            crate::embed::semantic_recipe_id("some-model")
        );
        assert_ne!(lexical.vector_recipe(), semantic.vector_recipe());
    }

    // ---- M2: cluster -----------------------------------------------------

    /// Analyze + embed one article, returning its id.
    fn through_embed(
        store: &FakeStore,
        queue: &RecordingQueue,
        url: &str,
        title: &str,
        feed: &str,
        published: i64,
        analysis: &str,
    ) -> String {
        let id = seed_article(store, url, title, feed, published);
        run_analyze(
            &MockProvider::chat_once(analysis),
            store,
            store,
            queue,
            &id,
            None,
            &cfg(),
            published,
        )
        .unwrap();
        run_embed(store, store, queue, &embed_unused(), &id, &cfg(), published).unwrap();
        id
    }

    #[test]
    fn a_first_article_creates_a_story_and_enqueues_a_summary() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let id = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Nvidia posts record quarter",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );

        let report = run_cluster(&store, &store, &queue, &id, &cfg(), 1000).unwrap();
        assert_eq!(report.decision, Some(ClusterDecision::Create));
        assert_eq!(store.story_count(), 1);
        assert_eq!(store.state_of(&id), Some(PipelineState::Complete));
        let story_id = report.story_id.unwrap();
        assert_eq!(
            store.article(&id).unwrap().story_id.as_deref(),
            Some(story_id.as_str())
        );
        assert_eq!(queue.count(Stage::Summarize), 1);
    }

    #[test]
    fn a_second_article_on_the_same_subject_joins_the_story() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let first = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Nvidia reports record datacenter revenue",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        run_cluster(&store, &store, &queue, &first, &cfg(), 1000).unwrap();

        let second = through_embed(
            &store,
            &queue,
            "https://x.test/2",
            "Nvidia datacenter revenue hits a record",
            "feed-2",
            1100,
            ANALYSIS_JSON,
        );
        let report = run_cluster(&store, &store, &queue, &second, &cfg(), 1100).unwrap();

        match report.decision {
            Some(ClusterDecision::Join { .. }) => {}
            other => panic!("expected Join, got {other:?}"),
        }
        assert_eq!(store.story_count(), 1, "no second story");
        let story = store.story(&report.story_id.unwrap()).unwrap();
        assert_eq!(story.article_count, 2);
        assert_eq!(story.last_article_at, Some(1100));
        assert_eq!(story.first_article_at, Some(1000));
    }

    #[test]
    fn an_unrelated_article_starts_its_own_story() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let first = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Nvidia posts record quarter",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        run_cluster(&store, &store, &queue, &first, &cfg(), 1000).unwrap();

        let other_analysis = r#"{"summary": "Heavy rain closed the coastal highway for a second day.",
            "entities": [{"name": "Pacific Coast Highway", "type": "place"}]}"#;
        let second = through_embed(
            &store,
            &queue,
            "https://x.test/2",
            "Flooding closes coastal highway",
            "feed-2",
            1100,
            other_analysis,
        );
        let report = run_cluster(&store, &store, &queue, &second, &cfg(), 1100).unwrap();

        assert_eq!(report.decision, Some(ClusterDecision::Create));
        assert_eq!(store.story_count(), 2);
    }

    #[test]
    fn an_identical_article_from_another_feed_is_filed_as_a_duplicate() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let title = "Nvidia reports record datacenter revenue";
        let first = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            title,
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        let created = run_cluster(&store, &store, &queue, &first, &cfg(), 1000).unwrap();
        let story_id = created.story_id.unwrap();

        // Same headline, same body, same analysis, different feed: a re-report.
        let second = through_embed(
            &store,
            &queue,
            "https://x.test/2",
            title,
            "feed-2",
            1100,
            ANALYSIS_JSON,
        );
        let report = run_cluster(&store, &store, &queue, &second, &cfg(), 1100).unwrap();

        match report.decision {
            Some(ClusterDecision::Duplicate {
                ref of_article_id,
                similarity,
                ..
            }) => {
                assert_eq!(of_article_id, &first);
                assert!(similarity >= crate::cluster::DEFAULT_NEAR_DUP_THRESHOLD);
            }
            other => panic!("expected Duplicate, got {other:?}"),
        }
        let dup = store.article(&second).unwrap();
        assert!(dup.is_duplicate);
        assert_eq!(dup.duplicate_of.as_deref(), Some(first.as_str()));
        assert_eq!(dup.story_id.as_deref(), Some(story_id.as_str()));
        // Filed as a source, not counted as a member.
        assert_eq!(store.story(&story_id).unwrap().article_count, 1);
    }

    #[test]
    fn cluster_rebuilds_a_vector_whose_recipe_no_longer_matches() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let id = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Title",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        // The operator changed the dimension; the stored vector is no longer
        // comparable to anything the new config produces.
        let mut config = cfg();
        config.vector_dim = 512;

        let report = run_cluster(&store, &store, &queue, &id, &config, 1000).unwrap();
        assert!(report.re_embedded);
        assert_eq!(report.decision, None);
        assert_eq!(
            store.story_count(),
            0,
            "no story built on an incomparable vector"
        );
        assert_eq!(
            queue.count(Stage::Embed),
            2,
            "the original plus the rebuild"
        );
    }

    #[test]
    fn a_busy_clustering_lease_defers_instead_of_splitting_a_story() {
        // The kernel honours one concurrency figure per plugin, not per queue,
        // so cluster jobs do run in parallel. Two workers scoring the same
        // event at once would each see no candidate and each create a story;
        // the lease is what prevents that, and the loser defers.
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let id = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Nvidia record revenue",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        // Another worker holds the lease.
        assert!(
            store
                .try_acquire_cluster_lease("other-worker", 1000, 30)
                .unwrap()
        );

        let report = run_cluster(&store, &store, &queue, &id, &cfg(), 1000).unwrap();
        assert!(report.lease_busy);
        assert_eq!(report.decision, None);
        assert_eq!(store.story_count(), 0, "no story built while locked out");
        assert_eq!(
            queue.last_delay(Stage::Cluster),
            Some(crate::pipeline::CLUSTER_LEASE_RETRY_SECONDS),
            "the job comes back shortly, it is not dropped"
        );
    }

    #[test]
    fn an_expired_lease_does_not_wedge_the_cluster_stage() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let id = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Nvidia record revenue",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        // A worker that trapped mid-job left its lease behind.
        store
            .try_acquire_cluster_lease("dead-worker", 1000, 30)
            .unwrap();

        // Before expiry: locked out.
        assert!(
            run_cluster(&store, &store, &queue, &id, &cfg(), 1020)
                .unwrap()
                .lease_busy
        );
        // After expiry: the stage recovers on its own.
        let report = run_cluster(&store, &store, &queue, &id, &cfg(), 1031).unwrap();
        assert!(!report.lease_busy);
        assert_eq!(report.decision, Some(ClusterDecision::Create));
    }

    #[test]
    fn the_lease_is_released_even_when_the_job_fails() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        // No article: the run returns early, and must still hand the lease back.
        run_cluster(&store, &store, &queue, "gone", &cfg(), 1000).unwrap();
        assert!(
            store
                .try_acquire_cluster_lease("next-worker", 1000, 30)
                .unwrap(),
            "the next worker must not be blocked by a finished job"
        );
    }

    #[test]
    fn cluster_on_a_missing_article_is_a_no_op() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let report = run_cluster(&store, &store, &queue, "gone", &cfg(), 1000).unwrap();
        assert!(report.missing);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn a_deferred_article_is_bounded_and_eventually_gets_a_story() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let first = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Nvidia reports record revenue",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        run_cluster(&store, &store, &queue, &first, &cfg(), 1000).unwrap();

        // Tune the threshold so the second article lands in the wait band.
        let second = through_embed(
            &store,
            &queue,
            "https://x.test/2",
            "Chip revenue climbs at Nvidia",
            "feed-2",
            1100,
            ANALYSIS_JSON,
        );
        let mut config = cfg();
        let probe = run_cluster(&store, &store, &queue, &second, &config, 1100).unwrap();
        let joined_score = match probe.decision {
            Some(ClusterDecision::Join { score, .. }) => score,
            other => panic!("fixture must join at the default threshold, got {other:?}"),
        };

        // Re-run the same article against a threshold just above its score.
        let store2 = FakeStore::default();
        let queue2 = RecordingQueue::default();
        let a = through_embed(
            &store2,
            &queue2,
            "https://x.test/1",
            "Nvidia reports record revenue",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        run_cluster(&store2, &store2, &queue2, &a, &config, 1000).unwrap();
        let b = through_embed(
            &store2,
            &queue2,
            "https://x.test/2",
            "Chip revenue climbs at Nvidia",
            "feed-2",
            1100,
            ANALYSIS_JSON,
        );
        config.cluster.join_threshold = joined_score + crate::cluster::WAIT_MARGIN / 2.0;

        let deferred = run_cluster(&store2, &store2, &queue2, &b, &config, 1100).unwrap();
        assert!(matches!(
            deferred.decision,
            Some(ClusterDecision::Wait { .. })
        ));
        assert_eq!(store2.state_of(&b), Some(PipelineState::Waiting));
        assert_eq!(store2.article(&b).unwrap().waits, 1);

        // Second pass: the wait budget is spent, so a story is forced.
        let forced = run_cluster(&store2, &store2, &queue2, &b, &config, 1200).unwrap();
        assert_eq!(forced.decision, Some(ClusterDecision::Create));
        assert_eq!(store2.state_of(&b), Some(PipelineState::Complete));
    }

    // ---- M2: summarize ---------------------------------------------------

    const SUMMARY_JSON: &str = r#"{"title": "Nvidia posts a record quarter",
        "summary": "Two outlets reported the same record.\n\nThey disagree on the margin."}"#;

    /// Build a story with two members and return its id.
    fn story_with_two_members(store: &FakeStore, queue: &RecordingQueue) -> String {
        let first = through_embed(
            store,
            queue,
            "https://x.test/1",
            "Nvidia reports record datacenter revenue",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        let created = run_cluster(store, store, queue, &first, &cfg(), 1000).unwrap();
        let second = through_embed(
            store,
            queue,
            "https://x.test/2",
            "Nvidia datacenter revenue hits a record",
            "feed-2",
            1100,
            ANALYSIS_JSON,
        );
        run_cluster(store, store, queue, &second, &cfg(), 1100).unwrap();
        created.story_id.unwrap()
    }

    #[test]
    fn summarize_writes_the_narrative_and_the_source_list() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let story_id = story_with_two_members(&store, &queue);

        let provider = MockProvider::chat_once(SUMMARY_JSON).with_chat_cost(0.02);
        let report = run_summarize(
            &provider,
            &store,
            &store,
            &queue,
            &story_id,
            Some("strong".into()),
            &cfg(),
            2000,
        )
        .unwrap();

        assert_eq!(provider.chat_calls(), 1);
        assert_eq!(report.members, 2);
        assert_eq!(report.cost_estimate, Some(0.02));
        let story = store.story(&story_id).unwrap();
        assert_eq!(story.item_title, "Nvidia posts a record quarter");
        assert!(story.item_summary.contains("disagree"));
        assert_eq!(story.item_sources.len(), 2, "both members credited");
        assert_eq!(story.last_summarized_at, Some(2000));
        assert!(!story.summarize_pending);
    }

    #[test]
    fn summarize_prompt_names_every_source() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let story_id = story_with_two_members(&store, &queue);
        let provider = MockProvider::chat_once(SUMMARY_JSON);
        run_summarize(
            &provider,
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &cfg(),
            2000,
        )
        .unwrap();
        let prompt = provider.last_chat().unwrap().user;
        assert!(prompt.contains("feed-1"));
        assert!(prompt.contains("feed-2"));
    }

    #[test]
    fn a_burst_of_joins_produces_one_summary_call() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let story_id = story_with_two_members(&store, &queue);
        let config = cfg();

        // First run summarizes.
        let first = MockProvider::chat_once(SUMMARY_JSON);
        run_summarize(
            &first, &store, &store, &queue, &story_id, None, &config, 2000,
        )
        .unwrap();
        assert_eq!(first.chat_calls(), 1);

        // Every further job inside the interval defers instead of calling, and
        // they all defer to the same instant.
        for now in [2001, 2100, 2300] {
            let again = MockProvider::chat_once(SUMMARY_JSON);
            let report = run_summarize(
                &again, &store, &store, &queue, &story_id, None, &config, now,
            )
            .unwrap();
            assert_eq!(again.chat_calls(), 0, "rate-limited, no call");
            assert_eq!(
                now + report.deferred_seconds,
                2000 + config.summarize_min_interval
            );
        }

        // Past the interval it summarizes again.
        let later = MockProvider::chat_once(SUMMARY_JSON);
        run_summarize(
            &later,
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &config,
            2000 + config.summarize_min_interval,
        )
        .unwrap();
        assert_eq!(later.chat_calls(), 1);
    }

    #[test]
    fn summarize_keeps_the_previous_summary_when_the_model_returns_nothing_usable() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let story_id = story_with_two_members(&store, &queue);
        let mut config = cfg();
        config.summarize_min_interval = 0;

        run_summarize(
            &MockProvider::chat_once(SUMMARY_JSON),
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &config,
            2000,
        )
        .unwrap();
        let good = store.story(&story_id).unwrap().item_summary;

        let report = run_summarize(
            &MockProvider::chat_once("I'd rather not."),
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &config,
            3000,
        )
        .unwrap();
        assert!(report.unparseable);
        assert_eq!(
            store.story(&story_id).unwrap().item_summary,
            good,
            "a bad response must not erase a good summary"
        );
    }

    #[test]
    fn summarize_propagates_a_provider_failure_for_retry() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let story_id = story_with_two_members(&store, &queue);
        let provider = MockProvider::new(vec![crate::provider::Scripted::ChatError("504".into())]);
        let err = run_summarize(
            &provider,
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &cfg(),
            2000,
        )
        .unwrap_err();
        assert!(err.is_transient());
    }

    #[test]
    fn summarize_pauses_past_the_daily_limit() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let story_id = story_with_two_members(&store, &queue);
        let now = 1_767_225_600 + 3_600;
        let mut config = cfg();
        config.budget = BudgetConfig {
            daily_limit_usd: 1.0,
            alert_threshold_usd: 0.5,
        };
        store
            .record_cost(&utc_day(now), Stage::Analyze, Some(2.0), now)
            .unwrap();

        let provider = MockProvider::chat_once(SUMMARY_JSON);
        let report = run_summarize(
            &provider, &store, &store, &queue, &story_id, None, &config, now,
        )
        .unwrap();
        assert!(report.paused);
        assert_eq!(provider.chat_calls(), 0);
        assert_eq!(
            utc_day(now + queue.last_delay(Stage::Summarize).unwrap()),
            "2026-01-02"
        );
    }

    #[test]
    fn summarize_on_a_missing_story_is_a_no_op() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let provider = MockProvider::chat_once(SUMMARY_JSON);
        let report = run_summarize(
            &provider,
            &store,
            &store,
            &queue,
            "gone",
            None,
            &cfg(),
            2000,
        )
        .unwrap();
        assert!(report.missing);
        assert_eq!(provider.chat_calls(), 0);
    }

    // ---- M2: maintenance -------------------------------------------------

    #[test]
    fn maintenance_retires_idle_stories_purges_old_bodies_and_requeues_waiters() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let day = 86_400;
        let now = 400 * day;

        // An old, completed article inside a story that has gone idle.
        let old = through_embed(
            &store,
            &queue,
            "https://x.test/old",
            "Old news",
            "feed-1",
            now - 300 * day,
            ANALYSIS_JSON,
        );
        run_cluster(&store, &store, &queue, &old, &cfg(), now - 300 * day).unwrap();
        // A deferred article.
        store.record_wait(&old, now).unwrap();
        let waiting = seed_article(&store, "https://x.test/w", "Waiting", "feed-2", now);
        store.record_wait(&waiting, now).unwrap();

        let jobs_before = queue.len();
        let report = run_maintenance(&store, &store, &queue, &cfg(), now).unwrap();

        assert_eq!(report.stories_retired, 1, "an idle story is retired");
        assert_eq!(report.waiting_requeued, 2);
        assert_eq!(queue.len(), jobs_before + 2);
        assert_eq!(
            queue.ids(Stage::Cluster).len(),
            3,
            "one per waiter plus the original"
        );
    }

    #[test]
    fn retention_reclaims_only_terminal_bodies() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let day = 86_400;
        let now = 400 * day;
        let mut config = cfg();
        config.article_retention_days = 180;

        // Terminal and old: reclaimed.
        let old = through_embed(
            &store,
            &queue,
            "https://x.test/old",
            "Old news",
            "feed-1",
            now - 300 * day,
            ANALYSIS_JSON,
        );
        run_cluster(&store, &store, &queue, &old, &config, now - 300 * day).unwrap();
        // Old but still in flight: kept, because a later stage still needs it.
        let in_flight = seed_article(
            &store,
            "https://x.test/live",
            "Live",
            "feed-1",
            now - 300 * day,
        );
        // Terminal but recent: kept.
        let recent = through_embed(
            &store,
            &queue,
            "https://x.test/new",
            "Fresh news",
            "feed-2",
            now - day,
            ANALYSIS_JSON,
        );
        run_cluster(&store, &store, &queue, &recent, &config, now - day).unwrap();

        let report = run_maintenance(&store, &store, &queue, &config, now).unwrap();
        assert_eq!(report.articles_purged, 1);
        assert!(store.article(&old).unwrap().content_purged);
        assert!(!store.article(&in_flight).unwrap().content_purged);
        assert!(!store.article(&recent).unwrap().content_purged);
        // Metadata and scores survive the purge.
        assert_eq!(store.state_of(&old), Some(PipelineState::Complete));
        assert!(store.article(&old).unwrap().story_id.is_some());
    }

    // ---- M2: idempotency and the full chain ------------------------------

    #[test]
    fn a_redelivered_analyze_job_converges() {
        // Queue v2 is at-least-once, so every stage must survive re-delivery.
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let id = seed_one_article(&store);
        for _ in 0..3 {
            run_analyze(
                &MockProvider::chat_once(ANALYSIS_JSON),
                &store,
                &store,
                &queue,
                &id,
                None,
                &cfg(),
                1000,
            )
            .unwrap();
        }
        assert_eq!(
            store.entities.borrow().len(),
            2,
            "entities are not duplicated"
        );
        assert_eq!(store.links.borrow().len(), 2, "links are not duplicated");
        assert_eq!(store.state_of(&id), Some(PipelineState::Analyzed));
    }

    #[test]
    fn a_redelivered_embed_job_converges() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let id = seed_one_article(&store);
        run_embed(&store, &store, &queue, &embed_unused(), &id, &cfg(), 1000).unwrap();
        let first = store.load_vector(&id).unwrap().unwrap();
        run_embed(&store, &store, &queue, &embed_unused(), &id, &cfg(), 1000).unwrap();
        assert_eq!(store.load_vector(&id).unwrap().unwrap(), first);
    }

    #[test]
    fn a_redelivered_cluster_job_does_not_double_count_a_member() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let id = through_embed(
            &store,
            &queue,
            "https://x.test/1",
            "Nvidia record revenue",
            "feed-1",
            1000,
            ANALYSIS_JSON,
        );
        let first = run_cluster(&store, &store, &queue, &id, &cfg(), 1000).unwrap();
        let story_id = first.story_id.unwrap();
        assert_eq!(store.story(&story_id).unwrap().article_count, 1);

        // Re-delivery: the article is already a member, so it joins its own
        // story rather than founding a second one.
        let again = run_cluster(&store, &store, &queue, &id, &cfg(), 1000).unwrap();
        assert_eq!(store.story_count(), 1, "no second story on re-delivery");
        assert_eq!(again.story_id.as_deref(), Some(story_id.as_str()));
    }

    #[test]
    fn a_redelivered_summarize_job_is_rate_limited_into_a_no_op() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let story_id = story_with_two_members(&store, &queue);
        let first = MockProvider::chat_once(SUMMARY_JSON);
        run_summarize(
            &first,
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &cfg(),
            2000,
        )
        .unwrap();
        let second = MockProvider::chat_once(SUMMARY_JSON);
        run_summarize(
            &second,
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &cfg(),
            2000,
        )
        .unwrap();
        assert_eq!(second.chat_calls(), 0);
    }

    #[test]
    fn the_whole_m2_chain_turns_two_articles_into_one_summarized_story() {
        let store = FakeStore::default();
        let queue = RecordingQueue::default();

        let ids: Vec<String> = [
            (
                "https://x.test/1",
                "Nvidia reports record datacenter revenue",
                "feed-1",
                1000,
            ),
            (
                "https://x.test/2",
                "Nvidia datacenter revenue hits a record",
                "feed-2",
                1100,
            ),
        ]
        .iter()
        .map(|(url, title, feed, at)| {
            let id = seed_article(&store, url, title, feed, *at);
            run_analyze(
                &MockProvider::chat_once(ANALYSIS_JSON).with_chat_cost(0.003),
                &store,
                &store,
                &queue,
                &id,
                None,
                &cfg(),
                *at,
            )
            .unwrap();
            run_embed(&store, &store, &queue, &embed_unused(), &id, &cfg(), *at).unwrap();
            run_cluster(&store, &store, &queue, &id, &cfg(), *at).unwrap();
            id
        })
        .collect();

        assert_eq!(store.story_count(), 1, "two reports, one story");
        let story_id = store.article(&ids[0]).unwrap().story_id.unwrap();
        assert_eq!(
            store.article(&ids[1]).unwrap().story_id.as_deref(),
            Some(story_id.as_str())
        );

        run_summarize(
            &MockProvider::chat_once(SUMMARY_JSON).with_chat_cost(0.02),
            &store,
            &store,
            &queue,
            &story_id,
            None,
            &cfg(),
            2000,
        )
        .unwrap();

        let story = store.story(&story_id).unwrap();
        assert_eq!(story.article_count, 2);
        assert_eq!(story.item_title, "Nvidia posts a record quarter");
        assert_eq!(story.item_sources.len(), 2);
        // Two analyze calls plus one summarize, every one of them priced from
        // the response rather than from a kernel-side query.
        let spend = store.load_daily_spend(&utc_day(1000)).unwrap();
        assert_eq!(spend.calls, 3);
        assert_eq!(spend.unpriced_calls, 0);
        assert!((spend.spent_usd - 0.026).abs() < 1e-9);
    }

    #[test]
    fn end_to_end_fetch_to_terminal_states() {
        // M1-9 at core level: fetch → dedup → decide reaching terminal states.
        let store = FakeStore::default().with_feed("feed-1", "https://x.test/rss");
        let queue = RecordingQueue::default();
        let fetcher = ScriptedFetcher(FetchOutcome::Fetched {
            body: RSS_TWO.to_string(),
            etag: None,
            last_modified: None,
        });
        let _ = embed_unused(); // keep the trait obj construction covered
        run_fetch(&fetcher, &store, &queue, "feed-1", 1000).unwrap();

        // Drain the decide jobs the fetch enqueued: one keep, one discard.
        let decide_jobs: Vec<String> = queue.ids(Stage::Decide);
        assert_eq!(decide_jobs.len(), 2);

        let provider = MockProvider::new(vec![
            crate::provider::Scripted::Chat(r#"{"score": 80}"#.into()),
            crate::provider::Scripted::Chat(r#"{"score": 20}"#.into()),
        ]);
        for id in &decide_jobs {
            run_decide(&provider, &store, &queue, id, Some("cheap".into())).unwrap();
        }
        let states: Vec<PipelineState> = decide_jobs
            .iter()
            .map(|id| store.state_of(id).unwrap())
            .collect();
        assert!(states.contains(&PipelineState::Decided));
        assert!(states.contains(&PipelineState::Discarded));
        // The kept one produced an analyze job.
        assert_eq!(queue.count(Stage::Analyze), 1);
    }

    // ---- M4: notifications ------------------------------------------------

    use crate::notify::{
        ChannelKind, ChannelOutcome, OutboundRequest, OutboundResponse, StoredEvent,
    };
    use crate::ports::{FailingFeed, QueueHealth};

    /// An in-memory [`NotifyStore`].
    ///
    /// Models the two properties the real one gets from Postgres and nothing
    /// else: `(kind, dedup_key)` is unique, and claiming a digest is atomic in
    /// the sense that a claimed row leaves the pending set.
    #[derive(Default)]
    struct FakeNotify {
        events: RefCell<Vec<StoredEvent>>,
        keys: RefCell<Vec<(String, String)>>,
        next: RefCell<u64>,
        channels: RefCell<Vec<ChannelConfig>>,
        deliveries: RefCell<Vec<(String, ChannelOutcome)>>,
        attempts: RefCell<HashMap<(String, String), u32>>,
        health: RefCell<HashMap<String, (u32, Option<String>)>>,
        topics: RefCell<HashMap<String, NotifyPriority>>,
        reasons: RefCell<HashMap<String, String>>,
        sent_at: RefCell<HashMap<String, i64>>,
        failing: RefCell<Vec<FailingFeed>>,
        queue_health: RefCell<QueueHealth>,
    }

    impl FakeNotify {
        fn with_channels(channels: Vec<ChannelConfig>) -> Self {
            Self {
                channels: RefCell::new(channels),
                ..Self::default()
            }
        }

        fn event(&self, id: &str) -> StoredEvent {
            self.events
                .borrow()
                .iter()
                .find(|e| e.id == id)
                .cloned()
                .expect("event exists")
        }

        fn states(&self) -> Vec<EventState> {
            self.events.borrow().iter().map(|e| e.state).collect()
        }

        fn deliveries_for(&self, event_id: &str) -> Vec<ChannelOutcome> {
            self.deliveries
                .borrow()
                .iter()
                .filter(|(id, _)| id == event_id)
                .map(|(_, o)| o.clone())
                .collect()
        }

        /// The candidate set both [`NotifyStore::pending_digestible`] and
        /// [`NotifyStore::claim_digest`] work over, so the count cannot disagree
        /// with what the claim takes.
        fn digest_candidates(&self, head: &str, window_start: i64, now: i64) -> Vec<String> {
            self.events
                .borrow()
                .iter()
                .filter(|e| {
                    e.id != head
                        && e.state == EventState::Pending
                        && e.kind.is_digestible()
                        && e.priority == NotifyPriority::Normal
                        && e.created >= window_start
                        && e.scheduled_at <= now
                })
                .map(|e| e.id.clone())
                .collect()
        }
    }

    impl NotifyStore for FakeNotify {
        fn record_event(&self, event: &NewEvent, now: i64) -> CoreResult<Option<String>> {
            let key = (event.kind.as_str().to_string(), event.dedup_key.clone());
            if self.keys.borrow().contains(&key) {
                return Ok(None);
            }
            self.keys.borrow_mut().push(key);
            let mut next = self.next.borrow_mut();
            *next += 1;
            let id = format!("evt-{next}");
            self.events.borrow_mut().push(StoredEvent {
                id: id.clone(),
                kind: event.kind,
                priority: event.priority,
                subject_id: event.subject_id.clone(),
                state: EventState::Pending,
                scheduled_at: now,
                created: now,
                title: event.title.clone(),
                body: event.body.clone(),
                link: event.link.clone(),
                data: event.data.clone(),
            });
            Ok(Some(id))
        }

        fn load_event(&self, event_id: &str) -> CoreResult<Option<StoredEvent>> {
            Ok(self
                .events
                .borrow()
                .iter()
                .find(|e| e.id == event_id)
                .cloned())
        }

        fn load_channels(&self) -> CoreResult<Vec<ChannelConfig>> {
            Ok(self.channels.borrow().clone())
        }

        fn load_channel(&self, channel_id: &str) -> CoreResult<Option<ChannelConfig>> {
            Ok(self
                .channels
                .borrow()
                .iter()
                .find(|c| c.id == channel_id)
                .cloned())
        }

        fn last_sent_at(&self, subject_id: &str) -> CoreResult<Option<i64>> {
            Ok(self.sent_at.borrow().get(subject_id).copied())
        }

        fn topic_priority(&self, topic_id: &str) -> CoreResult<NotifyPriority> {
            Ok(self
                .topics
                .borrow()
                .get(topic_id)
                .copied()
                .unwrap_or(NotifyPriority::Normal))
        }

        fn pending_digestible(
            &self,
            head_event_id: &str,
            window_start: i64,
            now: i64,
        ) -> CoreResult<usize> {
            Ok(self
                .digest_candidates(head_event_id, window_start, now)
                .len())
        }

        fn claim_digest(
            &self,
            head_event_id: &str,
            window_start: i64,
            limit: usize,
            _now: i64,
        ) -> CoreResult<Vec<StoredEvent>> {
            let mut ids = self.digest_candidates(head_event_id, window_start, _now);
            ids.truncate(limit);
            let mut events = self.events.borrow_mut();
            let mut claimed = Vec::new();
            for e in events.iter_mut().filter(|e| ids.contains(&e.id)) {
                e.state = EventState::Digested;
                claimed.push(e.clone());
            }
            Ok(claimed)
        }

        fn promote_to_digest(
            &self,
            event_id: &str,
            digest: &Notification,
            _folded: usize,
            _now: i64,
        ) -> CoreResult<()> {
            let mut events = self.events.borrow_mut();
            let event = events
                .iter_mut()
                .find(|e| e.id == event_id)
                .ok_or_else(|| CoreError::NotFound(event_id.to_string()))?;
            event.kind = digest.kind;
            event.title.clone_from(&digest.title);
            event.body.clone_from(&digest.body);
            event.data = digest.data.clone();
            Ok(())
        }

        fn set_event_state(
            &self,
            event_id: &str,
            state: EventState,
            reason: Option<&str>,
            now: i64,
        ) -> CoreResult<()> {
            let mut events = self.events.borrow_mut();
            let event = events
                .iter_mut()
                .find(|e| e.id == event_id)
                .ok_or_else(|| CoreError::NotFound(event_id.to_string()))?;
            event.state = state;
            if let Some(reason) = reason {
                self.reasons
                    .borrow_mut()
                    .insert(event_id.to_string(), reason.to_string());
            }
            if state == EventState::Sent
                && let Some(subject) = &event.subject_id
            {
                self.sent_at.borrow_mut().insert(subject.clone(), now);
            }
            Ok(())
        }

        fn reschedule_event(&self, event_id: &str, at: i64) -> CoreResult<()> {
            let mut events = self.events.borrow_mut();
            let event = events
                .iter_mut()
                .find(|e| e.id == event_id)
                .ok_or_else(|| CoreError::NotFound(event_id.to_string()))?;
            event.scheduled_at = at;
            Ok(())
        }

        fn record_delivery(
            &self,
            event_id: &str,
            outcome: &ChannelOutcome,
            _now: i64,
        ) -> CoreResult<u32> {
            self.deliveries
                .borrow_mut()
                .push((event_id.to_string(), outcome.clone()));
            let mut attempts = self.attempts.borrow_mut();
            let n = attempts
                .entry((event_id.to_string(), outcome.channel_id.clone()))
                .or_insert(0);
            *n += 1;
            Ok(*n)
        }

        fn note_channel_health(
            &self,
            channel_id: &str,
            ok: bool,
            error: Option<&str>,
            _now: i64,
        ) -> CoreResult<()> {
            let mut health = self.health.borrow_mut();
            let entry = health.entry(channel_id.to_string()).or_insert((0, None));
            if ok {
                *entry = (0, None);
            } else {
                entry.0 += 1;
                entry.1 = error.map(str::to_string);
            }
            Ok(())
        }

        fn failing_feeds(&self, threshold: u32, limit: usize) -> CoreResult<Vec<FailingFeed>> {
            Ok(self
                .failing
                .borrow()
                .iter()
                .filter(|f| f.failure_count >= threshold)
                .take(limit)
                .cloned()
                .collect())
        }

        fn queue_health(&self, _now: i64) -> CoreResult<QueueHealth> {
            Ok(*self.queue_health.borrow())
        }
    }

    /// A [`Transport`] that records requests and replays a scripted response.
    #[derive(Default)]
    struct FakeTransport {
        script: RefCell<Vec<CoreResult<OutboundResponse>>>,
        sent: RefCell<Vec<OutboundRequest>>,
    }

    impl FakeTransport {
        fn scripted(script: Vec<CoreResult<OutboundResponse>>) -> Self {
            Self {
                script: RefCell::new(script),
                sent: RefCell::new(Vec::new()),
            }
        }
        fn sent(&self) -> Vec<OutboundRequest> {
            self.sent.borrow().clone()
        }
    }

    impl Transport for FakeTransport {
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

    /// 2026-01-01T12:00:00Z — the middle of a working day, so the default quiet
    /// window is not in force unless a test asks for it.
    const NOON: i64 = 1_767_225_600 + 12 * 3_600;

    fn ntfy_channel(id: &str) -> ChannelConfig {
        ChannelConfig {
            id: id.into(),
            name: format!("ntfy {id}"),
            kind: ChannelKind::Ntfy,
            target: "argus".into(),
            server: String::new(),
            headers: Vec::new(),
            min_priority: NotifyPriority::Normal,
            events: Vec::new(),
            ntfy_priority: None,
        }
    }

    fn webhook_channel(id: &str, url: &str) -> ChannelConfig {
        ChannelConfig {
            id: id.into(),
            name: format!("webhook {id}"),
            kind: ChannelKind::Webhook,
            target: url.into(),
            server: String::new(),
            headers: Vec::new(),
            min_priority: NotifyPriority::Normal,
            events: Vec::new(),
            ntfy_priority: None,
        }
    }

    fn change(is_new: bool, score: i32) -> StoryChange {
        StoryChange {
            story_id: "story-1".into(),
            is_new,
            previous_summary: if is_new {
                String::new()
            } else {
                "Reuters reported record datacenter revenue this quarter.".into()
            },
            title: "Chip maker posts record quarter".into(),
            summary: "Reuters reported record datacenter revenue this quarter.".into(),
            topic_id: Some("topic-1".into()),
            relevance_score: Some(score),
            article_count: 2,
        }
    }

    fn notify_cfg() -> NotifyConfig {
        NotifyConfig::default()
    }

    /// Run one whole-event notify job with the usual wiring.
    #[allow(clippy::too_many_arguments)]
    fn notify_once(
        transport: &FakeTransport,
        notify: &FakeNotify,
        store: &FakeStore,
        provider: &dyn LlmProvider,
        queue: &RecordingQueue,
        event_id: &str,
        config: &NotifyConfig,
        now: i64,
    ) -> NotifyReport {
        run_notify(
            transport,
            notify,
            store,
            provider,
            queue,
            event_id,
            None,
            None,
            config,
            &BudgetConfig::default(),
            now,
        )
        .unwrap()
    }

    // ---- the trigger -----------------------------------------------------

    #[test]
    fn a_relevant_new_story_records_an_event_and_enqueues_its_dispatch() {
        let notify = FakeNotify::default();
        let queue = RecordingQueue::default();
        let id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .expect("a story at 85 clears the default floor of 70");

        let event = notify.event(&id);
        assert_eq!(event.kind, EventKind::StoryNew);
        assert_eq!(event.priority, NotifyPriority::Normal);
        assert_eq!(event.subject_id.as_deref(), Some("story-1"));
        assert_eq!(event.state, EventState::Pending);
        assert_eq!(queue.ids(Stage::Notify), vec![id]);
    }

    #[test]
    fn a_story_below_the_relevance_floor_notifies_nobody() {
        let notify = FakeNotify::default();
        let queue = RecordingQueue::default();
        assert!(
            notify_story_change(&notify, &queue, &change(true, 40), &notify_cfg(), NOON)
                .unwrap()
                .is_none()
        );
        assert!(notify.events.borrow().is_empty());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn a_high_priority_topic_notifies_whatever_the_score() {
        let notify = FakeNotify::default();
        notify
            .topics
            .borrow_mut()
            .insert("topic-1".into(), NotifyPriority::High);
        let queue = RecordingQueue::default();
        let id = notify_story_change(&notify, &queue, &change(true, 5), &notify_cfg(), NOON)
            .unwrap()
            .expect("a high-priority topic bypasses the floor");
        assert_eq!(notify.event(&id).priority, NotifyPriority::High);
    }

    #[test]
    fn a_redelivered_summarize_records_exactly_one_event() {
        // At-least-once delivery: the same summarize job runs twice. The
        // dedup key is derived from the story and its member count, never from
        // the model's wording, so the second run is a no-op even if the second
        // synthesis worded it differently.
        let notify = FakeNotify::default();
        let queue = RecordingQueue::default();
        let first =
            notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON).unwrap();
        let mut reworded = change(true, 85);
        reworded.summary = "Record datacenter revenue, Reuters reported.".into();
        let second =
            notify_story_change(&notify, &queue, &reworded, &notify_cfg(), NOON + 5).unwrap();

        assert!(first.is_some());
        assert!(
            second.is_none(),
            "the replay must not enqueue a second send"
        );
        assert_eq!(notify.events.borrow().len(), 1);
        assert_eq!(queue.count(Stage::Notify), 1);
    }

    #[test]
    fn a_further_update_is_a_new_event_because_the_member_count_moved() {
        let notify = FakeNotify::default();
        let queue = RecordingQueue::default();
        notify_story_change(&notify, &queue, &change(false, 85), &notify_cfg(), NOON).unwrap();
        let mut grown = change(false, 85);
        grown.article_count = 3;
        assert!(
            notify_story_change(&notify, &queue, &grown, &notify_cfg(), NOON + 60)
                .unwrap()
                .is_some()
        );
        assert_eq!(notify.events.borrow().len(), 2);
    }

    #[test]
    fn summarize_carries_the_previous_narrative_out_before_it_overwrites_it() {
        // The whole reason the trigger reads a StoryChange rather than the row:
        // once record_story_summary runs, the previous narrative is gone.
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let article = seed_article(&store, "https://a.test/1", "Headline", "feed-1", NOON);
        let story = store
            .create_story(
                &StorySeed {
                    title: "Developing story".into(),
                    topic_id: Some("topic-1".into()),
                    centroid: vec![1.0, 0.0],
                    recipe: "lex-v1/2".into(),
                    published_at: Some(NOON),
                    relevance_score: Some(85),
                },
                &article,
                NOON,
            )
            .unwrap();
        // A first synthesis, so the run under test is an *update* and has a
        // previous narrative to lose.
        store
            .record_story_summary(
                &story,
                &StorySummary {
                    title: "Old title".into(),
                    summary: "The old narrative.".into(),
                },
                &[],
                NOON,
            )
            .unwrap();

        let provider = MockProvider::chat_once(
            r#"{"title": "New title", "summary": "The new narrative, materially different."}"#,
        );
        let report = run_summarize(
            &provider,
            &store,
            &store,
            &queue,
            &story,
            None,
            &cfg(),
            NOON + 10_000,
        )
        .unwrap();

        let change = report.change.expect("a summarize reports its change");
        assert!(!change.is_new, "the story had been summarized before");
        assert_eq!(change.previous_summary, "The old narrative.");
        assert_eq!(change.summary, "The new narrative, materially different.");
        assert_eq!(change.title, "New title");
    }

    // ---- the whole chain -------------------------------------------------

    #[test]
    fn the_whole_notify_chain_delivers_one_story_to_every_channel() {
        // The core's end-to-end: a summarize produces a change, the change
        // becomes an outbox event, the event dispatches, and every channel gets
        // the payload its own renderer built.
        let notify = FakeNotify::with_channels(vec![
            ntfy_channel("chan-ntfy"),
            webhook_channel("chan-hook", "https://ops.example/hook"),
        ]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.delivered, 2);
        assert_eq!(report.failed + report.blocked + report.skipped, 0);
        assert_eq!(notify.event(&event_id).state, EventState::Sent);

        let sent = transport.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].url, "https://ntfy.sh/argus");
        let ntfy: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
        assert_eq!(ntfy["title"], "Chip maker posts record quarter");
        assert_eq!(sent[1].url, "https://ops.example/hook");
        let hook: serde_json::Value = serde_json::from_str(&sent[1].body).unwrap();
        assert_eq!(hook["event"], "story.new");
        assert_eq!(hook["data"]["article_count"], 2);

        // Every channel has a delivery row carrying exactly what it was sent.
        let rows = notify.deliveries_for(&event_id);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.state == DeliveryState::Delivered));
        assert_eq!(rows[1].request.as_ref().unwrap().body, sent[1].body);
    }

    #[test]
    fn an_already_sent_event_is_never_dispatched_twice() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();
        let cfg = notify_cfg();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &cfg, NOON)
            .unwrap()
            .unwrap();
        notify_once(
            &transport, &notify, &store, &provider, &queue, &event_id, &cfg, NOON,
        );
        let replay = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &cfg,
            NOON + 1,
        );

        assert!(replay.already_handled);
        assert_eq!(transport.sent().len(), 1, "the replay sent nothing");
    }

    #[test]
    fn a_missing_event_is_a_clean_no_op() {
        let notify = FakeNotify::default();
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            "evt-gone",
            &notify_cfg(),
            NOON,
        );
        assert!(report.missing);
        assert_eq!(queue.len(), 0);
    }

    // ---- the change judge ------------------------------------------------

    /// Record a story-update event and return its id.
    fn seed_update(notify: &FakeNotify, queue: &RecordingQueue, now: i64) -> String {
        let mut updated = change(false, 85);
        updated.summary =
            "The chip maker withdrew guidance after regulators opened an investigation.".into();
        notify_story_change(notify, queue, &updated, &notify_cfg(), now)
            .unwrap()
            .expect("an 85 update qualifies")
    }

    #[test]
    fn an_update_the_judge_calls_immaterial_is_suppressed_and_costs_its_call() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = MockProvider::chat_once(r#"{"material": false, "reason": "reworded"}"#)
            .with_chat_cost(0.0004);

        let event_id = seed_update(&notify, &queue, NOON);
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.material, Some(false));
        assert!(report.suppressed.unwrap().contains("reworded"));
        assert!(transport.sent().is_empty(), "nobody was told");
        assert_eq!(notify.event(&event_id).state, EventState::Suppressed);
        // The call was made, so it is counted — notification spend is spend.
        let spend = store.load_daily_spend(&utc_day(NOON)).unwrap();
        assert_eq!(spend.calls, 1);
        assert!((spend.spent_usd - 0.0004).abs() < 1e-9);
    }

    #[test]
    fn an_update_the_judge_calls_material_is_sent() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider =
            MockProvider::chat_once(r#"{"material": true, "reason": "guidance withdrawn"}"#);

        let event_id = seed_update(&notify, &queue, NOON);
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.material, Some(true));
        assert_eq!(report.delivered, 1);
        assert_eq!(notify.event(&event_id).state, EventState::Sent);
    }

    #[test]
    fn a_new_story_never_calls_the_judge() {
        // A story that did not exist before is unambiguously new; spending a
        // call to establish that would be spending for nothing.
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = MockProvider::chat_once(r#"{"material": false}"#);

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.material, None);
        assert_eq!(provider.chat_calls(), 0);
        assert_eq!(report.delivered, 1);
    }

    #[test]
    fn the_judge_can_be_switched_off_for_the_deterministic_fallback() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = MockProvider::chat_once(r#"{"material": false}"#);
        let config = NotifyConfig {
            judge_enabled: false,
            ..notify_cfg()
        };

        let event_id = seed_update(&notify, &queue, NOON);
        let report = notify_once(
            &transport, &notify, &store, &provider, &queue, &event_id, &config, NOON,
        );

        assert_eq!(provider.chat_calls(), 0, "no call was made");
        assert_eq!(report.material, Some(true), "the texts really did diverge");
        assert!(report.judge_reason.contains("judge off"));
        assert_eq!(store.load_daily_spend(&utc_day(NOON)).unwrap().calls, 0);
    }

    #[test]
    fn an_unparseable_judge_answer_falls_back_rather_than_losing_the_story() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = MockProvider::chat_once("I'm not sure what you mean.");

        let event_id = seed_update(&notify, &queue, NOON);
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.material, Some(true), "the fallback decided");
        assert_eq!(report.delivered, 1);
        // The call was made and could not be priced from an unusable answer, so
        // it is counted as unpriced rather than as free.
        let spend = store.load_daily_spend(&utc_day(NOON)).unwrap();
        assert_eq!((spend.calls, spend.unpriced_calls), (1, 1));
    }

    #[test]
    fn a_transient_judge_failure_propagates_for_retry() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = MockProvider::new(vec![crate::provider::Scripted::ChatError("503".into())]);

        let event_id = seed_update(&notify, &queue, NOON);
        let err = run_notify(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            None,
            None,
            &notify_cfg(),
            &BudgetConfig::default(),
            NOON,
        )
        .unwrap_err();

        assert!(err.is_transient());
        assert!(transport.sent().is_empty());
        assert_eq!(
            notify.event(&event_id).state,
            EventState::Pending,
            "the event stays claimable for the retry"
        );
    }

    #[test]
    fn a_budget_pause_defers_the_judge_to_the_next_day_without_notifying() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = MockProvider::chat_once(r#"{"material": true}"#);
        store
            .record_cost(&utc_day(NOON), Stage::Analyze, Some(10.0), NOON)
            .unwrap();

        let event_id = seed_update(&notify, &queue, NOON);
        let report = run_notify(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            None,
            None,
            &notify_cfg(),
            &BudgetConfig {
                daily_limit_usd: 5.0,
                alert_threshold_usd: 1.0,
            },
            NOON,
        )
        .unwrap();

        assert!(report.paused);
        assert_eq!(provider.chat_calls(), 0);
        assert!(transport.sent().is_empty());
        // Deferred into tomorrow rather than failed: a paused job that panicked
        // would burn its attempts against a limit that will not move for hours.
        let delay = queue.last_delay(Stage::Notify).unwrap();
        assert_eq!(utc_day(NOON + delay), "2026-01-02");
    }

    // ---- rate limiting ---------------------------------------------------

    #[test]
    fn a_second_story_about_the_same_subject_inside_the_window_is_suppressed() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = MockProvider::chat_once(r#"{"material": true}"#);
        let cfg = notify_cfg();

        let first = notify_story_change(&notify, &queue, &change(true, 85), &cfg, NOON)
            .unwrap()
            .unwrap();
        notify_once(
            &transport, &notify, &store, &provider, &queue, &first, &cfg, NOON,
        );

        let second = seed_update(&notify, &queue, NOON + 60);
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &second,
            &cfg,
            NOON + 60,
        );

        assert!(report.suppressed.unwrap().contains("debounced"));
        assert_eq!(transport.sent().len(), 1, "only the first went out");
        assert_eq!(notify.event(&second).state, EventState::Suppressed);
    }

    #[test]
    fn quiet_hours_reschedule_the_row_and_re_enqueue_the_job() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();
        let cfg = notify_cfg();
        // 2026-01-01T01:00:00Z, inside the default 23:00-07:00 window.
        let night = 1_767_225_600 + 3_600;

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &cfg, night)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport, &notify, &store, &provider, &queue, &event_id, &cfg, night,
        );

        assert!(report.deferred_seconds > 0);
        assert!(transport.sent().is_empty());
        let event = notify.event(&event_id);
        assert_eq!(event.state, EventState::Pending);
        assert_eq!(event.scheduled_at, 1_767_225_600 + 7 * 3_600);

        // At the rescheduled instant it goes out.
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &cfg,
            event.scheduled_at,
        );
        assert_eq!(report.delivered, 1);
    }

    #[test]
    fn a_high_priority_story_goes_out_during_quiet_hours() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        notify
            .topics
            .borrow_mut()
            .insert("topic-1".into(), NotifyPriority::High);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();
        let night = 1_767_225_600 + 3_600;

        let event_id = notify_story_change(&notify, &queue, &change(true, 5), &notify_cfg(), night)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            night,
        );
        assert_eq!(report.delivered, 1);
    }

    #[test]
    fn a_job_whose_scheduled_time_has_not_arrived_re_enqueues_itself() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        notify.reschedule_event(&event_id, NOON + 600).unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.deferred_seconds, 600);
        assert!(transport.sent().is_empty());
    }

    // ---- digest ----------------------------------------------------------

    /// Record `n` qualifying new-story events, one per distinct story.
    fn seed_stories(
        notify: &FakeNotify,
        queue: &RecordingQueue,
        n: usize,
        now: i64,
    ) -> Vec<String> {
        (0..n)
            .map(|i| {
                let mut c = change(true, 85);
                c.story_id = format!("story-{i}");
                c.title = format!("Story {i}");
                notify_story_change(notify, queue, &c, &notify_cfg(), now)
                    .unwrap()
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn five_due_stories_collapse_into_one_digest() {
        let notify = FakeNotify::with_channels(vec![webhook_channel(
            "chan-hook",
            "https://ops.example/hook",
        )]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        let ids = seed_stories(&notify, &queue, 5, NOON);
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &ids[0],
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.digested, 4, "the other four folded in");
        assert_eq!(report.delivered, 1, "one message, not five");
        assert_eq!(transport.sent().len(), 1);

        let body: serde_json::Value = serde_json::from_str(&transport.sent()[0].body).unwrap();
        assert_eq!(body["event"], "story.digest");
        assert_eq!(body["title"], "5 new stories");
        assert_eq!(body["data"]["count"], 5);
        for i in 0..5 {
            assert!(
                body["body"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("Story {i}")),
                "digest body must name every story: {body}"
            );
        }

        // The head row now *is* the digest, so a retry or an overflow channel
        // reloads the digest rather than the story it started as.
        assert_eq!(notify.event(&ids[0]).kind, EventKind::StoryDigest);
        assert_eq!(
            notify
                .states()
                .iter()
                .filter(|s| **s == EventState::Digested)
                .count(),
            4
        );
    }

    #[test]
    fn four_due_stories_stay_four_notifications() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();
        // Debouncing is per subject and these are four different stories, so
        // nothing else suppresses them.
        let ids = seed_stories(&notify, &queue, 4, NOON);

        for id in &ids {
            let report = notify_once(
                &transport,
                &notify,
                &store,
                &provider,
                &queue,
                id,
                &notify_cfg(),
                NOON,
            );
            assert_eq!(report.digested, 0);
        }
        assert_eq!(transport.sent().len(), 4);
        assert!(notify.states().iter().all(|s| *s == EventState::Sent));
    }

    #[test]
    fn a_spike_past_the_fold_cap_counts_the_remainder_instead_of_naming_it() {
        let notify =
            FakeNotify::with_channels(vec![webhook_channel("c", "https://ops.example/hook")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        let ids = seed_stories(&notify, &queue, 12, NOON);
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &ids[0],
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.digested, 11);
        let body: serde_json::Value = serde_json::from_str(&transport.sent()[0].body).unwrap();
        let text = body["body"].as_str().unwrap();
        assert!(text.contains("and 4 more"), "12 stories, 8 named: {text}");
        assert_eq!(body["data"]["count"], 12);
    }

    #[test]
    fn digesting_can_be_switched_off() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();
        let config = NotifyConfig {
            digest_threshold: 0,
            ..notify_cfg()
        };

        let ids = seed_stories(&notify, &queue, 6, NOON);
        let report = notify_once(
            &transport, &notify, &store, &provider, &queue, &ids[0], &config, NOON,
        );
        assert_eq!(report.digested, 0);
        assert_eq!(report.delivered, 1);
    }

    // ---- per-channel isolation, retry and blocking -----------------------

    #[test]
    fn one_failing_channel_neither_blocks_the_others_nor_fails_the_job() {
        let notify = FakeNotify::with_channels(vec![
            ntfy_channel("chan-ok"),
            webhook_channel("chan-down", "https://down.example/hook"),
            webhook_channel("chan-blocked", "http://127.0.0.1:9/hook"),
        ]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::scripted(vec![
            Ok(OutboundResponse {
                status: 200,
                body: String::new(),
            }),
            Ok(OutboundResponse {
                status: 503,
                body: "down".into(),
            }),
            Err(CoreError::FetchRefused("blocked or invalid URL".into())),
        ]);
        let provider = embed_unused();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!((report.delivered, report.failed, report.blocked), (1, 1, 1));
        assert_eq!(notify.event(&event_id).state, EventState::Sent);
        // Only the transient failure earned a retry; the blocked one did not.
        let retries: Vec<Option<String>> = queue
            .jobs
            .borrow()
            .iter()
            .filter(|(j, _)| j.stage == Stage::Notify && j.channel.is_some())
            .map(|(j, _)| j.channel.clone())
            .collect();
        assert_eq!(retries, vec![Some("chan-down".to_string())]);
        assert_eq!(report.requeued, 1);
    }

    #[test]
    fn an_ssrf_blocked_channel_records_the_payload_it_would_have_sent() {
        // The scope's explicit requirement, and the shape the kernel end-to-end
        // test asserts against: a blocked target is a per-channel error whose
        // rendered payload is still on the delivery row.
        let notify = FakeNotify::with_channels(vec![webhook_channel(
            "chan-local",
            "http://localhost:8080/hook",
        )]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::scripted(vec![Err(CoreError::FetchRefused(
            "blocked or invalid URL (host -32)".into(),
        ))]);
        let provider = embed_unused();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.blocked, 1);
        assert_eq!(report.requeued, 0, "a blocked target is never retried");
        let row = notify.deliveries_for(&event_id).remove(0);
        assert_eq!(row.state, DeliveryState::Blocked);
        assert!(row.error.unwrap().contains("blocked"));
        let payload: serde_json::Value = serde_json::from_str(&row.request.unwrap().body).unwrap();
        assert_eq!(payload["event"], "story.new");
        assert_eq!(payload["title"], "Chip maker posts record quarter");
        // The channel's health carries the reason an operator needs.
        assert_eq!(notify.health.borrow()["chan-local"].0, 1);
    }

    #[test]
    fn a_channel_scoped_retry_skips_every_gate_and_backs_off() {
        let notify = FakeNotify::with_channels(vec![webhook_channel(
            "chan-down",
            "https://down.example/hook",
        )]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let provider = embed_unused();
        let config = notify_cfg();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &config, NOON)
            .unwrap()
            .unwrap();

        // Attempt 1 through the whole-event path, then channel-scoped retries
        // until the attempt budget is exhausted.
        for attempt in 0..6 {
            let transport = FakeTransport::scripted(vec![Ok(OutboundResponse {
                status: 500,
                body: String::new(),
            })]);
            let channel = if attempt == 0 {
                None
            } else {
                Some("chan-down")
            };
            let report = run_notify(
                &transport,
                &notify,
                &store,
                &provider,
                &queue,
                &event_id,
                channel,
                None,
                &config,
                &BudgetConfig::default(),
                NOON + attempt,
            )
            .unwrap();
            if attempt < 5 {
                assert_eq!(report.failed, 1, "attempt {attempt}");
                assert_eq!(transport.sent().len(), 1, "attempt {attempt} was sent");
            }
        }

        // Four retries were scheduled, with doubling delays; the sixth attempt
        // found no budget left and scheduled nothing new.
        let retry_delays: Vec<i64> = queue
            .jobs
            .borrow()
            .iter()
            .filter(|(j, _)| j.channel.as_deref() == Some("chan-down"))
            .map(|(_, o)| o.delay)
            .collect();
        assert_eq!(retry_delays, vec![60, 120, 240, 480]);
    }

    #[test]
    fn a_channel_disabled_between_enqueue_and_retry_stops_cleanly() {
        let notify = FakeNotify::with_channels(vec![ntfy_channel("chan-ntfy")]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        notify.channels.borrow_mut().clear();

        let report = run_notify(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            Some("chan-ntfy"),
            None,
            &notify_cfg(),
            &BudgetConfig::default(),
            NOON,
        )
        .unwrap();

        assert!(report.suppressed.unwrap().contains("no longer enabled"));
        assert!(transport.sent().is_empty());
    }

    #[test]
    fn channels_past_the_dispatch_cap_get_their_own_jobs() {
        let channels: Vec<ChannelConfig> = (0..11)
            .map(|i| webhook_channel(&format!("c{i}"), "https://ops.example/hook"))
            .collect();
        let notify = FakeNotify::with_channels(channels);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(
            report.delivered,
            crate::ratelimit::MAX_CHANNELS_PER_DISPATCH
        );
        assert_eq!(report.requeued, 3);
        assert_eq!(
            transport.sent().len(),
            crate::ratelimit::MAX_CHANNELS_PER_DISPATCH,
            "one job never makes more POSTs than the cap"
        );
        let overflow: Vec<String> = queue
            .jobs
            .borrow()
            .iter()
            .filter_map(|(j, _)| j.channel.clone())
            .collect();
        assert_eq!(overflow, vec!["c8", "c9", "c10"]);
    }

    #[test]
    fn an_event_no_channel_wants_is_still_marked_sent() {
        // Skipped is not failed: the channels were asked and declined, so the
        // event is done rather than left to be retried forever.
        let mut picky = ntfy_channel("chan-alerts");
        picky.events = vec![EventKind::BudgetThreshold];
        let notify = FakeNotify::with_channels(vec![picky]);
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        let event_id = notify_story_change(&notify, &queue, &change(true, 85), &notify_cfg(), NOON)
            .unwrap()
            .unwrap();
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!((report.skipped, report.delivered), (1, 0));
        assert!(transport.sent().is_empty());
        assert_eq!(notify.event(&event_id).state, EventState::Sent);
    }

    // ---- operator alerts -------------------------------------------------

    fn failing_feed(id: &str, count: u32) -> FailingFeed {
        FailingFeed {
            id: id.into(),
            name: format!("Feed {id}"),
            failure_count: count,
            last_error: "HTTP 503".into(),
        }
    }

    #[test]
    fn the_alert_pass_records_a_feed_a_budget_and_a_queue_alert() {
        let notify = FakeNotify::default();
        *notify.failing.borrow_mut() = vec![failing_feed("feed-1", 4), failing_feed("feed-2", 3)];
        *notify.queue_health.borrow_mut() = QueueHealth {
            oldest_ready_age: 4_000,
            ready: 120,
            dead: 2,
        };
        let queue = RecordingQueue::default();
        let spend = DailySpend {
            spent_usd: 12.0,
            calls: 400,
            unpriced_calls: 0,
        };
        let budget = BudgetConfig {
            daily_limit_usd: 10.0,
            alert_threshold_usd: 5.0,
        };

        let report = run_alerts(&notify, &queue, &spend, &budget, &notify_cfg(), NOON).unwrap();

        assert_eq!(report.feeds_failing, 2);
        assert!(report.budget_alerted);
        assert!(report.queue_alerted);
        assert_eq!(report.events_recorded, 4);
        assert_eq!(queue.count(Stage::Notify), 4);

        let kinds: Vec<EventKind> = notify.events.borrow().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::FeedFailing,
                EventKind::FeedFailing,
                EventKind::BudgetThreshold,
                EventKind::QueueStuck
            ]
        );
        // A budget *pause* and a stuck queue are the two conditions that stop
        // work, so both are loud enough to bypass quiet hours.
        let priorities: Vec<NotifyPriority> =
            notify.events.borrow().iter().map(|e| e.priority).collect();
        assert_eq!(priorities[2], NotifyPriority::High);
        assert_eq!(priorities[3], NotifyPriority::High);
    }

    #[test]
    fn a_persistent_condition_does_not_re_alert_every_cron_cycle() {
        let notify = FakeNotify::default();
        *notify.failing.borrow_mut() = vec![failing_feed("feed-1", 4)];
        *notify.queue_health.borrow_mut() = QueueHealth {
            oldest_ready_age: 4_000,
            ready: 10,
            dead: 0,
        };
        let queue = RecordingQueue::default();
        let spend = DailySpend {
            spent_usd: 12.0,
            calls: 1,
            unpriced_calls: 0,
        };
        let budget = BudgetConfig {
            daily_limit_usd: 10.0,
            alert_threshold_usd: 5.0,
        };

        let first = run_alerts(&notify, &queue, &spend, &budget, &notify_cfg(), NOON).unwrap();
        // A minute later, nothing has changed.
        let second =
            run_alerts(&notify, &queue, &spend, &budget, &notify_cfg(), NOON + 60).unwrap();

        assert_eq!(first.events_recorded, 3);
        assert_eq!(second.events_recorded, 0, "every key was already known");
        assert_eq!(notify.events.borrow().len(), 3);
    }

    #[test]
    fn a_feed_that_fails_again_is_a_new_alert() {
        let notify = FakeNotify::default();
        *notify.failing.borrow_mut() = vec![failing_feed("feed-1", 3)];
        let queue = RecordingQueue::default();
        let spend = DailySpend::default();
        let budget = BudgetConfig::default();

        run_alerts(&notify, &queue, &spend, &budget, &notify_cfg(), NOON).unwrap();
        *notify.failing.borrow_mut() = vec![failing_feed("feed-1", 4)];
        let second =
            run_alerts(&notify, &queue, &spend, &budget, &notify_cfg(), NOON + 900).unwrap();

        assert_eq!(second.events_recorded, 1);
        assert_eq!(notify.events.borrow().len(), 2);
    }

    #[test]
    fn a_healthy_pipeline_records_no_alerts() {
        let notify = FakeNotify::default();
        let queue = RecordingQueue::default();
        let report = run_alerts(
            &notify,
            &queue,
            &DailySpend::default(),
            &BudgetConfig::default(),
            &notify_cfg(),
            NOON,
        )
        .unwrap();
        assert_eq!(report, AlertReport::default());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn the_alert_pass_can_be_switched_off_entirely() {
        let notify = FakeNotify::default();
        *notify.failing.borrow_mut() = vec![failing_feed("feed-1", 9)];
        *notify.queue_health.borrow_mut() = QueueHealth {
            oldest_ready_age: 99_999,
            ready: 1,
            dead: 1,
        };
        let queue = RecordingQueue::default();
        let config = NotifyConfig {
            alerts_enabled: false,
            ..notify_cfg()
        };
        let report = run_alerts(
            &notify,
            &queue,
            &DailySpend::default(),
            &BudgetConfig::default(),
            &config,
            NOON,
        )
        .unwrap();
        assert_eq!(report, AlertReport::default());
        assert!(notify.events.borrow().is_empty());
    }

    #[test]
    fn a_queue_below_the_stuck_threshold_is_not_alerted_on() {
        let notify = FakeNotify::default();
        *notify.queue_health.borrow_mut() = QueueHealth {
            oldest_ready_age: 899,
            ready: 5,
            dead: 0,
        };
        let queue = RecordingQueue::default();
        let report = run_alerts(
            &notify,
            &queue,
            &DailySpend::default(),
            &BudgetConfig::default(),
            &notify_cfg(),
            NOON,
        )
        .unwrap();
        assert!(!report.queue_alerted);
    }

    #[test]
    fn an_alert_dispatches_through_the_same_worker_as_a_story() {
        let notify = FakeNotify::with_channels(vec![webhook_channel(
            "chan-ops",
            "https://ops.example/hook",
        )]);
        *notify.queue_health.borrow_mut() = QueueHealth {
            oldest_ready_age: 4_000,
            ready: 99,
            dead: 3,
        };
        let store = FakeStore::default();
        let queue = RecordingQueue::default();
        let transport = FakeTransport::default();
        let provider = embed_unused();

        run_alerts(
            &notify,
            &queue,
            &DailySpend::default(),
            &BudgetConfig::default(),
            &notify_cfg(),
            NOON,
        )
        .unwrap();
        let event_id = queue.ids(Stage::Notify).remove(0);
        let report = notify_once(
            &transport,
            &notify,
            &store,
            &provider,
            &queue,
            &event_id,
            &notify_cfg(),
            NOON,
        );

        assert_eq!(report.delivered, 1);
        let body: serde_json::Value = serde_json::from_str(&transport.sent()[0].body).unwrap();
        assert_eq!(body["event"], "alert.queue_stuck");
        assert_eq!(body["priority"], "high");
        assert_eq!(body["data"]["dead"], 3);
    }
}
