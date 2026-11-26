//! A private static search index over the real item tables, for tests of the
//! writers that queue items for it (B218).
//!
//! The kernel's writers queue into `public.search_index_queue`, which every
//! test on the shared database shares. An [`Index`] has its own copy of the
//! index tables (ledger, term cache, state, files, chunk plan and queue) in a
//! schema of its own, ahead of `public` on its pool's `search_path`, and its
//! own output directory. [`Index::take`] moves the rows a test's write queued
//! out of the shared queue into its own; [`Index::run`] is one cron run of the
//! index over the items the test tracks, read through the kernel's own export
//! (`indexable_documents_for`, the access seam included); [`Index::search`]
//! reads the published index back and answers which result URLs a word finds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use trovato_kernel::content::ItemService;
use trovato_kernel::cron::{
    DocumentSource, IndexDocument, RunOptions, RunReport, indexable_documents_for, search_index_run,
};
use trovato_search_index::decode::{decode_bundle, read_published};
use uuid::Uuid;

use super::defer_cleanup;

/// The tables an index keeps, each copied into the private schema.
const TABLES: &[&str] = &[
    "search_page",
    "search_page_terms",
    "search_index_state",
    "search_index_file",
    "search_chunk",
    "search_index_queue",
];

/// One queued row: the item, its op (1 upsert, 2 delete) and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRow {
    pub item_id: Uuid,
    pub op: i16,
    pub reason: String,
}

/// A private index over the items a test tracks.
pub struct Index {
    /// The shared database.
    db: PgPool,
    /// A pool whose `search_path` starts with the private schema.
    pub pool: PgPool,
    /// The published index.
    pub out: PathBuf,
    items: Arc<ItemService>,
    tracked: Mutex<BTreeSet<Uuid>>,
}

impl Index {
    /// A fresh, empty index, dropped when the test ends.
    pub async fn new(db: &PgPool, items: &Arc<ItemService>) -> Self {
        let schema = format!("search_writer_{}", Uuid::now_v7().simple());
        // The schema name is a fresh UUID, never input.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(db)
            .await
            .expect("create the index schema");
        for table in TABLES {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "CREATE TABLE {schema}.{table} (LIKE public.{table} INCLUDING ALL)"
            )))
            .execute(db)
            .await
            .expect("copy an index table");
        }
        let options = (*db.connect_options())
            .clone()
            .options([("search_path", format!("{schema},public"))]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .expect("connect to the index schema");
        let out = std::env::temp_dir()
            .join(format!("b218-writer-{}", Uuid::now_v7().simple()))
            .join("pagefind");
        let cleanup_db = db.clone();
        let cleanup_pool = pool.clone();
        let cleanup_dir = out.parent().unwrap().to_path_buf();
        defer_cleanup(move || async move {
            cleanup_pool.close().await;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE"
            )))
            .execute(&cleanup_db)
            .await
            .ok();
            std::fs::remove_dir_all(&cleanup_dir).ok();
        });
        let index = Self {
            db: db.clone(),
            pool,
            out,
            items: Arc::clone(items),
            tracked: Mutex::new(BTreeSet::new()),
        };
        // The first run is the full build of nothing tracked yet, so every
        // later run is an update.
        let first = index.run().await;
        assert_eq!(
            first.full_builds.len(),
            1,
            "the first run builds: {first:?}"
        );
        index
    }

    /// Count `id` as one of this index's items.
    pub fn track(&self, id: Uuid) {
        self.tracked.lock().unwrap().insert(id);
    }

    /// Move the shared queue's rows for `ids` into this index's queue, and
    /// return them.
    pub async fn take(&self, ids: &[Uuid]) -> Vec<QueuedRow> {
        let rows: Vec<(Uuid, Option<String>, i16, String)> = sqlx::query_as(
            "DELETE FROM public.search_index_queue WHERE item_id = ANY($1) \
             RETURNING item_id, language, op, reason",
        )
        .bind(ids)
        .fetch_all(&self.db)
        .await
        .expect("take the queued rows");
        for (item_id, language, op, reason) in &rows {
            sqlx::query(
                "INSERT INTO search_index_queue (item_id, language, op, reason) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(item_id)
            .bind(language)
            .bind(op)
            .bind(reason)
            .execute(&self.pool)
            .await
            .expect("queue in the private index");
        }
        let mut rows: Vec<QueuedRow> = rows
            .into_iter()
            .map(|(item_id, _, op, reason)| QueuedRow {
                item_id,
                op,
                reason,
            })
            .collect();
        rows.sort_by_key(|r| r.item_id);
        rows
    }

    /// Whether the shared queue holds a row for `id`, and its reason.
    pub async fn shared_reason(&self, id: Uuid) -> Option<String> {
        sqlx::query_scalar("SELECT reason FROM public.search_index_queue WHERE item_id = $1")
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .expect("read the shared queue")
    }

    /// One cron run of the index.
    pub async fn run(&self) -> RunReport {
        search_index_run(
            &self.pool,
            &self.out,
            self,
            &RunOptions {
                // Never compact: these runs are about the updates.
                compaction_percent: 100.0,
                ..RunOptions::default()
            },
        )
        .await
        .expect("a search index run")
    }

    /// One cron run that must be exactly one incremental update, and drain the
    /// private queue; with nothing queued, a run that does nothing.
    pub async fn update(&self) -> RunReport {
        let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_queue")
            .fetch_one(&self.pool)
            .await
            .unwrap();
        let report = self.run().await;
        if queued == 0 {
            assert!(
                !report.did_work(),
                "nothing queued, nothing done: {report:?}"
            );
            return report;
        }
        assert!(
            report.full_builds.is_empty() && report.updates.len() == 1,
            "one incremental update, not a full build: {report:?}"
        );
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_queue")
            .fetch_one(&self.pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "the update drains the queue");
        report
    }

    /// The result URLs (each page's `url` meta) the published index finds for
    /// `word`, a single word, in `language`.
    pub fn search(&self, language: &str, word: &str) -> BTreeSet<String> {
        let stem = stem(language, word);
        let bundle = read_published(&self.out).expect("read the published index");
        let mut found = BTreeSet::new();
        for index in decode_bundle(&bundle).expect("decode the published index") {
            if index.language != language {
                continue;
            }
            for chunk in index.chunks.values() {
                for w in chunk.words.iter().filter(|w| w.word == stem) {
                    for page in &w.pages {
                        let hash = &index.meta.pages[page.page_number as usize].hash;
                        let fragment = &index.fragments[hash];
                        let url = fragment["meta"]["url"]
                            .as_str()
                            .or_else(|| fragment["url"].as_str())
                            .unwrap_or_default();
                        found.insert(url.to_string());
                    }
                }
            }
        }
        found
    }
}

