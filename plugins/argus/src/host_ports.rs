//! Implementations of the `argus_core` ports over Trovato kernel host functions.
//!
//! The pure pipeline in `argus-core` never names a host function; these adapters
//! do. Each type wraps one host surface:
//!
//! - [`HostProvider`] → `ai-request` (the background AI principal).
//! - [`HostFetcher`] → the streaming `http-open`/`http-read`/`http-close` host
//!   for feeds, with conditional GET on the stream (M1-5).
//! - [`HostStore`] → `db` host (`query-raw` / `execute-raw`) against the
//!   `argus_*` tables, plus [`crate::config_host`] for the feed and topic
//!   *configuration* that lives on Items from M3 on.
//! - [`HostQueue`] → `queue` host (`enqueue`).
//!
//! ## Feed fetch uses the streaming host with conditional GET (M1-5)
//!
//! `http-open` returns the response `status` and `headers` alongside the
//! streaming handle (p11j / G-HTTP-META), so conditional GET works on the
//! streaming path: [`HostFetcher::fetch`] replays the stored `ETag`/
//! `Last-Modified`, reads the status from the open metadata to short-circuit a
//! `304 Not Modified` (whose stream is at immediate EOF), and otherwise reads a
//! fresh `ETag`/`Last-Modified` back before streaming the body in 64 KB chunks up
//! to the manifest transfer ceiling (16 MB). Small feeds and multi-MB article
//! bodies both take the one path; no separate one-shot fallback is needed.

use argus_core::error::{CoreError, CoreResult};
use argus_core::ports::{
    ChatRequest, ChatResponse, ConditionalHeaders, DecideContext, EmbedResponse, EnqueueOpts, Feed,
    FeedSchedule, FetchOutcome, Fetcher, JobPayload, JobQueue, LlmProvider, NewArticle, Store,
    UpsertResult, Usage,
};
use serde::Deserialize;
use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::types::{
    AiMessage, AiOperationType, AiRequest, AiRequestOptions, HttpRequest, QueueOptions,
};

use crate::config_host;

/// User-Agent sent on every outbound feed fetch.
const USER_AGENT: &str = "Argus/1.0 (Trovato news intelligence)";

// ===========================================================================
// Error mapping
// ===========================================================================

/// Map an `ai-request` host error code to a transient provider error.
///
/// Every AI failure mode (no provider, request failed, rate limited, auth,
/// background-denied) is treated as transient so a decide job retries; a
/// persistently misconfigured provider then dead-letters via queue v2 rather
/// than silently discarding an article.
fn map_ai_err(code: i32) -> CoreError {
    CoreError::Provider(format!("ai-request host error {code}"))
}

/// Map an HTTP host error code to fetch outcome.
///
/// `ERR_HTTP_INVALID_URL` (-32) is the SSRF fence / malformed-URL code and is
/// *permanent* (a blocked or bad URL will not become valid on retry), so it
/// maps to [`CoreError::FetchRefused`]. Timeouts and connection failures are
/// transient. `RESPONSE_TOO_LARGE` (-33) is permanent for a given feed.
pub(crate) fn map_http_err(code: i32) -> CoreError {
    use trovato_sdk::host_errors::{
        ERR_HTTP_INVALID_URL, ERR_HTTP_RESPONSE_TOO_LARGE, ERR_HTTP_TRANSFER_BUDGET,
    };
    match code {
        c if c == ERR_HTTP_INVALID_URL => {
            CoreError::FetchRefused(format!("blocked or invalid URL (host {c})"))
        }
        c if c == ERR_HTTP_RESPONSE_TOO_LARGE || c == ERR_HTTP_TRANSFER_BUDGET => {
            CoreError::FetchRefused(format!("response too large (host {c})"))
        }
        c => CoreError::Fetch(format!("http host error {c}")),
    }
}

/// Map a `db` host error code to a transient store error.
fn map_db_err(code: i32) -> CoreError {
    CoreError::Store(format!("db host error {code}"))
}

/// Case-insensitive header lookup over the response header map.
fn header<'a>(
    headers: &'a std::collections::HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// ===========================================================================
// Provider
// ===========================================================================

