//! The native search index: the page ledger, the term cache, and the full
//! build that publishes a Pagefind 1.3.0 bundle without the `pagefind` CLI.
//!
//! `docs/design/incremental-search-index.md` is the design; this is part 2 of
//! it, the full build. In order, [`full_build`]:
//!
//! 1. renders every searchable item's fragment ([`render_fragment`]) and
//!    hashes it (SHA-256). An item whose hash matches its term cache row
//!    (`search_page_terms`) reuses the cached terms; every other item is
//!    tokenized by the vendored Pagefind 1.3.0 tokenizer
//!    ([`trovato_search_index::tokenize()`]);
//! 2. in one transaction, under `pg_advisory_xact_lock(hashtext('search_index'))`
//!    (one publisher for the whole index, because `pagefind-entry.json` is
//!    shared by every language), numbers the pages through the ledger
//!    (`search_page`): an item keeps its page number for life, a page number
//!    no item holds any more becomes a tombstone, and a new item takes the
//!    lowest tombstone in its language before a new number is added;
//! 3. encodes every language's index into a staging directory beside
//!    `PAGEFIND_OUTPUT_DIR`, naming no file after a different one still on
//!    disk, bumps `search_index_state.generation` only if it is still the
//!    generation the caller read before exporting the documents (otherwise
//!    nothing changes and [`RebuildAgain`] asks for a fresh build), and
//!    publishes: data files first, `pagefind-entry.json` last by rename
//!    ([`trovato_search_index::publish::publish`]). A staged bundle without an
//!    entry file is refused before the live directory is touched;
//! 4. records every index file the new entry no longer reaches
//!    (`search_index_file`) and deletes those unreferenced for
//!    [`RETENTION_HOURS`], so a browser tab holding an older meta keeps finding
//!    its files.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use trovato_search_index::publish::{
    is_content_addressed, publish, staging_dir_for, unreferenced_files, write_bundle,
};
use trovato_search_index::{PAGEFIND_VERSION, PageTerms, Slot};

use super::pagefind::{IndexDocument, render_fragment};

/// How long an index file stays on disk after the published entry stops
/// reaching it.
pub const RETENTION_HOURS: i32 = 24;

/// The advisory lock every publisher takes, as `hashtext(LOCK_KEY)`.
pub(super) const LOCK_KEY: &str = "search_index";

/// What one full build did.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct FullBuildReport {
    /// Live pages published, across every language.
    pub pages: usize,
    /// Tombstone slots published.
    pub tombstones: usize,
    /// Each language published, with its slot count.
    pub languages: Vec<(String, usize)>,
    /// Items tokenized this build.
    pub tokenized: usize,
    /// Items whose cached terms were reused.
    pub cached: usize,
    /// Index files new to the live directory.
    pub written: usize,
    /// Index files already there under the same name.
    pub kept: usize,
    /// Unreferenced files deleted after the retention window.
    pub expired: usize,
    /// The generation published.
    pub generation: i64,
    /// Whether this build renumbered the pages densely (compaction).
    pub compacted: bool,
    /// Milliseconds rendering, hashing and tokenizing (cache misses only).
    pub tokenize_ms: u128,
    /// Milliseconds encoding the bundle.
    pub encode_ms: u128,
    /// Milliseconds staging, publishing and sweeping, under the lock.
    pub publish_ms: u128,
}

/// A build that ended without leaving the published index and the database in
/// agreement with the current content. Either way the fix is another full
/// build from fresh content, so the caller asks for one.
#[doc(hidden)]
#[derive(Debug, thiserror::Error)]
pub enum RebuildAgain {
    /// Another build published since this one read its starting generation
    /// (before it exported the documents), so its documents may be older than
    /// what is live. Nothing was changed.
    #[error(
        "the search index was published by another build since generation {snapshot} \
         (now {current}); this build is dropped"
    )]
    Superseded {
        /// The generation this build started from.
        snapshot: i64,
        /// The generation found under the lock.
        current: i64,
    },
    /// The new entry file is live but the transaction recording it (ledger,
    /// term cache, generation, retention) did not commit. The live index is
    /// complete and self-consistent; the ledger behind it is the previous one.
    #[error("the search index was published but not recorded: {0:#}")]
    Unrecorded(anyhow::Error),
}

