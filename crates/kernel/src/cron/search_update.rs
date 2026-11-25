//! The incremental search index update (part 3 of
//! `docs/design/incremental-search-index.md`), and the cron run that drives it.
//!
//! Writers queue item ids in `search_index_queue`
//! ([`crate::search::index_queue`]). Each cron run, [`run_index`]:
//!
//! 1. takes a **full build** instead when one is asked for, when nothing has
//!    been published yet, or when the live directory has no entry file;
//! 2. otherwise drains the queue in batches of [`BATCH_SIZE`], oldest first.
//!    Each batch is one [`incremental_update`]: the queued items are exported
//!    through the same access seam as a full build (restricted to their ids),
//!    rendered, hashed against the term cache and tokenized when changed;
//!    then, in one transaction under the publish lock, the ledger places them
//!    (an item keeps its page number; a new one takes the lowest tombstone,
//!    else a new slot; one no longer searchable leaves a tombstone), the
//!    updater rewrites only the touched fragments and word chunks and the
//!    meta file ([`trovato_search_index::update`]), and the result is
//!    published in the design's write order, entry file last. The batch's
//!    queue rows are deleted in the same transaction, exactly the
//!    `(item_id, seq)` pairs read, so a change queued meanwhile stays;
//! 3. falls back to a **full build, logged at `warn` with the reason**,
//!    whenever a precondition fails: the state row's Pagefind version is not
//!    the pinned one, the live entry names other meta files than the database
//!    recorded, a meta file is missing, the ledger and the meta disagree on the
//!    slot count, a cached term row is missing, a chunk does not decode or the
//!    term cache and a chunk disagree, a batch needs a language that has no
//!    index yet or empties one, or the queue holds more items than a quarter
//!    of the index (where a full build is the cheaper path);
//! 4. after draining, **compacts** (a full build that renumbers densely) when
//!    tombstones are more than the site's compaction threshold of a language's
//!    page slots, measured after free list reuse. Logged at `info` with the
//!    ratio.
//!
//! **One publisher at a time.** Every publish, full or incremental, writes the
//! output directory only while holding `pg_advisory_xact_lock(hashtext(
//! 'search_index'))`, and only if `search_index_state.generation` is still the
//! one read before its export; a runner that lost the race changes nothing
//! and its batch stays queued. Cron's own Redis lock already keeps a second
//! server from running cron at the same time; the advisory lock and the
//! generation check hold even if it did not.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use trovato_search_index::bundle::{ENTRY_FILE, entry_json_for};
use trovato_search_index::decode::{Bundle, published_files};
use trovato_search_index::publish::{publish, staging_dir_for, write_bundle};
use trovato_search_index::update::{LanguageUpdate, PageChange, read_meta, update_language};
use trovato_search_index::{ChunkPlan, PAGEFIND_VERSION, PageTerms};

use super::pagefind::{IndexDocument, render_fragment};
use super::search_index::{
    FullBuildOptions, FullBuildReport, LOCK_KEY, LedgerRow, RebuildAgain, StagingGuard, TermRow,
    apply_ledger_plan, delete_drained, full_build_with, generation, page_url, plan_ledger,
    store_chunk_plans, sweep,
};

/// Queue rows applied per incremental update.
pub const BATCH_SIZE: usize = 200;

/// Most incremental updates one cron run makes; the rest wait for the next.
pub const MAX_BATCHES_PER_RUN: usize = 50;

/// The default compaction threshold: tombstones above this percentage of a
/// language's page slots trigger a dense rebuild.
pub const DEFAULT_COMPACTION_PERCENT: f64 = 5.0;

/// A queue longer than this, and longer than a quarter of the index's page
/// slots, is applied by a full build rather than batch by batch.
pub const BACKLOG_FULL_BUILD: i64 = 1_000;

