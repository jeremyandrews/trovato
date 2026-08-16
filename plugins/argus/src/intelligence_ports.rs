//! Implementations of the M2 `argus_core` ports over the kernel `db` and
//! `item-api` hosts.
//!
//! [`HostStore`] already carries the M1 [`Store`](argus_core::ports::Store)
//! surface; the two M2 traits are implemented on the same type here so every
//! stage talks to one object over one host.
//!
//! ## Two constraints shape most of this file
//!
//! **No transactions.** The `db` host exposes `query-raw` and `execute-raw` and
//! rejects any statement containing a semicolon, so a plugin cannot open a
//! transaction and cannot batch statements. Multi-row writes are therefore
//! sequences of independent statements, each atomic on its own. Where the
//! ordering matters — creating a story Item before the row that points at it —
//! the sequence is chosen so that an interruption leaves inert débris rather
//! than a corrupt state, and every write is idempotent so an at-least-once
//! re-delivery converges. Recorded as **G-DB-NO-TX** in `M2-FRICTION.md`.
//!
//! **A 256 KB output buffer.** Every `query-raw` result crosses one
//! caller-allocated buffer, and clustering rows carry a full vector each, so
//! every candidate query here is `LIMIT`ed by an explicit caller-supplied
//! bound. Those bounds are correctness constraints, not tuning.
//!
//! ## Vectors as text
//!
//! Feature vectors are stored as a JSON float array in a `TEXT` column. pgvector
//! is not available (optional extension, absent from stock `postgres:17`) and
//! there is no plugin-facing embedding call to feed it anyway; `M2-DESIGN.md`
//! argues both. Similarity is computed in `argus-core`, not in SQL.

use argus_core::analyze::Analysis;
use argus_core::budget::DailySpend;
use argus_core::cluster::{CandidateArticle, CandidateStory};
use argus_core::embed::fold_centroid;
use argus_core::entity::{EntityAction, EntityPlan, EntityRecord, EntityType};
use argus_core::error::{CoreError, CoreResult};
use argus_core::model::{PipelineState, Stage};
use argus_core::ports::{
    AnalysisStore, AnalyzeContext, ClusterContext, EmbedContext, EntityApplyReport, StoredVector,
    StoryRow, StorySeed, StoryStore,
};
use argus_core::summarize::{StoryMember, StorySummary};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::host_ports::{HostStore, exec, query_rows};
use crate::item_host::save_item;

/// The Item content type stories are stored as.
const STORY_TYPE: &str = "argus_story";

/// Placeholder summary on a story Item that has not been summarized yet.
///
/// Deliberately not plausible prose: an un-summarized story must never read as
/// a summarized one, in the admin or in a reader UI.
const PENDING_SUMMARY: &str = "Clustering in progress; no synthesis yet.";

/// Longest entity-candidate set loaded for one article's resolution. The rows
/// are small, but the table is not bounded, so the query is.
const MAX_ENTITY_CANDIDATES: usize = 200;

/// Map an `item-api` host error code to a transient store error.
///
/// Every item host failure is treated as transient: the failure modes are a
/// missing services handle, a SQL failure, and a serialization failure, none of
/// which a plugin can correct by giving up.
fn item_err(code: i32) -> CoreError {
    CoreError::Store(format!("item host error {code}"))
}

/// Serialize a vector for the `TEXT` column.
fn vector_to_text(vector: &[f32]) -> CoreResult<String> {
    serde_json::to_string(vector).map_err(|e| CoreError::Store(format!("encode vector: {e}")))
}

/// Parse a vector back out of the `TEXT` column.
///
/// A row whose text will not parse is treated as absent rather than fatal: the
/// caller re-embeds, which is both self-healing and the correct response to a
/// column that was written by an older, incompatible recipe.
fn vector_from_text(text: &str) -> Option<Vec<f32>> {
    serde_json::from_str(text).ok()
}

/// The `IN` list of terminal pipeline states, built from the enum so the SQL
/// and [`PipelineState::is_terminal`] cannot drift apart.
fn terminal_state_list() -> String {
    PipelineState::terminal_columns()
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ===========================================================================
// Row shapes
// ===========================================================================

#[derive(Deserialize)]
struct AnalyzeRow {
    title: String,
    content: String,
}

#[derive(Deserialize)]
struct EmbedRow {
    title: String,
    summary: String,
    entities: Vec<String>,
}

#[derive(Deserialize)]
struct EntityRow {
    id: String,
    canonical_name: String,
    match_key: String,
    entity_type: String,
}

#[derive(Deserialize)]
struct VectorRow {
    vector: String,
    recipe: String,
}

#[derive(Deserialize)]
struct SpendRow {
    spent: f64,
    calls: i64,
    unpriced: i64,
}

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Deserialize)]
struct ClusterRow {
    topic_id: Option<String>,
    feed_id: Option<String>,
    published_at: Option<i64>,
    relevance_score: Option<i64>,
    cluster_attempts: i64,
}

#[derive(Deserialize)]
struct CandidateStoryRow {
    id: String,
    centroid: String,
    recipe: String,
    topic_id: Option<String>,
    last_article_at: Option<i64>,
    article_count: i64,
}