#[async_trait::async_trait]
impl DocumentSource for Index {
    async fn documents(&self, ids: Option<&[Uuid]>) -> anyhow::Result<Vec<IndexDocument>> {
        let tracked: Vec<Uuid> = {
            let tracked = self.tracked.lock().unwrap();
            match ids {
                None => tracked.iter().copied().collect(),
                Some(ids) => ids
                    .iter()
                    .copied()
                    .filter(|id| tracked.contains(id))
                    .collect(),
            }
        };
        indexable_documents_for(&self.db, &self.items, Some(&tracked)).await
    }
}

/// `word` as the index holds it: through Pagefind's own tokenizer, so stemmed
/// when the language has a stemmer.
fn stem(language: &str, word: &str) -> String {
    let html = format!(
        "<html lang=\"{language}\"><body><main data-pagefind-body><p>{word}</p></main></body></html>"
    );
    let terms = trovato_search_index::tokenize("/stem", &html)
        .expect("tokenize")
        .expect("a word");
    let words: Vec<&String> = terms.words.keys().collect();
    assert_eq!(words.len(), 1, "{word} is one word: {words:?}");
    words[0].clone()
}

/// A word no other test writes, letters only (Pagefind splits at digits).
pub fn unique_word(what: &str) -> String {
    let tag: String = Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .rev()
        .take(12)
        .map(|c| match c.to_digit(16) {
            Some(d) if c.is_ascii_digit() => char::from(b'g' + d as u8),
            _ => c,
        })
        .collect();
    format!("zq{what}{tag}x")
}

/// The URLs as a set, for comparisons.
pub fn urls<const N: usize>(urls: [&str; N]) -> BTreeSet<String> {
    urls.iter().map(|u| u.to_string()).collect()
}

/// Rows by item, for comparisons that do not care about order.
pub fn by_item(rows: &[QueuedRow]) -> BTreeMap<Uuid, (i16, String)> {
    rows.iter()
        .map(|r| (r.item_id, (r.op, r.reason.clone())))
        .collect()
}