/// Where the documents of a run come from: the kernel's access seam in
/// production ([`super::pagefind::ItemDocuments`]), a fixture in the tests.
#[async_trait]
pub trait DocumentSource: Send + Sync {
    /// The searchable documents among `ids`, or of the whole site for `None`.
    /// An id that is not searchable (unpublished, deleted, switched out of
    /// search, hidden from anonymous visitors) is simply absent.
    async fn documents(&self, ids: Option<&[Uuid]>) -> Result<Vec<IndexDocument>>;
}

/// One slot's change, owned: the page number, the words its previous holder
/// posted (from the term cache), and its new terms.
type OwnedChange = (u32, Option<BTreeMap<String, Vec<i32>>>, Option<PageTerms>);

/// What one incremental update did.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct UpdateReport {
    /// Queue rows applied.
    pub queued: usize,
    /// Page slots that changed: filled, edited or freed.
    pub pages_changed: usize,
    /// Items whose page was already what it would be (same fragment HTML).
    pub unchanged: usize,
    /// Items tokenized.
    pub tokenized: usize,
    /// Languages rewritten.
    pub languages: Vec<String>,
    /// Files new to the live directory, the entry file included.
    pub files_written: usize,
    /// Word chunks re-encoded.
    pub chunks_rewritten: usize,
    /// Word chunks split past 40,000 locations.
    pub chunks_split: usize,
    /// Word chunks dropped because they emptied.
    pub chunks_dropped: usize,
    /// The generation published; unchanged when nothing was.
    pub generation: i64,
    /// Milliseconds from start to commit.
    pub total_ms: u128,
}

/// How an incremental update ended.
#[doc(hidden)]
#[derive(Debug)]
pub enum UpdateOutcome {
    /// Published, or (no page changed) the queue rows were simply dropped.
    Applied(UpdateReport),
    /// A precondition failed; nothing was changed. The reason is logged.
    FullBuildNeeded(String),
}

/// The live entry file's languages: language -> (meta stem, page count).
fn live_entry(output_dir: &Path) -> Result<BTreeMap<String, (String, usize)>> {
    let path = output_dir.join(ENTRY_FILE);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let entry: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    let languages = entry
        .get("languages")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("{} names no languages", path.display()))?;
    let mut out = BTreeMap::new();
    for (language, value) in languages {
        let hash = value
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("language {language} has no hash"))?;
        let pages = value
            .get("page_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        out.insert(
            language.clone(),
            (hash.to_string(), usize::try_from(pages).unwrap_or_default()),
        );
    }
    Ok(out)
}

/// One state row.
#[derive(sqlx::FromRow)]
struct StateRow {
    generation: i64,
    pagefind_version: String,
    entry_meta: sqlx::types::Json<BTreeMap<String, String>>,
}

/// One chunk plan row.
#[derive(sqlx::FromRow)]
struct ChunkRow {
    first_word: String,
    last_word: String,
    locations: i64,
    hash: String,
}

/// One touched slot of one language: its number, its holder before and after.
type SlotChange = (u32, Option<Uuid>, Option<Uuid>);