#[derive(Deserialize)]
struct CandidateArticleRow {
    id: String,
    vector: String,
    recipe: String,
    feed_id: Option<String>,
    story_id: Option<String>,
    published_at: Option<i64>,
}

#[derive(Deserialize)]
struct StoryRowRaw {
    id: String,
    centroid: String,
    recipe: String,
    article_count: i64,
    last_article_at: Option<i64>,
    last_summarized_at: Option<i64>,
    // M4: the notification trigger needs the story as it stands *before* this
    // summarize overwrites it — the previous narrative is what the change judge
    // compares against, and there is nowhere else to read it from.
    title: String,
    summary: String,
    topic_id: Option<String>,
    relevance_score: Option<i32>,
}

/// Everything `sync_story_item` needs to rewrite a story's Item in full.
///
/// It is the *whole* projection, deliberately: `save-item` replaces the entire
/// `fields` object, so a partial write erases whatever it omits.
#[derive(Deserialize)]
struct StoryStateRow {
    centroid: String,
    article_count: i64,
    first_article_at: Option<i64>,
    last_article_at: Option<i64>,
    relevance_score: Option<i64>,
    topic_id: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    sources: Option<String>,
    last_summarized_at: Option<i64>,
    is_active: bool,
}

#[derive(Deserialize)]
struct MemberRow {
    id: String,
    title: String,
    summary: String,
    source: String,
    published_at: Option<i64>,
    is_duplicate: bool,
}

// ===========================================================================
// AnalysisStore
// ===========================================================================

impl AnalysisStore for HostStore {
    fn load_analyze_context(&self, article_id: &str) -> CoreResult<Option<AnalyzeContext>> {
        let rows: Vec<AnalyzeRow> = query_rows(
            "SELECT title, content FROM argus_articles WHERE id = $1::uuid",
            &[json!(article_id)],
        )?;
        Ok(rows.into_iter().next().map(|r| AnalyzeContext {
            title: r.title,
            content: r.content,
        }))
    }

    fn record_analysis(
        &self,
        article_id: &str,
        analysis: &Analysis,
        raw: &str,
        now: i64,
    ) -> CoreResult<()> {
        exec(
            "UPDATE argus_articles SET summary = $2, critical_analysis = $3, \
             fallacy_analysis = $4, source_analysis = $5, analysis = $6, \
             analyzed_at = $7::bigint, pipeline_state = 'analyzed', \
             changed = EXTRACT(EPOCH FROM NOW())::bigint WHERE id = $1::uuid",
            &[
                json!(article_id),
                json!(analysis.summary),
                json!(analysis.critical_analysis),
                json!(analysis.fallacy_analysis),
                json!(analysis.source_analysis),
                json!(raw),
                json!(now),
            ],
        )?;
        Ok(())
    }

    fn load_entity_candidates(
        &self,
        prefixes: &[String],
        types: &[EntityType],
    ) -> CoreResult<Vec<EntityRecord>> {
        if prefixes.is_empty() || types.is_empty() {
            return Ok(Vec::new());
        }
        let type_names: Vec<&str> = types.iter().map(EntityType::as_str).collect();
        // The prefix list is matched with LIKE rather than `left(match_key, n)`
        // because prefixes are shorter than four characters for short names.
        // A match key is `[a-z0-9]*` by construction, so it can carry no LIKE
        // wildcard; the pattern is still built by the database from a bound
        // parameter, never by string concatenation here.
        let rows: Vec<EntityRow> = query_rows(
            &format!(
                "SELECT id::text AS id, canonical_name, match_key, entity_type \
                 FROM argus_entities \
                 WHERE entity_type IN (SELECT jsonb_array_elements_text($1::jsonb)) \
                   AND EXISTS (SELECT 1 FROM jsonb_array_elements_text($2::jsonb) AS p(prefix) \
                               WHERE match_key LIKE p.prefix || '%') \
                 ORDER BY match_key LIMIT {MAX_ENTITY_CANDIDATES}"
            ),
            &[json!(type_names), json!(prefixes)],
        )?;
        Ok(rows
            .into_iter()
            .map(|r| EntityRecord {
                id: r.id,
                canonical_name: r.canonical_name,
                match_key: r.match_key,
                entity_type: EntityType::parse(&r.entity_type),
            })
            .collect())
    }