/// The published generation, 0 before the first publish. A build reads it
/// before exporting its documents and passes it to [`full_build`], which
/// publishes only if no other build has published since.
#[doc(hidden)]
pub async fn generation(pool: &PgPool) -> Result<i64> {
    let generation: Option<i64> =
        sqlx::query_scalar("SELECT generation FROM search_index_state WHERE id")
            .fetch_optional(pool)
            .await
            .context("read the search index generation")?;
    Ok(generation.unwrap_or(0))
}

/// One term cache row.
#[derive(sqlx::FromRow)]
pub(super) struct TermRow {
    pub(super) item_id: Uuid,
    pub(super) language: String,
    pub(super) content_hash: Vec<u8>,
    pub(super) word_count: i32,
    pub(super) fragment: String,
    pub(super) words: Vec<u8>,
}

/// One ledger row.
#[derive(sqlx::FromRow, Debug, Clone)]
pub(super) struct LedgerRow {
    pub(super) language: String,
    pub(super) page_number: i32,
    pub(super) item_id: Option<Uuid>,
}

/// The URL Pagefind gives a page: its export file's path, `/<id>.html`, as
/// the CLI derived it from `<id>.html`. Result cards link through the `url`
/// meta, which carries the alias.
pub(super) fn page_url(id: Uuid) -> String {
    format!("/{id}.html")
}

/// The changes a full build makes to the ledger, and the slots it publishes.
#[derive(Debug, Default, PartialEq)]
pub(super) struct LedgerPlan {
    /// Items whose page numbers become tombstones.
    pub(super) free: Vec<Uuid>,
    /// Tombstones taken by an item: (language, page number, item).
    pub(super) reuse: Vec<(String, i32, Uuid)>,
    /// New page numbers: (language, page number, item).
    pub(super) insert: Vec<(String, i32, Uuid)>,
    /// Languages left with no live page, whose rows are deleted.
    pub(super) drop_languages: Vec<String>,
    /// Every published language's slots in page number order.
    pub(super) slots: BTreeMap<String, Vec<Option<Uuid>>>,
}

/// Number the wanted items (item, language) through the ledger rows.
///
/// An item keeps its page number while its language stays the same. Every
/// other page number it or a gone item held becomes a tombstone. New items, in
/// language then id order, each take the lowest tombstone in their language,
/// then numbers after the highest. A language with no live page left is
/// dropped from the ledger and not published.
pub(super) fn plan_ledger(rows: &[LedgerRow], wanted: &BTreeMap<Uuid, String>) -> LedgerPlan {
    let mut plan = LedgerPlan::default();
    // language -> page number -> holder
    let mut ledger: BTreeMap<String, BTreeMap<i32, Option<Uuid>>> = BTreeMap::new();
    let mut placed: HashMap<Uuid, (String, i32)> = HashMap::new();
    for row in rows {
        ledger
            .entry(row.language.clone())
            .or_default()
            .insert(row.page_number, row.item_id);
        if let Some(item) = row.item_id {
            placed.insert(item, (row.language.clone(), row.page_number));
        }
    }

    // Free every page held by an item that is gone or changed language.
    for (item, (language, page)) in &placed {
        if wanted.get(item) != Some(language) {
            plan.free.push(*item);
            if let Some(pages) = ledger.get_mut(language) {
                pages.insert(*page, None);
            }
        }
    }
    plan.free.sort();

    // Place every item that has no page in its language, lowest tombstone
    // first, in (language, id) order.
    let mut new_items: Vec<(&String, &Uuid)> = wanted
        .iter()
        .filter(|(item, language)| placed.get(*item).map(|(l, _)| l) != Some(*language))
        .map(|(item, language)| (language, item))
        .collect();
    new_items.sort();
    // Each language's free list (lowest first) and next new number, found once,
    // so placing n items costs O(n log n) rather than a scan per item.
    let mut free_lists: HashMap<String, (BTreeSet<i32>, i32)> = HashMap::new();
    for (language, item) in new_items {
        let pages = ledger.entry(language.clone()).or_default();
        let (free, next) = free_lists.entry(language.clone()).or_insert_with(|| {
            let free = pages
                .iter()
                .filter(|(_, holder)| holder.is_none())
                .map(|(page, _)| *page)
                .collect();
            let next = pages.keys().next_back().map_or(0, |last| last + 1);
            (free, next)
        });
        match free.pop_first() {
            Some(page) => {
                pages.insert(page, Some(*item));
                plan.reuse.push((language.clone(), page, *item));
            }
            None => {
                let page = *next;
                *next += 1;
                pages.insert(page, Some(*item));
                plan.insert.push((language.clone(), page, *item));
            }
        }
    }

    for (language, pages) in ledger {
        if pages.values().all(Option::is_none) {
            plan.drop_languages.push(language);
            continue;
        }
        let last = pages.keys().next_back().copied().unwrap_or(0);
        let slots = (0..=last)
            .map(|page| pages.get(&page).copied().flatten())
            .collect();
        plan.slots.insert(language, slots);
    }
    plan
}