/// Apply the queue rows `queued` (`(item_id, seq)`) to the published index
/// in `output_dir`. `documents` are the searchable documents among their
/// items, exported after `snapshot` ([`generation`]) was read. See the module
/// documentation.
///
/// Fails with [`RebuildAgain::Superseded`] (nothing changed, rows kept) when
/// another publisher published since `snapshot`, and with
/// [`RebuildAgain::Unrecorded`] when the new entry is live but its database
/// record did not commit (the next run's entry check takes a full build).
#[doc(hidden)]
pub async fn incremental_update(
    pool: &PgPool,
    output_dir: &Path,
    queued: &[(Uuid, i64)],
    documents: &[IndexDocument],
    snapshot: i64,
) -> Result<UpdateOutcome> {
    let started = Instant::now();
    let mut report = UpdateReport {
        queued: queued.len(),
        ..UpdateReport::default()
    };
    let ids: Vec<Uuid> = queued.iter().map(|(id, _)| *id).collect();
    let id_set: BTreeSet<Uuid> = ids.iter().copied().collect();

    // 1. Render, hash, and tokenize what changed.
    let cache_rows = sqlx::query_as::<_, TermRow>(
        "SELECT item_id, language, content_hash, word_count, fragment, words \
         FROM search_page_terms WHERE item_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .context("read the search term cache")?;
    let mut cache: HashMap<Uuid, TermRow> =
        cache_rows.into_iter().map(|r| (r.item_id, r)).collect();

    let mut terms: BTreeMap<Uuid, PageTerms> = BTreeMap::new();
    let mut same: BTreeSet<Uuid> = BTreeSet::new();
    let mut to_tokenize = Vec::new();
    for document in documents {
        if !id_set.contains(&document.id) {
            continue;
        }
        let html = render_fragment(document);
        let hash = Sha256::digest(html.as_bytes()).to_vec();
        match cache.get(&document.id) {
            Some(row) if row.content_hash == hash => {
                let words = PageTerms::words_from_cbor(&row.words)
                    .with_context(|| format!("decode the cached terms of {}", document.id))?;
                terms.insert(
                    document.id,
                    PageTerms {
                        language: row.language.clone(),
                        word_count: u32::try_from(row.word_count).unwrap_or_default(),
                        fragment: row.fragment.clone(),
                        words,
                    },
                );
                same.insert(document.id);
            }
            _ => to_tokenize.push((document.id, html, hash)),
        }
    }
    report.tokenized = to_tokenize.len();
    let tokenized = tokio::task::spawn_blocking(move || {
        to_tokenize
            .into_iter()
            .map(|(id, html, hash)| {
                trovato_search_index::tokenize(&page_url(id), &html).map(|t| (id, hash, t))
            })
            .collect::<Result<Vec<_>>>()
    })
    .await
    .context("the tokenizer task failed")??;
    let mut changed_terms: Vec<(Uuid, Vec<u8>)> = Vec::new();
    for (id, hash, page) in tokenized {
        if let Some(page) = page {
            terms.insert(id, page);
            changed_terms.push((id, hash));
        }
    }

    // 2. Under the lock: the state, the preconditions, the ledger.
    let mut tx = pool
        .begin()
        .await
        .context("begin the search index update")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(LOCK_KEY)
        .execute(&mut *tx)
        .await
        .context("take the search index lock")?;
    let state = sqlx::query_as::<_, StateRow>(
        "SELECT generation, pagefind_version, entry_meta FROM search_index_state WHERE id",
    )
    .fetch_optional(&mut *tx)
    .await
    .context("read the search index state")?;
    let Some(state) = state else {
        return Ok(UpdateOutcome::FullBuildNeeded(
            "no search index has been published yet".into(),
        ));
    };
    if state.generation != snapshot {
        return Err(RebuildAgain::Superseded {
            snapshot,
            current: state.generation,
        }
        .into());
    }
    if state.pagefind_version != PAGEFIND_VERSION {
        return Ok(UpdateOutcome::FullBuildNeeded(format!(
            "the published index was written by Pagefind {}, not the pinned {PAGEFIND_VERSION}",
            state.pagefind_version
        )));
    }
    let entry = match live_entry(output_dir) {
        Ok(entry) => entry,
        Err(e) => {
            return Ok(UpdateOutcome::FullBuildNeeded(format!(
                "the live index has no readable entry file: {e:#}"
            )));
        }
    };
    let entry_hashes: BTreeMap<String, String> = entry
        .iter()
        .map(|(l, (h, _))| (l.clone(), h.clone()))
        .collect();
    if entry_hashes != state.entry_meta.0 {
        return Ok(UpdateOutcome::FullBuildNeeded(format!(
            "the live entry names {entry_hashes:?} but the database recorded {:?}",
            state.entry_meta.0
        )));
    }

    let rows = sqlx::query_as::<_, LedgerRow>(
        "SELECT language, page_number, item_id FROM search_page ORDER BY language, page_number",
    )
    .fetch_all(&mut *tx)
    .await
    .context("read the search page ledger")?;
    let mut before: BTreeMap<String, BTreeMap<i32, Option<Uuid>>> = BTreeMap::new();
    let mut wanted: BTreeMap<Uuid, String> = BTreeMap::new();
    for row in &rows {
        before
            .entry(row.language.clone())
            .or_default()
            .insert(row.page_number, row.item_id);
        if let Some(item) = row.item_id
            && !id_set.contains(&item)
        {
            wanted.insert(item, row.language.clone());
        }
    }
    for (id, page) in &terms {
        wanted.insert(*id, page.language.clone());
    }
    let plan = plan_ledger(&rows, &wanted);
    if let Some(language) = plan.drop_languages.first() {
        return Ok(UpdateOutcome::FullBuildNeeded(format!(
            "the batch leaves no page in the {language} index"
        )));
    }
    if let Some(language) = plan.slots.keys().find(|l| !entry.contains_key(*l)) {
        return Ok(UpdateOutcome::FullBuildNeeded(format!(
            "the batch puts a page in {language}, which has no index yet"
        )));
    }

    // Which slots changed, per language.
    let mut changes: BTreeMap<String, Vec<SlotChange>> = BTreeMap::new();
    for (language, slots) in &plan.slots {
        let was_slots = before.get(language);
        for (page, holder) in slots.iter().enumerate() {
            let page = i32::try_from(page).context("too many search pages")?;
            let was = was_slots.and_then(|m| m.get(&page)).copied().flatten();
            let edited = holder.is_some_and(|id| id_set.contains(&id) && !same.contains(&id));
            if was != *holder || edited {
                changes.entry(language.clone()).or_default().push((
                    page.unsigned_abs(),
                    was,
                    *holder,
                ));
            }
        }
    }
    report.unchanged = same
        .iter()
        .filter(|id| !changes.values().flatten().any(|(_, _, h)| h == &Some(**id)))
        .count();
    report.pages_changed = changes.values().map(Vec::len).sum();

    if changes.is_empty() {
        // Every queued item's page is already what it would be.
        delete_drained(&mut tx, queued).await?;
        tx.commit()
            .await
            .context("commit the drained search index queue")?;
        report.generation = state.generation;
        report.total_ms = started.elapsed().as_millis();
        return Ok(UpdateOutcome::Applied(report));
    }

    // 3. The updater, per changed language.
    let mut updates: Vec<LanguageUpdate> = Vec::new();
    for (language, slot_changes) in &changes {
        let Some((meta_hash, _)) = entry.get(language) else {
            continue;
        };
        let meta = match read_meta(output_dir, meta_hash) {
            Ok(meta) => meta,
            Err(e) => {
                return Ok(UpdateOutcome::FullBuildNeeded(format!(
                    "the {language} meta file does not read: {e:#}"
                )));
            }
        };
        let slots_before = before.get(language).map_or(0, BTreeMap::len);
        if meta.pages.len() != slots_before {
            return Ok(UpdateOutcome::FullBuildNeeded(format!(
                "the {language} ledger has {slots_before} slots but its meta file {}",
                meta.pages.len()
            )));
        }
        let plan_rows = sqlx::query_as::<_, ChunkRow>(
            "SELECT first_word, last_word, locations, hash FROM search_chunk \
             WHERE language = $1 ORDER BY ordinal",
        )
        .bind(language)
        .fetch_all(&mut *tx)
        .await
        .context("read the search chunk plan")?;
        let chunk_plan: Vec<ChunkPlan> = plan_rows
            .into_iter()
            .map(|r| ChunkPlan {
                first: r.first_word,
                last: r.last_word,
                locations: u64::try_from(r.locations).unwrap_or_default(),
                hash: r.hash,
            })
            .collect();

        // The words each previous holder posted, from the term cache.
        let mut owned: Vec<OwnedChange> = Vec::with_capacity(slot_changes.len());
        for (page, was, now) in slot_changes {
            let old = match was {
                None => None,
                Some(id) => match cache.remove(id) {
                    Some(row) if row.language == *language => Some(
                        PageTerms::words_from_cbor(&row.words)
                            .with_context(|| format!("decode the cached terms of {id}"))?,
                    ),
                    _ => {
                        return Ok(UpdateOutcome::FullBuildNeeded(format!(
                            "item {id} holds page {page} of {language} but has no cached terms \
                             there"
                        )));
                    }
                },
            };
            let new = now.and_then(|id| terms.get(&id)).cloned();
            owned.push((*page, old, new));
        }
        let live = output_dir.to_path_buf();
        let language_key = language.clone();
        let result = tokio::task::spawn_blocking(move || {
            let changes: Vec<PageChange<'_>> = owned
                .iter()
                .map(|(page, old, new)| PageChange {
                    page_number: *page,
                    old: old.as_ref(),
                    new: new.as_ref(),
                })
                .collect();
            update_language(&live, &language_key, &meta, &chunk_plan, &changes)
        })
        .await
        .context("the index updater task failed")?;
        match result {
            Ok(update) => updates.push(update),
            Err(e) => {
                return Ok(UpdateOutcome::FullBuildNeeded(format!(
                    "the {language} index cannot be updated in place: {e:#}"
                )));
            }
        }
    }

    // 4. Stage: the new files, every meta the entry names, the entry.
    let mut bundle = Bundle::new();
    let mut entry_rows: Vec<(String, String, usize)> = Vec::new();
    for (language, (hash, pages)) in &entry {
        match updates.iter().find(|u| &u.language == language) {
            Some(update) => {
                bundle.extend(update.files.clone());
                let meta = format!("pagefind.{}.pf_meta", update.meta_hash);
                if !bundle.contains_key(&meta) {
                    bundle.insert(
                        meta.clone(),
                        std::fs::read(output_dir.join(&meta))
                            .with_context(|| format!("read {meta}"))?,
                    );
                }
                entry_rows.push((
                    language.clone(),
                    update.meta_hash.clone(),
                    update.page_count,
                ));
            }
            None => {
                let meta = format!("pagefind.{hash}.pf_meta");
                bundle.insert(
                    meta.clone(),
                    std::fs::read(output_dir.join(&meta))
                        .with_context(|| format!("read {meta}"))?,
                );
                entry_rows.push((language.clone(), hash.clone(), *pages));
            }
        }
    }
    let entry_refs: Vec<(&str, &str, usize)> = entry_rows
        .iter()
        .map(|(l, h, p)| (l.as_str(), h.as_str(), *p))
        .collect();
    bundle.insert(
        ENTRY_FILE.to_string(),
        entry_json_for(&entry_refs)?.into_bytes(),
    );
    let suffix = Uuid::now_v7().simple().to_string();
    let staging = staging_dir_for(output_dir, &suffix);
    let _guard = StagingGuard(staging.clone());
    {
        let staging = staging.clone();
        tokio::task::spawn_blocking(move || write_bundle(&bundle, &staging))
            .await
            .context("the staging task failed")??;
    }

    // 5. Record: ledger, terms, chunk plans, generation, queue.
    apply_ledger_plan(&mut tx, &plan).await?;
    let gone: Vec<Uuid> = ids
        .iter()
        .filter(|id| !terms.contains_key(id))
        .copied()
        .collect();
    store_some_terms(&mut tx, &terms, &changed_terms, &gone).await?;
    let updated_languages: Vec<String> = updates.iter().map(|u| u.language.clone()).collect();
    store_chunk_plans(
        &mut tx,
        Some(&updated_languages),
        updates
            .iter()
            .map(|u| (u.language.as_str(), u.plan.as_slice())),
    )
    .await?;
    let entry_meta: BTreeMap<&str, &str> = entry_rows
        .iter()
        .map(|(l, h, _)| (l.as_str(), h.as_str()))
        .collect();
    let bumped = sqlx::query(
        "UPDATE search_index_state \
         SET generation = generation + 1, entry_meta = $2, published_at = now() \
         WHERE id AND generation = $1",
    )
    .bind(state.generation)
    .bind(sqlx::types::Json(&entry_meta))
    .execute(&mut *tx)
    .await
    .context("bump the search index generation")?;
    if bumped.rows_affected() != 1 {
        return Err(RebuildAgain::Superseded {
            snapshot,
            current: -1,
        }
        .into());
    }
    delete_drained(&mut tx, queued).await?;
    report.generation = state.generation + 1;
    report.languages = updated_languages;
    report.chunks_rewritten = updates.iter().map(|u| u.chunks_rewritten).sum();
    report.chunks_split = updates.iter().map(|u| u.chunks_split).sum();
    report.chunks_dropped = updates.iter().map(|u| u.chunks_dropped).sum();

    // 6. Publish: data files, metas, then the entry by rename.
    let published = {
        let staging = staging.clone();
        let live = output_dir.to_path_buf();
        tokio::task::spawn_blocking(move || publish(&staging, &live, &suffix))
            .await
            .context("the publish task failed")??
    };
    report.files_written = published.written + 1;

    // The entry is live from here: a failure leaves the database behind it,
    // which the next run's entry check turns into a full build.
    let referenced = {
        let live = output_dir.to_path_buf();
        tokio::task::spawn_blocking(move || published_files(&live))
            .await
            .context("the reference scan failed")?
            .map_err(RebuildAgain::Unrecorded)?
    };
    sweep(&mut tx, output_dir, &referenced)
        .await
        .map_err(RebuildAgain::Unrecorded)?;
    tx.commit().await.map_err(|e| {
        RebuildAgain::Unrecorded(anyhow::Error::new(e).context("commit the search index update"))
    })?;
    report.total_ms = started.elapsed().as_millis();
    info!(
        queued = report.queued,
        pages_changed = report.pages_changed,
        unchanged = report.unchanged,
        tokenized = report.tokenized,
        languages = ?report.languages,
        files_written = report.files_written,
        chunks_rewritten = report.chunks_rewritten,
        generation = report.generation,
        total_ms = report.total_ms,
        "search index updated incrementally"
    );
    Ok(UpdateOutcome::Applied(report))
}

/// Store the terms of the items tokenized this update, and drop the cache rows
/// of queued items that are no longer searchable.
async fn store_some_terms(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    terms: &BTreeMap<Uuid, PageTerms>,
    changed: &[(Uuid, Vec<u8>)],
    gone: &[Uuid],
) -> Result<()> {
    if !gone.is_empty() {
        sqlx::query("DELETE FROM search_page_terms WHERE item_id = ANY($1)")
            .bind(gone)
            .execute(&mut **tx)
            .await
            .context("drop search terms of items no longer searchable")?;
    }
    if changed.is_empty() {
        return Ok(());
    }
    let mut ids = Vec::with_capacity(changed.len());
    let mut languages = Vec::with_capacity(changed.len());
    let mut hashes = Vec::with_capacity(changed.len());
    let mut word_counts = Vec::with_capacity(changed.len());
    let mut fragments = Vec::with_capacity(changed.len());
    let mut words = Vec::with_capacity(changed.len());
    for (id, hash) in changed {
        let Some(page) = terms.get(id) else {
            continue;
        };
        ids.push(*id);
        languages.push(page.language.clone());
        hashes.push(hash.clone());
        word_counts.push(i32::try_from(page.word_count).unwrap_or(i32::MAX));
        fragments.push(page.fragment.clone());
        words.push(page.words_cbor()?);
    }
    sqlx::query(
        "INSERT INTO search_page_terms \
             (item_id, language, content_hash, word_count, fragment, words) \
         SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::bytea[], $4::int[], $5::text[], \
                              $6::bytea[]) \
         ON CONFLICT (item_id) DO UPDATE SET \
             language = EXCLUDED.language, content_hash = EXCLUDED.content_hash, \
             word_count = EXCLUDED.word_count, fragment = EXCLUDED.fragment, \
             words = EXCLUDED.words",
    )
    .bind(&ids)
    .bind(&languages)
    .bind(&hashes)
    .bind(&word_counts)
    .bind(&fragments)
    .bind(&words)
    .execute(&mut **tx)
    .await
    .context("store search terms")?;
    Ok(())
}

/// What a cron run asks of [`run_index`].
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Take a full build first, for this reason (an operator asked).
    pub full_build: Option<String>,
    /// Compact when tombstones exceed this percentage of a language's slots.
    pub compaction_percent: f64,
    /// Queue rows per incremental update.
    pub batch_size: usize,
    /// Most incremental updates this run.
    pub max_batches: usize,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            full_build: None,
            compaction_percent: DEFAULT_COMPACTION_PERCENT,
            batch_size: BATCH_SIZE,
            max_batches: MAX_BATCHES_PER_RUN,
        }
    }
}