    fn apply_entity_plan(
        &self,
        article_id: &str,
        plan: &EntityPlan,
        now: i64,
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
                        // The guard in the WHERE clause is what makes this
                        // idempotent: a re-delivered job appends nothing and
                        // reports zero rows affected.
                        let added = exec(
                            "UPDATE argus_entities \
                             SET aliases = aliases || jsonb_build_array($2::text), \
                                 changed = $3::bigint \
                             WHERE id = $1::uuid \
                               AND NOT (aliases @> jsonb_build_array($2::text)) \
                               AND canonical_name <> $2",
                            &[json!(entity_id), json!(alias), json!(now)],
                        )?;
                        if added > 0 {
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
                    exec(
                        "INSERT INTO argus_entities \
                         (id, canonical_name, match_key, entity_type, aliases, article_count, \
                          first_seen_at, last_seen_at, created, changed) \
                         VALUES (gen_random_uuid(), $1, $2, $3, '[]'::jsonb, 0, \
                                 $4::bigint, $4::bigint, $4::bigint, $4::bigint) \
                         ON CONFLICT (match_key, entity_type) DO NOTHING",
                        &[
                            json!(canonical_name),
                            json!(match_key),
                            json!(entity_type.as_str()),
                            json!(now),
                        ],
                    )?;
                    let rows: Vec<IdRow> = query_rows(
                        "SELECT id::text AS id FROM argus_entities \
                         WHERE match_key = $1 AND entity_type = $2",
                        &[json!(match_key), json!(entity_type.as_str())],
                    )?;
                    rows.into_iter().next().map(|r| r.id).ok_or_else(|| {
                        CoreError::Store(format!("entity vanished after upsert: {match_key}"))
                    })?
                }
            };

            // Only a genuinely new link bumps the entity's counters, so an
            // at-least-once replay cannot inflate `article_count`.
            let linked = exec(
                "INSERT INTO argus_article_entities (article_id, entity_id, created) \
                 VALUES ($1::uuid, $2::uuid, $3::bigint) \
                 ON CONFLICT (article_id, entity_id) DO NOTHING",
                &[json!(article_id), json!(entity_id), json!(now)],
            )?;
            if linked > 0 {
                exec(
                    "UPDATE argus_entities \
                     SET article_count = article_count + 1, \
                         last_seen_at = GREATEST(last_seen_at, $2::bigint), \
                         changed = $2::bigint \
                     WHERE id = $1::uuid",
                    &[json!(entity_id), json!(now)],
                )?;
            }
        }
        Ok(report)
    }

    fn load_embed_context(&self, article_id: &str) -> CoreResult<Option<EmbedContext>> {
        // The summary falls back to the body: an article whose analyze call
        // produced entities but no summary still deserves a usable vector.
        let rows: Vec<EmbedRow> = query_rows(
            "SELECT a.title AS title, \
                    COALESCE(NULLIF(a.summary, ''), a.content) AS summary, \
                    COALESCE((SELECT jsonb_agg(e.canonical_name) \
                              FROM argus_article_entities ae \
                              JOIN argus_entities e ON e.id = ae.entity_id \
                              WHERE ae.article_id = a.id), '[]'::jsonb) AS entities \
             FROM argus_articles a WHERE a.id = $1::uuid",
            &[json!(article_id)],
        )?;
        Ok(rows.into_iter().next().map(|r| EmbedContext {
            title: r.title,
            summary: r.summary,
            entities: r.entities,
        }))
    }

    fn save_vector(
        &self,
        article_id: &str,
        vector: &[f32],
        recipe: &str,
        now: i64,
    ) -> CoreResult<()> {
        exec(
            "INSERT INTO argus_article_vectors (article_id, dim, recipe, vector, created) \
             VALUES ($1::uuid, $2::int, $3, $4, $5::bigint) \
             ON CONFLICT (article_id) DO UPDATE SET dim = EXCLUDED.dim, \
                 recipe = EXCLUDED.recipe, vector = EXCLUDED.vector, created = EXCLUDED.created",
            &[
                json!(article_id),
                json!(vector.len() as i64),
                json!(recipe),
                json!(vector_to_text(vector)?),
                json!(now),
            ],
        )?;
        Ok(())
    }

    fn load_vector(&self, article_id: &str) -> CoreResult<Option<StoredVector>> {
        let rows: Vec<VectorRow> = query_rows(
            "SELECT vector, recipe FROM argus_article_vectors WHERE article_id = $1::uuid",
            &[json!(article_id)],
        )?;
        Ok(rows.into_iter().next().and_then(|r| {
            vector_from_text(&r.vector).map(|vector| StoredVector {
                vector,
                recipe: r.recipe,
            })
        }))
    }

    fn record_cost(&self, day: &str, stage: Stage, cost: Option<f64>, _now: i64) -> CoreResult<()> {
        // An unpriced call increments `unpriced_calls` and adds nothing to the
        // dollar total: unknown spend must not read as zero spend.
        let (unpriced, dollars) = match cost {
            Some(c) => (0_i64, c),
            None => (1_i64, 0.0_f64),
        };
        exec(
            "INSERT INTO argus_cost_daily (day, stage, calls, unpriced_calls, cost_usd) \
             VALUES ($1, $2, 1, $3::int, $4::double precision) \
             ON CONFLICT (day, stage) DO UPDATE SET \
                 calls = argus_cost_daily.calls + 1, \
                 unpriced_calls = argus_cost_daily.unpriced_calls + EXCLUDED.unpriced_calls, \
                 cost_usd = argus_cost_daily.cost_usd + EXCLUDED.cost_usd",
            &[
                json!(day),
                json!(stage.queue_name()),
                json!(unpriced),
                json!(dollars),
            ],
        )?;
        Ok(())
    }

    fn load_daily_spend(&self, day: &str) -> CoreResult<DailySpend> {
        let rows: Vec<SpendRow> = query_rows(
            "SELECT COALESCE(SUM(cost_usd), 0)::double precision AS spent, \
                    COALESCE(SUM(calls), 0)::bigint AS calls, \
                    COALESCE(SUM(unpriced_calls), 0)::bigint AS unpriced \
             FROM argus_cost_daily WHERE day = $1",
            &[json!(day)],
        )?;
        Ok(rows
            .into_iter()
            .next()
            .map(|r| DailySpend {
                spent_usd: r.spent,
                calls: r.calls.clamp(0, i64::from(u32::MAX)) as u32,
                unpriced_calls: r.unpriced.clamp(0, i64::from(u32::MAX)) as u32,
            })
            .unwrap_or_default())
    }

    fn purge_article_content(&self, cutoff: i64, now: i64, limit: usize) -> CoreResult<u64> {
        // Body text only. Scores, analysis prose, entity links and story
        // membership survive, so a purged article still reads as a source on
        // its story; only the text nobody re-reads is reclaimed.
        exec(
            &format!(
                "UPDATE argus_articles SET content = '', content_purged_at = $2::bigint, \
                     changed = EXTRACT(EPOCH FROM NOW())::bigint \
                 WHERE id IN (SELECT id FROM argus_articles \
                              WHERE pipeline_state IN ({states}) \
                                AND content_purged_at IS NULL \
                                AND content <> '' \
                                AND published_at IS NOT NULL \
                                AND published_at < $1::bigint \
                              ORDER BY published_at LIMIT $3::int)",
                states = terminal_state_list()
            ),
            &[json!(cutoff), json!(now), json!(limit as i64)],
        )
    }
}