/// The background-AI-backed [`LlmProvider`].
pub struct HostProvider;

impl LlmProvider for HostProvider {
    fn chat(&self, req: &ChatRequest) -> CoreResult<ChatResponse> {
        let mut messages = Vec::new();
        if let Some(system) = &req.system {
            messages.push(AiMessage::system(system.clone()));
        }
        messages.push(AiMessage::user(req.user.clone()));
        let ai = AiRequest {
            operation: AiOperationType::Chat,
            provider_id: None,
            model: req.model.clone(),
            messages,
            input: None,
            options: AiRequestOptions {
                max_tokens: req.max_tokens,
                ..Default::default()
            },
        };
        let resp = host::ai_request(&ai).map_err(map_ai_err)?;
        Ok(ChatResponse {
            content: resp.content,
            model: resp.model,
            usage: Usage {
                prompt_tokens: resp.usage.prompt_tokens,
                completion_tokens: resp.usage.completion_tokens,
                total_tokens: resp.usage.total_tokens,
            },
            // Cost read from the response (G-COST-OPAQUE fixed by p11j), not a
            // kernel-side SQL query.
            cost_estimate: resp.cost_estimate,
        })
    }

    fn embed(&self, input: &str, model: Option<&str>) -> CoreResult<EmbedResponse> {
        // Not exercised in M1 (embed is a stub stage). The kernel returns the
        // embedding as JSON text in `content`; parse it defensively.
        let ai = AiRequest {
            operation: AiOperationType::Embedding,
            provider_id: None,
            model: model.map(str::to_string),
            messages: Vec::new(),
            input: Some(input.to_string()),
            options: AiRequestOptions::default(),
        };
        let resp = host::ai_request(&ai).map_err(map_ai_err)?;
        let vector: Vec<f32> = serde_json::from_str(&resp.content)
            .map_err(|e| CoreError::Provider(format!("embedding not a float array: {e}")))?;
        Ok(EmbedResponse {
            vector,
            model: resp.model,
            usage: Usage {
                prompt_tokens: resp.usage.prompt_tokens,
                completion_tokens: resp.usage.completion_tokens,
                total_tokens: resp.usage.total_tokens,
            },
        })
    }
}

// ===========================================================================
// Fetcher
// ===========================================================================

/// The streaming-`http-open`-backed [`Fetcher`] with conditional GET (see module
/// note).
pub struct HostFetcher;

impl Fetcher for HostFetcher {
    fn fetch(&self, url: &str, cond: &ConditionalHeaders) -> CoreResult<FetchOutcome> {
        let mut req = HttpRequest::get(url).header("User-Agent", USER_AGENT);
        if let Some(etag) = &cond.etag {
            req = req.header("If-None-Match", etag.clone());
        }
        if let Some(lm) = &cond.last_modified {
            req = req.header("If-Modified-Since", lm.clone());
        }
        // Open the stream: `http-open` returns the response status + headers
        // alongside the handle (p11j / G-HTTP-META), so conditional GET works on
        // the streaming path. A blocked/malformed URL fails here with the same
        // SSRF/URL codes as one-shot `request`.
        let opened = host::http_open(&req).map_err(map_http_err)?;
        let handle = opened.handle;
        match opened.status {
            // 304: nothing changed. The stream has no body (immediate EOF); close
            // it and short-circuit without reading.
            304 => {
                let _ = host::http_close(handle);
                Ok(FetchOutcome::NotModified)
            }
            200..=299 => {
                let etag = header(&opened.headers, "etag").map(str::to_string);
                let last_modified = header(&opened.headers, "last-modified").map(str::to_string);
                let body = read_to_end(handle)?;
                Ok(FetchOutcome::Fetched {
                    etag,
                    last_modified,
                    body,
                })
            }
            // 429 and 5xx are transient (retry with backoff); other 4xx are
            // permanent (flag the feed, do not retry-storm). Drain nothing; close.
            429 | 500..=599 => {
                let _ = host::http_close(handle);
                Err(CoreError::Fetch(format!("HTTP {}", opened.status)))
            }
            s => {
                let _ = host::http_close(handle);
                Err(CoreError::FetchRefused(format!("HTTP {s}")))
            }
        }
    }
}