/// What one cron run did to the index.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct RunReport {
    /// Full builds taken, each with its reason.
    pub full_builds: Vec<(String, FullBuildReport)>,
    /// Incremental updates applied.
    pub updates: Vec<UpdateReport>,
    /// Whether another publisher got in first; the queue is left for the
    /// next run.
    pub superseded: bool,
}

impl RunReport {
    /// Whether anything was published or drained.
    pub fn did_work(&self) -> bool {
        !self.full_builds.is_empty() || !self.updates.is_empty()
    }
}

/// Every queue row, oldest first, at most `limit` (all with `None`).
async fn read_queue(pool: &PgPool, limit: Option<usize>) -> Result<Vec<(Uuid, i64)>> {
    let limit = limit.map_or(i64::MAX, |l| i64::try_from(l).unwrap_or(i64::MAX));
    let rows: Vec<(Uuid, i64)> =
        sqlx::query_as("SELECT item_id, seq FROM search_index_queue ORDER BY seq LIMIT $1")
            .bind(limit)
            .fetch_all(pool)
            .await
            .context("read the search index queue")?;
    Ok(rows)
}

/// One full build of the whole site, covering every queue row read before
/// its export. `Ok(None)` when another publisher got in first.
async fn full<S: DocumentSource>(
    pool: &PgPool,
    output_dir: &Path,
    source: &S,
    compact: bool,
) -> Result<Option<FullBuildReport>> {
    let drained = read_queue(pool, None).await?;
    let snapshot = generation(pool).await?;
    let documents = source.documents(None).await?;
    match full_build_with(
        pool,
        output_dir,
        &documents,
        snapshot,
        &FullBuildOptions { compact, drained },
    )
    .await
    {
        Ok(report) => Ok(Some(report)),
        Err(e) => match e.downcast_ref::<RebuildAgain>() {
            Some(RebuildAgain::Superseded { .. }) => {
                info!(reason = %e, "search index build superseded; the next run starts again");
                Ok(None)
            }
            _ => Err(e),
        },
    }
}