// ===========================================================================
// StoryStore
// ===========================================================================

/// Everything an `argus_story` Item's `fields` object can carry.
///
/// A struct rather than a parameter list because the fields are individually
/// optional and easy to transpose: two adjacent `Option<i64>` timestamps passed
/// positionally is exactly the bug that would silently invert a story's span.
#[derive(Debug, Clone, Default)]
struct StoryItemFields<'a> {
    summary: &'a str,
    topic_id: Option<&'a str>,
    article_count: u32,
    relevance: Option<i32>,
    sources: Option<&'a str>,
    summary_updated_at: Option<i64>,
    is_active: bool,
    first_article_at: Option<i64>,
    last_article_at: Option<i64>,
}

/// Build the `fields` object for an `argus_story` Item.
///
/// This is the **complete** field set every time, never a patch: `save-item`
/// replaces the whole `fields` object, so anything omitted here is erased from
/// the Item. Optional values are omitted only when the story genuinely has no
/// value for them yet.
fn story_fields(f: &StoryItemFields<'_>) -> Value {
    let mut fields = json!({
        "field_summary": { "value": f.summary },
        "field_article_count": { "value": f.article_count },
        "field_is_active": { "value": f.is_active },
    });
    let Some(map) = fields.as_object_mut() else {
        return fields;
    };
    if let Some(topic) = f.topic_id {
        map.insert("field_topic_id".into(), json!({ "value": topic }));
    }
    if let Some(score) = f.relevance {
        map.insert("field_relevance_score".into(), json!({ "value": score }));
    }
    if let Some(sources) = f.sources {
        map.insert("field_sources".into(), json!({ "value": sources }));
    }
    if let Some(at) = f.summary_updated_at {
        map.insert("field_summary_updated".into(), json!({ "value": at }));
    }
    if let Some(at) = f.first_article_at {
        map.insert("field_first_article".into(), json!({ "value": at }));
    }
    if let Some(at) = f.last_article_at {
        map.insert("field_last_article".into(), json!({ "value": at }));
    }
    fields
}