/// Drain an open streaming handle to end-of-body (`http-read` in 64 KB
/// chunks/`http-close`), reassembling the bytes into a UTF-8 string.
///
/// The transfer is bounded by the manifest total-transfer ceiling (up to 16 MB),
/// enforced kernel-side; this handles both small feeds and multi-MB article
/// bodies on the one streaming path.
///
/// # Errors
///
/// Maps host HTTP error codes via [`map_http_err`]; a non-UTF-8 body is a
/// transient [`CoreError::Fetch`]. Always closes the handle.
fn read_to_end(handle: u32) -> CoreResult<String> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match host::http_read(handle) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(code) => {
                // Best-effort close; the read error is what we report.
                let _ = host::http_close(handle);
                return Err(map_http_err(code));
            }
        }
    }
    let _ = host::http_close(handle);
    String::from_utf8(buf).map_err(|e| CoreError::Fetch(format!("non-UTF-8 body: {e}")))
}

// ===========================================================================
// Queue
// ===========================================================================

/// The `queue`-host-backed [`JobQueue`].
pub struct HostQueue;

impl JobQueue for HostQueue {
    fn enqueue(&self, job: &JobPayload, opts: EnqueueOpts) -> CoreResult<()> {
        let payload = serde_json::to_value(job)
            .map_err(|e| CoreError::Queue(format!("serialize job: {e}")))?;
        host::queue_enqueue(
            job.stage.queue_name(),
            &payload,
            &QueueOptions {
                priority: opts.priority,
                delay: opts.delay,
            },
        )
        .map_err(|c| CoreError::Queue(format!("enqueue host error {c}")))
    }
}

// ===========================================================================
// Store
// ===========================================================================

/// The `db`-host-backed [`Store`], operating on the `argus_*` tables.
pub struct HostStore;

/// Run a `SELECT` and deserialize its rows into `T`.
///
/// # Errors
///
/// Maps a `db` host error code to a transient [`CoreError::Store`], and a row
/// shape that does not match `T` to a store decode error.
pub fn query_rows<T: for<'de> Deserialize<'de>>(sql: &str, params: &[Value]) -> CoreResult<Vec<T>> {
    let json = host::query_raw(sql, params).map_err(map_db_err)?;
    serde_json::from_str(&json).map_err(|e| CoreError::Store(format!("row decode: {e}")))
}

/// Run a DML statement, returning rows affected.
///
/// # Errors
///
/// Maps a `db` host error code to a transient [`CoreError::Store`].
pub fn exec(sql: &str, params: &[Value]) -> CoreResult<u64> {
    host::execute_raw(sql, params).map_err(map_db_err)
}

/// Bind a nullable string as a JSON param.
fn opt_str(s: &Option<String>) -> Value {
    match s {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

/// Bind a nullable i64 as a JSON param.
fn opt_i64(v: Option<i64>) -> Value {
    match v {
        Some(n) => json!(n),
        None => Value::Null,
    }
}

/// The conditional-GET validators from a feed's state row (M3: state only —
/// its configuration comes from the feed Item).
#[derive(Deserialize)]
struct FeedStateRow {
    etag: Option<String>,
    last_modified: Option<String>,
}

/// The scheduling half of a feed's state row.
#[derive(Deserialize)]
struct FeedScheduleRow {
    id: String,
    last_fetched_at: Option<i64>,
}

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Deserialize)]
struct DecideRow {
    title: String,
    content: String,
    topic_id: String,
}

#[derive(Deserialize)]
struct ValueRow {
    value: String,
}

impl Store for HostStore {
    /// Configuration comes from the feed's Item, fetch state from the row keyed
    /// by that Item's id (`M3-DESIGN.md` Decision 2). Two reads where M1 had
    /// one; the feed set is small and this runs once per fetch job.
    fn load_feed(&self, feed_id: &str) -> CoreResult<Option<Feed>> {
        let Some(config) = config_host::load_feed_config(feed_id)? else {
            return Ok(None);
        };
        let rows: Vec<FeedStateRow> = query_rows(
            "SELECT etag, last_modified FROM argus_feeds WHERE id = $1::uuid",
            &[json!(feed_id)],
        )?;
        // No state row yet is the normal first-fetch case, not an error: the row
        // is created by the first success or failure record.
        let state = rows.into_iter().next();
        Ok(Some(Feed {
            id: config.id,
            url: config.url,
            topic_id: config.topic_id,
            conditional: ConditionalHeaders {
                etag: state.as_ref().and_then(|s| s.etag.clone()),
                last_modified: state.and_then(|s| s.last_modified),
            },
        }))
    }