/// Tombstones and page slots per language.
async fn tombstone_ratios(pool: &PgPool) -> Result<Vec<(String, i64, i64)>> {
    sqlx::query_as(
        "SELECT language, count(*), count(*) FILTER (WHERE item_id IS NULL) \
         FROM search_page GROUP BY language ORDER BY language",
    )
    .fetch_all(pool)
    .await
    .context("count search page tombstones")
}

/// One cron run over the search index: the full build or the queue drain,
/// then compaction. See the module documentation.
#[doc(hidden)]
pub async fn run_index<S: DocumentSource>(
    pool: &PgPool,
    output_dir: &Path,
    source: &S,
    options: &RunOptions,
) -> Result<RunReport> {
    let mut report = RunReport::default();

    // A full build first when one is asked for or nothing is live.
    let published: Option<i64> =
        sqlx::query_scalar("SELECT generation FROM search_index_state WHERE id")
            .fetch_optional(pool)
            .await
            .context("read the search index state")?;
    let reason = match (&options.full_build, published) {
        (Some(reason), _) => Some(reason.clone()),
        (None, None) => Some("no search index has been published yet".to_string()),
        (None, Some(_)) if !output_dir.join(ENTRY_FILE).is_file() => Some(format!(
            "the live directory {} has no {ENTRY_FILE}",
            output_dir.display()
        )),
        _ => None,
    };
    if let Some(reason) = reason {
        info!(reason = %reason, "search index: full build");
        match full(pool, output_dir, source, false).await? {
            Some(build) => report.full_builds.push((reason, build)),
            None => {
                report.superseded = true;
                return Ok(report);
            }
        }
    } else {
        for _ in 0..options.max_batches {
            let batch = read_queue(pool, Some(options.batch_size)).await?;
            if batch.is_empty() {
                break;
            }
            let pending = crate::search::index_queue::pending(pool).await?;
            let slots: i64 = sqlx::query_scalar("SELECT count(*) FROM search_page")
                .fetch_one(pool)
                .await
                .context("count search pages")?;
            if pending > BACKLOG_FULL_BUILD && pending * 4 > slots {
                let reason = format!(
                    "{pending} items are queued, more than a quarter of the index's {slots} \
                     page slots; a full build is cheaper"
                );
                warn!(reason = %reason, "search index: full build instead of an update");
                if let Some(build) = full(pool, output_dir, source, false).await? {
                    report.full_builds.push((reason, build));
                } else {
                    report.superseded = true;
                }
                break;
            }
            let ids: Vec<Uuid> = batch.iter().map(|(id, _)| *id).collect();
            let snapshot = generation(pool).await?;
            let documents = source.documents(Some(&ids)).await?;
            let outcome = match incremental_update(pool, output_dir, &batch, &documents, snapshot)
                .await
            {
                Ok(outcome) => outcome,
                Err(e) => match e.downcast_ref::<RebuildAgain>() {
                    Some(RebuildAgain::Superseded { .. }) => {
                        info!(reason = %e, "search index update superseded; the batch stays queued");
                        report.superseded = true;
                        return Ok(report);
                    }
                    _ => return Err(e),
                },
            };
            match outcome {
                UpdateOutcome::Applied(update) => report.updates.push(update),
                UpdateOutcome::FullBuildNeeded(reason) => {
                    warn!(reason = %reason, "search index: full build instead of an update");
                    if let Some(build) = full(pool, output_dir, source, false).await? {
                        report.full_builds.push((reason, build));
                    } else {
                        report.superseded = true;
                    }
                    break;
                }
            }
        }
    }

    // Compaction, measured after the free list took what it could.
    let over: Vec<(String, f64)> = tombstone_ratios(pool)
        .await?
        .into_iter()
        .filter(|(_, slots, _)| *slots > 0)
        .map(|(language, slots, tombstones)| (language, tombstones as f64 / slots as f64))
        .filter(|(_, ratio)| *ratio * 100.0 > options.compaction_percent)
        .collect();
    if let Some((language, ratio)) = over.first() {
        let reason = format!(
            "tombstones are {:.1}% of the {language} index's page slots, over the {}% threshold",
            ratio * 100.0,
            options.compaction_percent
        );
        info!(language = %language, ratio = *ratio, threshold = options.compaction_percent, "search index: compacting");
        match full(pool, output_dir, source, true).await? {
            Some(build) => report.full_builds.push((reason, build)),
            None => report.superseded = true,
        }
    }
    Ok(report)
}