/// The `sources` json written onto a story Item: one entry per member,
/// including near-duplicates, each naming what it contributed.
fn sources_json(members: &[StoryMember]) -> String {
    let entries: Vec<Value> = members
        .iter()
        .map(|m| {
            json!({
                "article_id": m.article_id,
                "source": m.source,
                "title": m.title,
                "published_at": m.published_at,
                // A duplicate is credited as a source without being counted as
                // a member, so a reader can see that three outlets carried it.
                "contribution": if m.is_duplicate { "duplicate" } else { "member" },
            })
        })
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

impl HostStore {
    /// Load the story fields the join/summary paths need to recompute.
    fn story_state(&self, story_id: &str) -> CoreResult<Option<StoryStateRow>> {
        let rows: Vec<StoryStateRow> = query_rows(
            "SELECT centroid, article_count, first_article_at, last_article_at, \
                    relevance_score, topic_id::text AS topic_id, title, summary, sources, \
                    last_summarized_at, is_active \
             FROM argus_stories WHERE id = $1::uuid",
            &[json!(story_id)],
        )?;
        Ok(rows.into_iter().next())
    }

    /// Push a story's current state onto its `argus_story` Item.
    ///
    /// Every story mutation refreshes the Item, because the Item is what a
    /// reader and the admin actually see; a row that has moved on while its
    /// Item has not is a story that silently stopped updating.
    fn sync_story_item(&self, story_id: &str, state: &StoryStateRow) -> CoreResult<()> {
        let mut payload = json!({
            "id": story_id,
            "fields": story_fields(&StoryItemFields {
                summary: state.summary.as_deref().unwrap_or(PENDING_SUMMARY),
                topic_id: state.topic_id.as_deref(),
                article_count: state.article_count.clamp(0, i64::from(u32::MAX)) as u32,
                relevance: state.relevance_score.map(|s| s.clamp(0, 100) as i32),
                sources: state.sources.as_deref(),
                summary_updated_at: state.last_summarized_at,
                is_active: state.is_active,
                first_article_at: state.first_article_at,
                last_article_at: state.last_article_at,
            }),
        });
        if let (Some(title), Some(map)) = (state.title.as_deref(), payload.as_object_mut()) {
            map.insert("title".into(), json!(title));
        }
        save_item(&payload).map_err(item_err)?;
        Ok(())
    }
}

impl StoryStore for HostStore {
    fn try_acquire_cluster_lease(
        &self,
        token: &str,
        now: i64,
        lease_seconds: i64,
    ) -> CoreResult<bool> {
        // One statement, so the check and the take are atomic without a
        // transaction the `db` host does not offer. The row stores
        // `"{expiry}|{token}"`; the `split_part` on the left of the comparison
        // is what makes the expiry readable without a second column, and the
        // conflict clause only overwrites a lease that is free or expired.
        let value = format!("{}|{token}", now + lease_seconds.max(1));
        let taken = exec(
            "INSERT INTO argus_state (name, value) VALUES ('cluster_lease', $1) \
             ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value \
             WHERE (split_part(argus_state.value, '|', 1) ~ '^[0-9]+$' \
                    AND split_part(argus_state.value, '|', 1)::bigint <= $2::bigint) \
                OR split_part(argus_state.value, '|', 2) = $3",
            &[json!(value), json!(now), json!(token)],
        )?;
        Ok(taken > 0)
    }

    fn release_cluster_lease(&self, token: &str) -> CoreResult<()> {
        // Only the holder may release, so a worker whose lease already expired
        // and was taken by someone else cannot free the new holder's lease.
        exec(
            "UPDATE argus_state SET value = '0|' WHERE name = 'cluster_lease' \
             AND split_part(value, '|', 2) = $1",
            &[json!(token)],
        )?;
        Ok(())
    }

    fn load_cluster_context(&self, article_id: &str) -> CoreResult<Option<ClusterContext>> {
        let rows: Vec<ClusterRow> = query_rows(
            "SELECT topic_id::text AS topic_id, feed_id::text AS feed_id, published_at, \
                    relevance_score, cluster_attempts \
             FROM argus_articles WHERE id = $1::uuid",
            &[json!(article_id)],
        )?;
        Ok(rows.into_iter().next().map(|r| ClusterContext {
            topic_id: r.topic_id,
            feed_id: r.feed_id,
            published_at: r.published_at,
            relevance_score: r.relevance_score.map(|s| s.clamp(0, 100) as i32),
            waits: r.cluster_attempts.clamp(0, i64::from(u32::MAX)) as u32,
        }))
    }

    fn load_candidate_stories(
        &self,
        article_id: &str,
        window_start: i64,
        limit: usize,
    ) -> CoreResult<Vec<CandidateStory>> {
        // The precision guard the design note describes: a story is only a
        // candidate if it shares the article's topic or one of its entities.
        // Without it every active story in the window would be scored, which is
        // both slower and more likely to produce a spurious near-threshold
        // match.
        let rows: Vec<CandidateStoryRow> = query_rows(
            "SELECT s.id::text AS id, s.centroid, s.recipe, s.topic_id::text AS topic_id, \
                    s.last_article_at, s.article_count \
             FROM argus_stories s \
             WHERE s.is_active = true \
               AND (s.last_article_at IS NULL OR s.last_article_at >= $2::bigint) \
               AND (s.topic_id = (SELECT topic_id FROM argus_articles WHERE id = $1::uuid) \
                    OR EXISTS (SELECT 1 FROM argus_articles m \
                               JOIN argus_article_entities me ON me.article_id = m.id \
                               WHERE m.story_id = s.id \
                                 AND me.entity_id IN (SELECT entity_id \
                                                      FROM argus_article_entities \
                                                      WHERE article_id = $1::uuid))) \
             ORDER BY s.last_article_at DESC NULLS LAST LIMIT $3::int",
            &[json!(article_id), json!(window_start), json!(limit as i64)],
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                vector_from_text(&r.centroid).map(|centroid| CandidateStory {
                    id: r.id,
                    centroid,
                    recipe: r.recipe,
                    topic_id: r.topic_id,
                    last_article_at: r.last_article_at,
                    article_count: r.article_count.clamp(0, i64::from(u32::MAX)) as u32,
                })
            })
            .collect())
    }

    fn load_candidate_articles(
        &self,
        article_id: &str,
        window_start: i64,
        limit: usize,
    ) -> CoreResult<Vec<CandidateArticle>> {
        // Near-duplicate candidates must share an entity. An article with no
        // extracted entities therefore gets no duplicate detection — an
        // accepted limitation, recorded in M2-FRICTION.md, and the honest
        // alternative (scan every embedded article in the window) does not fit
        // the output buffer.
        let rows: Vec<CandidateArticleRow> = query_rows(
            "SELECT a.id::text AS id, v.vector, v.recipe, a.feed_id::text AS feed_id, \
                    a.story_id::text AS story_id, a.published_at \
             FROM argus_articles a \
             JOIN argus_article_vectors v ON v.article_id = a.id \
             WHERE a.id <> $1::uuid \
               AND a.is_duplicate = false \
               AND (a.published_at IS NULL OR a.published_at >= $2::bigint) \
               AND EXISTS (SELECT 1 FROM argus_article_entities x \
                           WHERE x.article_id = a.id \
                             AND x.entity_id IN (SELECT entity_id \
                                                 FROM argus_article_entities \
                                                 WHERE article_id = $1::uuid)) \
             ORDER BY a.published_at ASC NULLS LAST LIMIT $3::int",
            &[json!(article_id), json!(window_start), json!(limit as i64)],
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                vector_from_text(&r.vector).map(|vector| CandidateArticle {
                    id: r.id,
                    vector,
                    recipe: r.recipe,
                    feed_id: r.feed_id,
                    story_id: r.story_id,
                    published_at: r.published_at,
                })
            })
            .collect())
    }

    fn create_story(&self, seed: &StorySeed, article_id: &str, now: i64) -> CoreResult<String> {
        // Item first, because `argus_stories.id` IS the Item id — there is no
        // id to insert until the Item exists. With no transaction available
        // (G-DB-NO-TX), a failure between the two leaves an Item with no row:
        // inert, invisible to clustering and to summarize, and detectable as an
        // `argus_story` Item with no matching `argus_stories` row.
        let created = save_item(&json!({
            "type": STORY_TYPE,
            "title": seed.title,
            "status": 1,
            "fields": story_fields(&StoryItemFields {
                // A brand-new story has no synthesis yet, and `field_summary`
                // is required, so the placeholder stands in until summarize runs.
                summary: PENDING_SUMMARY,
                topic_id: seed.topic_id.as_deref(),
                article_count: 1,
                relevance: seed.relevance_score,
                is_active: true,
                first_article_at: seed.published_at,
                last_article_at: seed.published_at,
                ..Default::default()
            }),
        }))
        .map_err(item_err)?;

        let story_id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::Store("save-item returned no id".to_string()))?
            .to_string();

        exec(
            "INSERT INTO argus_stories \
             (id, topic_id, centroid, dim, recipe, article_count, first_article_at, \
              last_article_at, is_active, relevance_score, summarize_pending, created, changed) \
             VALUES ($1::uuid, $2::uuid, $3, $4::int, $5, 1, $6::bigint, $6::bigint, true, \
                     $7::int, true, $8::bigint, $8::bigint) \
             ON CONFLICT (id) DO NOTHING",
            &[
                json!(story_id),
                seed.topic_id.as_ref().map_or(Value::Null, |t| json!(t)),
                json!(vector_to_text(&seed.centroid)?),
                json!(seed.centroid.len() as i64),
                json!(seed.recipe),
                seed.published_at.map_or(Value::Null, |t| json!(t)),
                seed.relevance_score.map_or(Value::Null, |s| json!(s)),
                json!(now),
            ],
        )?;

        exec(
            "UPDATE argus_articles SET story_id = $2::uuid, pipeline_state = 'complete', \
             changed = EXTRACT(EPOCH FROM NOW())::bigint WHERE id = $1::uuid",
            &[json!(article_id), json!(story_id)],
        )?;

        Ok(story_id)
    }

    fn join_story(
        &self,
        story_id: &str,
        article_id: &str,
        vector: &[f32],
        ctx: &ClusterContext,
        _now: i64,
    ) -> CoreResult<()> {
        let Some(state) = self.story_state(story_id)? else {
            return Err(CoreError::NotFound(format!("story {story_id}")));
        };

        // Claim the article first. The claim is conditional on the article not
        // already belonging to a story, so a re-delivered job affects zero rows
        // and the counter updates below are skipped — which is what keeps
        // `article_count` honest under an at-least-once queue.
        let claimed = exec(
            "UPDATE argus_articles SET story_id = $2::uuid, pipeline_state = 'complete', \
             changed = EXTRACT(EPOCH FROM NOW())::bigint \
             WHERE id = $1::uuid AND story_id IS DISTINCT FROM $2::uuid",
            &[json!(article_id), json!(story_id)],
        )?;
        if claimed == 0 {
            return Ok(());
        }

        let count = state.article_count.clamp(0, i64::from(u32::MAX)) as u32;
        let centroid = match vector_from_text(&state.centroid) {
            Some(current) => fold_centroid(&current, count, vector),
            // An unreadable centroid is replaced by the joining vector rather
            // than left to poison every future comparison.
            None => vector.to_vec(),
        };

        exec(
            "UPDATE argus_stories SET centroid = $2, dim = $3::int, \
                 article_count = article_count + 1, \
                 first_article_at = LEAST(COALESCE(first_article_at, $4::bigint), \
                                          COALESCE($4::bigint, first_article_at)), \
                 last_article_at = GREATEST(COALESCE(last_article_at, $4::bigint), \
                                            COALESCE($4::bigint, last_article_at)), \
                 relevance_score = GREATEST(COALESCE(relevance_score, $5::int), \
                                            COALESCE($5::int, relevance_score)), \
                 is_active = true, summarize_pending = true, changed = $6::bigint \
             WHERE id = $1::uuid",
            &[
                json!(story_id),
                json!(vector_to_text(&centroid)?),
                json!(centroid.len() as i64),
                ctx.published_at.map_or(Value::Null, |t| json!(t)),
                ctx.relevance_score.map_or(Value::Null, |s| json!(s)),
                json!(_now),
            ],
        )?;

        if let Some(fresh) = self.story_state(story_id)? {
            self.sync_story_item(story_id, &fresh)?;
        }
        Ok(())
    }

    fn mark_duplicate(
        &self,
        article_id: &str,
        of_article_id: &str,
        story_id: Option<&str>,
        _now: i64,
    ) -> CoreResult<()> {
        exec(
            "UPDATE argus_articles SET is_duplicate = true, duplicate_of = $2::uuid, \
                 story_id = $3::uuid, pipeline_state = 'complete', \
                 changed = EXTRACT(EPOCH FROM NOW())::bigint \
             WHERE id = $1::uuid",
            &[
                json!(article_id),
                json!(of_article_id),
                story_id.map_or(Value::Null, |s| json!(s)),
            ],
        )?;
        if let Some(story_id) = story_id {
            // The story's source list changed even though its member count did
            // not, so it is worth re-summarizing.
            exec(
                "UPDATE argus_stories SET summarize_pending = true, \
                 changed = EXTRACT(EPOCH FROM NOW())::bigint WHERE id = $1::uuid",
                &[json!(story_id)],
            )?;
        }
        Ok(())
    }

    fn record_wait(&self, article_id: &str, _now: i64) -> CoreResult<()> {
        exec(
            "UPDATE argus_articles SET cluster_attempts = cluster_attempts + 1, \
                 pipeline_state = 'waiting', changed = EXTRACT(EPOCH FROM NOW())::bigint \
             WHERE id = $1::uuid",
            &[json!(article_id)],
        )?;
        Ok(())
    }

    fn load_story(&self, story_id: &str) -> CoreResult<Option<StoryRow>> {
        let rows: Vec<StoryRowRaw> = query_rows(
            "SELECT id::text AS id, centroid, recipe, article_count, last_article_at, \
                    last_summarized_at, COALESCE(title, '') AS title, \
                    COALESCE(summary, '') AS summary, topic_id::text AS topic_id, \
                    relevance_score \
             FROM argus_stories WHERE id = $1::uuid",
            &[json!(story_id)],
        )?;
        Ok(rows.into_iter().next().map(|r| StoryRow {
            id: r.id,
            centroid: vector_from_text(&r.centroid).unwrap_or_default(),
            recipe: r.recipe,
            article_count: r.article_count.clamp(0, i64::from(u32::MAX)) as u32,
            last_article_at: r.last_article_at,
            last_summarized_at: r.last_summarized_at,
            title: r.title,
            summary: r.summary,
            topic_id: r.topic_id,
            relevance_score: r.relevance_score,
        }))
    }

    fn load_story_members(&self, story_id: &str, limit: usize) -> CoreResult<Vec<StoryMember>> {
        // Newest first, so a story that has outgrown the prompt is described by
        // the reports that changed it rather than the ones that started it.
        let rows: Vec<MemberRow> = query_rows(
            "SELECT a.id::text AS id, a.title, COALESCE(a.summary, '') AS summary, \
                    COALESCE(NULLIF(f.name, ''), 'Unknown source') AS source, \
                    a.published_at, a.is_duplicate \
             FROM argus_articles a \
             LEFT JOIN argus_feeds f ON f.id = a.feed_id \
             WHERE a.story_id = $1::uuid \
             ORDER BY a.published_at DESC NULLS LAST LIMIT $2::int",
            &[json!(story_id), json!(limit as i64)],
        )?;
        Ok(rows
            .into_iter()
            .map(|r| StoryMember {
                article_id: r.id,
                title: r.title,
                summary: r.summary,
                source: r.source,
                published_at: r.published_at,
                is_duplicate: r.is_duplicate,
            })
            .collect())
    }

    fn record_story_summary(
        &self,
        story_id: &str,
        summary: &StorySummary,
        members: &[StoryMember],
        now: i64,
    ) -> CoreResult<()> {
        exec(
            "UPDATE argus_stories SET title = $3, summary = $4, sources = $5, \
                 last_summarized_at = $2::bigint, summarize_pending = false, \
                 changed = $2::bigint WHERE id = $1::uuid",
            &[
                json!(story_id),
                json!(now),
                json!(summary.title),
                json!(summary.summary),
                json!(sources_json(members)),
            ],
        )?;
        let Some(state) = self.story_state(story_id)? else {
            return Err(CoreError::NotFound(format!("story {story_id}")));
        };
        self.sync_story_item(story_id, &state)
    }

    fn clear_summarize_pending(&self, story_id: &str) -> CoreResult<()> {
        exec(
            "UPDATE argus_stories SET summarize_pending = false, \
             changed = EXTRACT(EPOCH FROM NOW())::bigint WHERE id = $1::uuid",
            &[json!(story_id)],
        )?;
        Ok(())
    }

    fn deactivate_stale_stories(&self, cutoff: i64, now: i64) -> CoreResult<u64> {
        // The row is the source of truth for candidate selection, so retiring
        // it is what actually stops the story accepting articles; the Item sync
        // that follows is for the reader's benefit.
        let retired_ids: Vec<IdRow> = query_rows(
            "SELECT id::text AS id FROM argus_stories \
             WHERE is_active = true AND last_article_at IS NOT NULL \
               AND last_article_at < $1::bigint LIMIT 100",
            &[json!(cutoff)],
        )?;
        if retired_ids.is_empty() {
            return Ok(0);
        }
        let retired = exec(
            "UPDATE argus_stories SET is_active = false, changed = $2::bigint \
             WHERE is_active = true AND last_article_at IS NOT NULL \
               AND last_article_at < $1::bigint",
            &[json!(cutoff), json!(now)],
        )?;
        for row in &retired_ids {
            if let Some(state) = self.story_state(&row.id)? {
                self.sync_story_item(&row.id, &state)?;
            }
        }
        Ok(retired)
    }

    fn load_waiting_articles(&self, cutoff: i64, limit: usize) -> CoreResult<Vec<String>> {
        let rows: Vec<IdRow> = query_rows(
            "SELECT id::text AS id FROM argus_articles \
             WHERE pipeline_state = 'waiting' AND changed <= $1::bigint \
             ORDER BY changed LIMIT $2::int",
            &[json!(cutoff), json!(limit as i64)],
        )?;
        Ok(rows.into_iter().map(|r| r.id).collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn member(id: &str, source: &str, dup: bool) -> StoryMember {
        StoryMember {
            article_id: id.into(),
            title: format!("Title {id}"),
            summary: String::new(),
            source: source.into(),
            published_at: Some(100),
            is_duplicate: dup,
        }
    }

    #[test]
    fn vectors_round_trip_through_the_text_column() {
        let v = vec![0.5_f32, -0.25, 0.0, 1.0];
        let text = vector_to_text(&v).unwrap();
        assert_eq!(vector_from_text(&text), Some(v));
    }

    #[test]
    fn an_unreadable_vector_reads_as_absent() {
        assert_eq!(vector_from_text("not a vector"), None);
        assert_eq!(vector_from_text(""), None);
    }

    #[test]
    fn the_terminal_state_list_matches_the_enum() {
        let list = terminal_state_list();
        for state in PipelineState::terminal_columns() {
            assert!(list.contains(state), "{state} missing from the SQL list");
        }
        assert!(!list.contains("fetched"));
        assert!(!list.contains("waiting"));
    }

    #[test]
    fn sources_json_credits_duplicates_distinctly() {
        let json_text = sources_json(&[member("a", "Reuters", false), member("b", "AP", true)]);
        let parsed: Vec<Value> = serde_json::from_str(&json_text).unwrap();
        assert_eq!(parsed.len(), 2, "duplicates stay in the source list");
        assert_eq!(parsed[0]["contribution"], "member");
        assert_eq!(parsed[1]["contribution"], "duplicate");
        assert_eq!(parsed[0]["source"], "Reuters");
    }

    #[test]
    fn story_fields_omit_what_is_not_known() {
        let fields = story_fields(&StoryItemFields {
            summary: PENDING_SUMMARY,
            article_count: 1,
            is_active: true,
            ..Default::default()
        });
        let map = fields.as_object().unwrap();
        assert_eq!(fields["field_summary"]["value"], PENDING_SUMMARY);
        assert!(map.contains_key("field_article_count"));
        assert!(map.contains_key("field_is_active"));
        // `save-item` replaces the whole object, so the fields present here are
        // exactly the fields the Item will have. Absent ones are absent because
        // the story has no value for them yet, not because this is a patch.
        assert!(!map.contains_key("field_topic_id"));
        assert!(!map.contains_key("field_sources"));
        assert!(!map.contains_key("field_relevance_score"));
    }

    #[test]
    fn story_fields_carry_everything_once_it_is_known() {
        let fields = story_fields(&StoryItemFields {
            summary: "narrative",
            topic_id: Some("topic-1"),
            article_count: 4,
            relevance: Some(88),
            sources: Some("[]"),
            summary_updated_at: Some(1000),
            is_active: false,
            first_article_at: Some(10),
            last_article_at: Some(20),
        });
        assert_eq!(fields["field_summary"]["value"], "narrative");
        assert_eq!(fields["field_article_count"]["value"], 4);
        assert_eq!(fields["field_topic_id"]["value"], "topic-1");
        assert_eq!(fields["field_relevance_score"]["value"], 88);
        assert_eq!(fields["field_is_active"]["value"], false);
        assert_eq!(fields["field_first_article"]["value"], 10);
        assert_eq!(fields["field_last_article"]["value"], 20);
    }
}