/// The site's default language as a Pagefind key, for an index with no pages.
/// Read on the build's own connection: the transaction already holds one, and
/// asking the pool for another while holding the publish lock can wait forever
/// on a small or busy pool.
async fn default_language(conn: &mut sqlx::PgConnection) -> Result<String> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM language WHERE is_default = true LIMIT 1")
            .fetch_optional(conn)
            .await
            .context("read the default language")?;
    Ok(id
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "en".to_string()))
}

/// Removes a staging directory when dropped, whatever happened.
pub(super) struct StagingGuard(pub(super) PathBuf);

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.0.exists()
            && let Err(e) = std::fs::remove_dir_all(&self.0)
        {
            debug!(error = %e, path = %self.0.display(), "failed to remove the search index staging directory");
        }
    }
}

/// Build the whole index from `documents` through the ledger and publish it
/// into `output_dir`. See the module documentation for the steps.
///
/// `snapshot` is [`generation`] as read before `documents` were exported. If
/// another build has published since, this one fails with
/// [`RebuildAgain::Superseded`] before changing anything, so an older export
/// never replaces a newer one.
#[doc(hidden)]
pub async fn full_build(
    pool: &PgPool,
    output_dir: &Path,
    documents: &[IndexDocument],
    snapshot: i64,
) -> Result<FullBuildReport> {
    full_build_with(
        pool,
        output_dir,
        documents,
        snapshot,
        &FullBuildOptions::default(),
    )
    .await
}

/// How a full build runs beyond the default.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct FullBuildOptions {
    /// Renumber every page densely, as a build from an empty ledger would:
    /// the tombstones go, the free list empties, and the live items take page
    /// numbers in (language, id) order. The output is then byte for byte a
    /// first build of the same content.
    pub compact: bool,
    /// Queue rows, `(item_id, seq)`, read before `documents` were exported:
    /// this build covers them, so it deletes them when it publishes.
    pub drained: Vec<(Uuid, i64)>,
}