    fn load_enabled_feeds(&self) -> CoreResult<Vec<FeedSchedule>> {
        let configs = config_host::load_enabled_feed_configs()?;
        if configs.is_empty() {
            return Ok(Vec::new());
        }
        // One read for every state row, then a lookup per feed. Cheaper than a
        // query per feed, and the table has one row per feed by construction.
        let state: Vec<FeedScheduleRow> = query_rows(
            "SELECT id::text AS id, last_fetched_at FROM argus_feeds",
            &[],
        )?;
        let mut out: Vec<FeedSchedule> = configs
            .into_iter()
            .map(|c| {
                let last_fetched_at = state
                    .iter()
                    .find(|s| s.id == c.id)
                    .and_then(|s| s.last_fetched_at);
                FeedSchedule {
                    id: c.id,
                    interval_seconds: c.interval_seconds,
                    last_fetched_at,
                }
            })
            .collect();
        // The round-robin cursor indexes into this list, so its order has to be
        // stable across ticks. M1 got that from `ORDER BY id`; the item host
        // makes no ordering promise, so the sort is explicit here.
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn upsert_article(&self, a: &NewArticle) -> CoreResult<UpsertResult> {
        // Insert-or-do-nothing on the unique URL. rows-affected tells us whether
        // this call inserted (1) or hit the conflict (0) — the replay-safety
        // signal (M1-6). The id is fetched back by URL in either case.
        let affected = exec(
            "INSERT INTO argus_articles \
             (id, url, title, content, published_at, feed_id, topic_id, pipeline_state, content_hash, created, changed) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4::bigint, $5::uuid, $6::uuid, 'fetched', $7, \
                     EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint) \
             ON CONFLICT (url) DO NOTHING",
            &[
                json!(a.url),
                json!(a.title),
                json!(a.content),
                opt_i64(a.published_at),
                json!(a.feed_id),
                json!(a.topic_id),
                json!(a.content_hash),
            ],
        )?;
        let rows: Vec<IdRow> = query_rows(
            "SELECT id::text AS id FROM argus_articles WHERE url = $1",
            &[json!(a.url)],
        )?;
        let id =
            rows.into_iter().next().map(|r| r.id).ok_or_else(|| {
                CoreError::Store(format!("article vanished after upsert: {}", a.url))
            })?;
        Ok(UpsertResult {
            id,
            inserted: affected >= 1,
        })
    }

    /// Upsert rather than update: from M3 the state row is created by the first
    /// fetch of a feed an admin just added, not by the row that used to carry
    /// the feed's configuration.
    fn record_feed_success(
        &self,
        feed_id: &str,
        cond: &ConditionalHeaders,
        now: i64,
    ) -> CoreResult<()> {
        exec(
            "INSERT INTO argus_feeds (id, etag, last_modified, last_fetched_at, \
                                      failure_count, last_error) \
             VALUES ($1::uuid, $2, $3, $4::bigint, 0, NULL) \
             ON CONFLICT (id) DO UPDATE SET \
                 etag = EXCLUDED.etag, last_modified = EXCLUDED.last_modified, \
                 last_fetched_at = EXCLUDED.last_fetched_at, \
                 failure_count = 0, last_error = NULL, \
                 changed = EXTRACT(EPOCH FROM NOW())::bigint",
            &[
                json!(feed_id),
                opt_str(&cond.etag),
                opt_str(&cond.last_modified),
                json!(now),
            ],
        )?;
        Ok(())
    }

    fn record_feed_failure(&self, feed_id: &str, error: &str, now: i64) -> CoreResult<()> {
        exec(
            "INSERT INTO argus_feeds (id, last_error, last_fetched_at, failure_count) \
             VALUES ($1::uuid, $2, $3::bigint, 1) \
             ON CONFLICT (id) DO UPDATE SET \
                 failure_count = argus_feeds.failure_count + 1, \
                 last_error = EXCLUDED.last_error, \
                 last_fetched_at = EXCLUDED.last_fetched_at, \
                 changed = EXTRACT(EPOCH FROM NOW())::bigint",
            &[json!(feed_id), json!(error), json!(now)],
        )?;
        Ok(())
    }

    /// The article comes from the record table; its topic's scoring criteria
    /// come from the topic Item, so what M1 did as one `LEFT JOIN` is now a
    /// query plus an item-host read.
    fn load_decide_context(&self, article_id: &str) -> CoreResult<Option<DecideContext>> {
        let rows: Vec<DecideRow> = query_rows(
            "SELECT title, content, COALESCE(topic_id::text, '') AS topic_id \
             FROM argus_articles WHERE id = $1::uuid",
            &[json!(article_id)],
        )?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        // A missing or deleted topic is not a missing article: the article is
        // still scored, against an empty brief at the default threshold, exactly
        // as M1's `COALESCE`d outer join did.
        let topic = config_host::load_topic_config(&row.topic_id)?;
        Ok(Some(DecideContext {
            title: row.title,
            content: row.content,
            topic_prompt: topic
                .as_ref()
                .map(|t| t.relevance_prompt.clone())
                .unwrap_or_default(),
            threshold: topic
                .map(|t| t.threshold)
                .unwrap_or(argus_core::config::DEFAULT_RELEVANCE_THRESHOLD),
        }))
    }

    fn record_decision(
        &self,
        article_id: &str,
        score: u8,
        reason: &str,
        keep: bool,
    ) -> CoreResult<()> {
        let state = if keep { "decided" } else { "discarded" };
        exec(
            "UPDATE argus_articles SET relevance_score = $2::int, relevance_reason = $3, \
             pipeline_state = $4, changed = EXTRACT(EPOCH FROM NOW())::bigint WHERE id = $1::uuid",
            &[
                json!(article_id),
                json!(i32::from(score)),
                json!(reason),
                json!(state),
            ],
        )?;
        Ok(())
    }

    fn set_state(
        &self,
        article_id: &str,
        state: argus_core::model::PipelineState,
    ) -> CoreResult<()> {
        exec(
            "UPDATE argus_articles SET pipeline_state = $2, changed = EXTRACT(EPOCH FROM NOW())::bigint \
             WHERE id = $1::uuid",
            &[json!(article_id), json!(state.as_str())],
        )?;
        Ok(())
    }

    fn record_article_error(&self, article_id: &str, error: &str) -> CoreResult<()> {
        exec(
            "UPDATE argus_articles SET pipeline_state = 'error', pipeline_error = $2, \
             changed = EXTRACT(EPOCH FROM NOW())::bigint WHERE id = $1::uuid",
            &[json!(article_id), json!(error)],
        )?;
        Ok(())
    }

    fn load_cursor(&self) -> CoreResult<u64> {
        let rows: Vec<ValueRow> = query_rows(
            "SELECT value FROM argus_state WHERE name = 'schedule_cursor'",
            &[],
        )?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|r| r.value.parse().ok())
            .unwrap_or(0))
    }

    fn save_cursor(&self, cursor: u64) -> CoreResult<()> {
        exec(
            "INSERT INTO argus_state (name, value) VALUES ('schedule_cursor', $1) \
             ON CONFLICT (name) DO UPDATE SET value = $1",
            &[json!(cursor.to_string())],
        )?;
        Ok(())
    }
}

/// The current time as a unix timestamp, sourced from Postgres (WASM has no
/// clock). Used by the queue worker, which — unlike `tap_cron` — receives no
/// timestamp from the kernel.
///
/// # Errors
///
/// Propagates a transient store error if the query fails.
pub fn host_now() -> CoreResult<i64> {
    #[derive(Deserialize)]
    struct TsRow {
        ts: i64,
    }
    let rows: Vec<TsRow> = query_rows("SELECT EXTRACT(EPOCH FROM NOW())::bigint AS ts", &[])?;
    Ok(rows.into_iter().next().map(|r| r.ts).unwrap_or(0))
}