/// [`full_build`] with options.
#[doc(hidden)]
pub async fn full_build_with(
    pool: &PgPool,
    output_dir: &Path,
    documents: &[IndexDocument],
    snapshot: i64,
    options: &FullBuildOptions,
) -> Result<FullBuildReport> {
    let mut report = FullBuildReport {
        compacted: options.compact,
        ..FullBuildReport::default()
    };
    let started = Instant::now();

    // 1. Render, hash, and tokenize what the term cache does not hold.
    let rendered: Vec<(Uuid, String, Vec<u8>)> = documents
        .iter()
        .map(|document| {
            let html = render_fragment(document);
            let hash = Sha256::digest(html.as_bytes()).to_vec();
            (document.id, html, hash)
        })
        .collect();
    let ids: Vec<Uuid> = rendered.iter().map(|(id, _, _)| *id).collect();

    let cached_version: Option<String> =
        sqlx::query_scalar("SELECT pagefind_version FROM search_index_state WHERE id")
            .fetch_optional(pool)
            .await
            .context("read the search index state")?;
    let mut cache: HashMap<Uuid, TermRow> = HashMap::new();
    if cached_version.as_deref() == Some(PAGEFIND_VERSION) {
        let rows = sqlx::query_as::<_, TermRow>(
            "SELECT item_id, language, content_hash, word_count, fragment, words \
             FROM search_page_terms WHERE item_id = ANY($1)",
        )
        .bind(&ids)
        .fetch_all(pool)
        .await
        .context("read the search term cache")?;
        cache = rows.into_iter().map(|row| (row.item_id, row)).collect();
    }

    let mut terms: BTreeMap<Uuid, PageTerms> = BTreeMap::new();
    let mut changed: Vec<(Uuid, Vec<u8>)> = Vec::new();
    let mut to_tokenize: Vec<(Uuid, String, Vec<u8>)> = Vec::new();
    for (id, html, hash) in rendered {
        match cache.remove(&id) {
            Some(row) if row.content_hash == hash => {
                let words = PageTerms::words_from_cbor(&row.words)
                    .with_context(|| format!("decode the cached terms of {id}"))?;
                terms.insert(
                    id,
                    PageTerms {
                        language: row.language,
                        word_count: u32::try_from(row.word_count).unwrap_or_default(),
                        fragment: row.fragment,
                        words,
                    },
                );
                report.cached += 1;
            }
            _ => to_tokenize.push((id, html, hash)),
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
    for (id, hash, page) in tokenized {
        // A page with no words is not in a Pagefind index at all.
        if let Some(page) = page {
            terms.insert(id, page);
            changed.push((id, hash));
        }
    }
    report.tokenize_ms = started.elapsed().as_millis();

    let wanted: BTreeMap<Uuid, String> = terms
        .iter()
        .map(|(id, page)| (*id, page.language.clone()))
        .collect();

    // 2. Under the lock: the generation, the ledger and the term cache.
    let mut tx = pool
        .begin()
        .await
        .context("begin the search index publish")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(LOCK_KEY)
        .execute(&mut *tx)
        .await
        .context("take the search index lock")?;
    sqlx::query(
        "INSERT INTO search_index_state (id, generation, pagefind_version, entry_meta) \
         VALUES (true, 0, $1, '{}'::jsonb) ON CONFLICT (id) DO NOTHING",
    )
    .bind(PAGEFIND_VERSION)
    .execute(&mut *tx)
    .await
    .context("create the search index state")?;
    let read_generation: i64 =
        sqlx::query_scalar("SELECT generation FROM search_index_state WHERE id")
            .fetch_one(&mut *tx)
            .await
            .context("read the search index generation")?;
    if read_generation != snapshot {
        return Err(RebuildAgain::Superseded {
            snapshot,
            current: read_generation,
        }
        .into());
    }

    let rows = sqlx::query_as::<_, LedgerRow>(
        "SELECT language, page_number, item_id FROM search_page ORDER BY language, page_number",
    )
    .fetch_all(&mut *tx)
    .await
    .context("read the search page ledger")?;
    let plan = if options.compact {
        // Every row goes; the plan numbers from an empty ledger.
        sqlx::query("DELETE FROM search_page")
            .execute(&mut *tx)
            .await
            .context("clear the search page ledger for compaction")?;
        plan_ledger(&[], &wanted)
    } else {
        plan_ledger(&rows, &wanted)
    };
    apply_ledger_plan(&mut tx, &plan).await?;
    store_terms(&mut tx, &terms, &changed).await?;

    // 3. Encode, stage, check the generation, publish.
    let mut slots = plan.slots;
    if slots.is_empty() {
        // Nothing searchable: publish an empty index rather than none, so the
        // browser finds an index and returns no results.
        slots.insert(default_language(&mut tx).await?, Vec::new());
    }
    report.languages = slots.iter().map(|(l, s)| (l.clone(), s.len())).collect();
    report.pages = wanted.len();
    report.tombstones = slots
        .values()
        .map(|s| s.iter().filter(|slot| slot.is_none()).count())
        .sum();

    let encode_started = Instant::now();
    let live = output_dir.to_path_buf();
    let (indexes, bundle) = tokio::task::spawn_blocking(move || {
        let numbered: BTreeMap<String, Vec<Slot<'_>>> = slots
            .iter()
            .map(|(language, slots)| {
                let slots = slots
                    .iter()
                    .map(|slot| match slot.and_then(|id| terms.get(&id)) {
                        Some(page) => Slot::Page(page),
                        None => Slot::Tombstone,
                    })
                    .collect();
                (language.clone(), slots)
            })
            .collect();
        // Named so that no file takes the name of a different one still on
        // disk from an earlier publish.
        trovato_search_index::build_bundle_for_live(&numbered, &live)
    })
    .await
    .context("the index encoder task failed")??;
    report.encode_ms = encode_started.elapsed().as_millis();

    let publish_started = Instant::now();
    let suffix = Uuid::now_v7().simple().to_string();
    let staging = staging_dir_for(output_dir, &suffix);
    let _guard = StagingGuard(staging.clone());
    {
        let staging = staging.clone();
        tokio::task::spawn_blocking(move || write_bundle(&bundle, &staging))
            .await
            .context("the staging task failed")??;
    }

    store_chunk_plans(
        &mut tx,
        None,
        indexes
            .iter()
            .map(|index| (index.language.as_str(), index.plan.as_slice())),
    )
    .await?;
    delete_drained(&mut tx, &options.drained).await?;

    let entry_meta: BTreeMap<&str, &str> = indexes
        .iter()
        .map(|index| (index.language.as_str(), index.meta_hash.as_str()))
        .collect();
    let bumped = sqlx::query(
        "UPDATE search_index_state \
         SET generation = generation + 1, pagefind_version = $2, entry_meta = $3, \
             published_at = now() \
         WHERE id AND generation = $1",
    )
    .bind(read_generation)
    .bind(PAGEFIND_VERSION)
    .bind(sqlx::types::Json(&entry_meta))
    .execute(&mut *tx)
    .await
    .context("bump the search index generation")?;
    if bumped.rows_affected() != 1 {
        bail!(
            "the search index was published by someone else since generation {read_generation}; \
             this build is dropped and the next one starts from the new state"
        );
    }
    report.generation = read_generation + 1;

    let published = {
        let staging = staging.clone();
        let live = output_dir.to_path_buf();
        tokio::task::spawn_blocking(move || publish(&staging, &live, &suffix))
            .await
            .context("the publish task failed")??
    };
    report.written = published.written;
    report.kept = published.kept;

    // 4. Retention: record what the new entry no longer reaches; delete what
    // has gone unreferenced for the window.
    // The entry is live from here on: a failure now leaves the database behind
    // the published index, which the caller mends with another build.
    report.expired = sweep(&mut tx, output_dir, &published.referenced)
        .await
        .map_err(RebuildAgain::Unrecorded)?;
    tx.commit().await.map_err(|e| {
        RebuildAgain::Unrecorded(anyhow::Error::new(e).context("commit the search index publish"))
    })?;
    report.publish_ms = publish_started.elapsed().as_millis();

    info!(
        pages = report.pages,
        tombstones = report.tombstones,
        languages = ?report.languages,
        tokenized = report.tokenized,
        cached = report.cached,
        written = report.written,
        kept = report.kept,
        expired = report.expired,
        generation = report.generation,
        compacted = report.compacted,
        tokenize_ms = report.tokenize_ms,
        encode_ms = report.encode_ms,
        publish_ms = report.publish_ms,
        total_ms = started.elapsed().as_millis(),
        "search index published"
    );
    Ok(report)
}

/// Write a ledger plan.
pub(super) async fn apply_ledger_plan(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    plan: &LedgerPlan,
) -> Result<()> {
    // Tombstones first: an item moving language must leave its old row (and
    // its UNIQUE item_id) before it takes a new one.
    if !plan.free.is_empty() {
        sqlx::query(
            "UPDATE search_page SET item_id = NULL, freed_at = now() WHERE item_id = ANY($1)",
        )
        .bind(&plan.free)
        .execute(&mut **tx)
        .await
        .context("free search pages")?;
    }
    if !plan.drop_languages.is_empty() {
        sqlx::query("DELETE FROM search_page WHERE language = ANY($1)")
            .bind(&plan.drop_languages)
            .execute(&mut **tx)
            .await
            .context("drop empty search languages")?;
    }
    let columns = |rows: &[(String, i32, Uuid)]| {
        let languages: Vec<String> = rows.iter().map(|(l, _, _)| l.clone()).collect();
        let pages: Vec<i32> = rows.iter().map(|(_, p, _)| *p).collect();
        let items: Vec<Uuid> = rows.iter().map(|(_, _, i)| *i).collect();
        (languages, pages, items)
    };
    if !plan.reuse.is_empty() {
        let (languages, pages, items) = columns(&plan.reuse);
        sqlx::query(
            "UPDATE search_page p SET item_id = u.item_id, freed_at = NULL \
             FROM UNNEST($1::text[], $2::int[], $3::uuid[]) AS u(language, page_number, item_id) \
             WHERE p.language = u.language AND p.page_number = u.page_number",
        )
        .bind(&languages)
        .bind(&pages)
        .bind(&items)
        .execute(&mut **tx)
        .await
        .context("reuse free search pages")?;
    }
    if !plan.insert.is_empty() {
        let (languages, pages, items) = columns(&plan.insert);
        sqlx::query(
            "INSERT INTO search_page (language, page_number, item_id) \
             SELECT * FROM UNNEST($1::text[], $2::int[], $3::uuid[])",
        )
        .bind(&languages)
        .bind(&pages)
        .bind(&items)
        .execute(&mut **tx)
        .await
        .context("add search pages")?;
    }
    Ok(())
}

/// Store the terms of every item tokenized this build, and drop the cache rows
/// of items no longer searchable.
async fn store_terms(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    terms: &BTreeMap<Uuid, PageTerms>,
    changed: &[(Uuid, Vec<u8>)],
) -> Result<()> {
    let live: Vec<Uuid> = terms.keys().copied().collect();
    sqlx::query("DELETE FROM search_page_terms WHERE item_id <> ALL($1)")
        .bind(&live)
        .execute(&mut **tx)
        .await
        .context("drop stale search terms")?;
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

/// Record the index files in `live` that the published entry no longer
/// reaches, and delete those unreferenced for [`RETENTION_HOURS`]. Returns how
/// many were deleted.
pub(super) async fn sweep(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    live: &Path,
    referenced: &BTreeSet<String>,
) -> Result<usize> {
    let unreferenced = {
        let live = live.to_path_buf();
        let referenced = referenced.clone();
        tokio::task::spawn_blocking(move || unreferenced_files(&live, &referenced))
            .await
            .context("the retention scan failed")??
    };
    // Rows for files that are referenced again, or gone, go; files newly
    // unreferenced are recorded now.
    sqlx::query("DELETE FROM search_index_file WHERE path <> ALL($1)")
        .bind(&unreferenced)
        .execute(&mut **tx)
        .await
        .context("forget referenced search index files")?;
    sqlx::query(
        "INSERT INTO search_index_file (path, unreferenced_at) \
         SELECT path, now() FROM UNNEST($1::text[]) AS u(path) \
         ON CONFLICT (path) DO NOTHING",
    )
    .bind(&unreferenced)
    .execute(&mut **tx)
    .await
    .context("record unreferenced search index files")?;
    let expired: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM search_index_file \
         WHERE unreferenced_at < now() - make_interval(hours => $1) ORDER BY path",
    )
    .bind(RETENTION_HOURS)
    .fetch_all(&mut **tx)
    .await
    .context("find expired search index files")?;
    let mut deleted = Vec::new();
    for path in &expired {
        // Only ever a content addressed file inside the output directory.
        if !is_content_addressed(path) || path.contains("..") || path.starts_with('/') {
            warn!(path = %path, "refusing to delete an unexpected search index path");
            continue;
        }
        match std::fs::remove_file(live.join(path)) {
            Ok(()) => deleted.push(path.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => deleted.push(path.clone()),
            Err(e) => {
                warn!(error = %e, path = %path, "failed to delete an expired search index file");
            }
        }
    }
    if !deleted.is_empty() {
        sqlx::query("DELETE FROM search_index_file WHERE path = ANY($1)")
            .bind(&deleted)
            .execute(&mut **tx)
            .await
            .context("forget deleted search index files")?;
    }
    Ok(deleted.len())
}

/// Replace the stored chunk plan of `only` (one language), or of every
/// language when `None`, with `plans`.
pub(super) async fn store_chunk_plans<'a>(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    only: Option<&[String]>,
    plans: impl Iterator<Item = (&'a str, &'a [trovato_search_index::ChunkPlan])>,
) -> Result<()> {
    match only {
        None => {
            sqlx::query("DELETE FROM search_chunk")
                .execute(&mut **tx)
                .await
        }
        Some(languages) => {
            sqlx::query("DELETE FROM search_chunk WHERE language = ANY($1)")
                .bind(languages)
                .execute(&mut **tx)
                .await
        }
    }
    .context("clear the search chunk plan")?;
    let mut languages = Vec::new();
    let mut ordinals = Vec::new();
    let mut firsts = Vec::new();
    let mut lasts = Vec::new();
    let mut locations = Vec::new();
    let mut hashes = Vec::new();
    for (language, plan) in plans {
        for (ordinal, chunk) in plan.iter().enumerate() {
            languages.push(language.to_string());
            ordinals.push(i32::try_from(ordinal).context("too many word chunks")?);
            firsts.push(chunk.first.clone());
            lasts.push(chunk.last.clone());
            locations.push(i64::try_from(chunk.locations).unwrap_or(i64::MAX));
            hashes.push(chunk.hash.clone());
        }
    }
    if languages.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO search_chunk (language, ordinal, first_word, last_word, locations, hash) \
         SELECT * FROM UNNEST($1::text[], $2::int[], $3::text[], $4::text[], $5::bigint[], \
                              $6::text[])",
    )
    .bind(&languages)
    .bind(&ordinals)
    .bind(&firsts)
    .bind(&lasts)
    .bind(&locations)
    .bind(&hashes)
    .execute(&mut **tx)
    .await
    .context("store the search chunk plan")?;
    Ok(())
}

/// Delete exactly the queue rows a publish covered: each `(item_id, seq)` it
/// read before exporting. A row queued again since has a new `seq` and stays.
pub(super) async fn delete_drained(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    drained: &[(Uuid, i64)],
) -> Result<u64> {
    if drained.is_empty() {
        return Ok(0);
    }
    let ids: Vec<Uuid> = drained.iter().map(|(id, _)| *id).collect();
    let seqs: Vec<i64> = drained.iter().map(|(_, seq)| *seq).collect();
    let deleted = sqlx::query(
        "DELETE FROM search_index_queue q \
         USING UNNEST($1::uuid[], $2::bigint[]) AS d(item_id, seq) \
         WHERE q.item_id = d.item_id AND q.seq = d.seq",
    )
    .bind(&ids)
    .bind(&seqs)
    .execute(&mut **tx)
    .await
    .context("delete the drained search index queue rows")?;
    Ok(deleted.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn row(language: &str, page: i32, item: Option<u128>) -> LedgerRow {
        LedgerRow {
            language: language.into(),
            page_number: page,
            item_id: item.map(id),
        }
    }

    fn wanted(items: &[(u128, &str)]) -> BTreeMap<Uuid, String> {
        items
            .iter()
            .map(|(n, l)| (id(*n), (*l).to_string()))
            .collect()
    }

    #[test]
    fn a_fresh_ledger_numbers_densely_in_id_order_per_language() {
        let plan = plan_ledger(&[], &wanted(&[(3, "en"), (1, "en"), (2, "de")]));
        assert_eq!(
            plan.insert,
            vec![
                ("de".into(), 0, id(2)),
                ("en".into(), 0, id(1)),
                ("en".into(), 1, id(3))
            ]
        );
        assert_eq!(plan.slots["en"], vec![Some(id(1)), Some(id(3))]);
        assert!(plan.free.is_empty() && plan.reuse.is_empty());
    }

    #[test]
    fn items_keep_their_numbers_and_gone_items_leave_tombstones() {
        let rows = [
            row("en", 0, Some(1)),
            row("en", 1, Some(2)),
            row("en", 2, Some(3)),
        ];
        let plan = plan_ledger(&rows, &wanted(&[(1, "en"), (3, "en")]));
        assert_eq!(plan.free, vec![id(2)]);
        assert_eq!(plan.slots["en"], vec![Some(id(1)), None, Some(id(3))]);
        assert!(plan.insert.is_empty());
    }

    #[test]
    fn a_new_item_takes_the_lowest_tombstone() {
        let rows = [
            row("en", 0, Some(1)),
            row("en", 1, None),
            row("en", 2, Some(3)),
            row("en", 3, None),
        ];
        let plan = plan_ledger(
            &rows,
            &wanted(&[(1, "en"), (3, "en"), (9, "en"), (8, "en")]),
        );
        // (en, 8) before (en, 9): id order.
        assert_eq!(
            plan.reuse,
            vec![("en".into(), 1, id(8)), ("en".into(), 3, id(9))]
        );
        assert!(plan.insert.is_empty());
    }

    /// Placing items must not scan the language's pages once per item: a fresh
    /// build of 200,000 pages in one language would be 2 * 10^10 holder
    /// checks, far past the 30 s test budget. Linear placement takes well under
    /// a second.
    #[test]
    fn placing_many_new_items_does_not_rescan_the_ledger_per_item() {
        const N: u128 = 200_000;
        let many: BTreeMap<Uuid, String> = (0..N).map(|n| (id(n), "en".to_string())).collect();
        let plan = plan_ledger(&[], &many);
        assert_eq!(plan.insert.len(), N as usize);
        assert!(
            plan.insert
                .iter()
                .enumerate()
                .all(|(i, (_, page, item))| *page as usize == i && *item == id(i as u128))
        );

        // And with half of a full ledger freed, the new items fill the
        // tombstones lowest first, then continue after the last page.
        let rows: Vec<LedgerRow> = (0..N)
            .map(|n| row("en", n as i32, (n % 2 == 0).then_some(n)))
            .collect();
        let mut wanted: BTreeMap<Uuid, String> = (0..N)
            .step_by(2)
            .map(|n| (id(n), "en".to_string()))
            .collect();
        wanted.extend((N..N + N / 2 + 1).map(|n| (id(n), "en".to_string())));
        let plan = plan_ledger(&rows, &wanted);
        assert_eq!(plan.reuse.len(), (N / 2) as usize);
        assert!(
            plan.reuse
                .iter()
                .enumerate()
                .all(|(i, (_, page, _))| *page as usize == 2 * i + 1)
        );
        assert_eq!(plan.insert, vec![("en".into(), N as i32, id(N + N / 2))]);
    }

    #[test]
    fn a_language_change_is_a_delete_and_an_upsert() {
        let rows = [row("en", 0, Some(1)), row("en", 1, Some(2))];
        let plan = plan_ledger(&rows, &wanted(&[(1, "en"), (2, "de")]));
        assert_eq!(plan.free, vec![id(2)]);
        assert_eq!(plan.insert, vec![("de".into(), 0, id(2))]);
        assert_eq!(plan.slots["en"], vec![Some(id(1)), None]);
        assert_eq!(plan.slots["de"], vec![Some(id(2))]);
    }

    #[test]
    fn a_language_with_no_live_page_is_dropped() {
        let rows = [
            row("en", 0, Some(1)),
            row("fr", 0, Some(2)),
            row("fr", 1, None),
        ];
        let plan = plan_ledger(&rows, &wanted(&[(1, "en")]));
        assert_eq!(plan.drop_languages, vec!["fr".to_string()]);
        assert!(!plan.slots.contains_key("fr"));
    }
}
