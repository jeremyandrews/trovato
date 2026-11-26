#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The three compatibility gates of `docs/design/incremental-search-index.md`
//! for the native full build (B217, part 2 of 3) and the incremental updater
//! (B218, part 3 of 3; see "Part 3" at the end of this file).
//!
//! 1. **Building twice gives byte identical output**, through the ledger: a
//!    fresh build and a cached one, a build with tombstones twice, and the same
//!    state rebuilt with the term cache emptied, file for file.
//! 2. **Decoded equality with a `pagefind` CLI build of the same corpus**:
//!    every page's words, locations, weights, word count and fragment, page
//!    numbers remapped by URL, in every language.
//! 3. **The stock `pagefind.js` returns the same results** from both indexes,
//!    run headless in Node over a fixed query set (single and multi-term,
//!    a phrase, prefixes, stemmed forms, punctuation, non-ASCII, a term only in
//!    comments, a term only in a title), plus the same set at 5% tombstones.
//!
//! The corpus is 60 constructed items, 45 English and 15 German, rendered by
//! the kernel's own `render_fragment`. The ledger lives in a schema of its own
//! per test (the four search index tables, copied with `LIKE`), so a cron
//! build elsewhere in the shared database never renumbers these pages.
//!
//! Gates 2 and 3 need the pinned `pagefind` 1.3.0 CLI (`PAGEFIND_BIN`, else
//! `pagefind` on `PATH`) and print `SKIPPED` without it; gate 3 also needs
//! `node` (`NODE_BIN`, else on `PATH`). CI installs both and sets
//! `TROVATO_REQUIRE_PAGEFIND_CLI=1`, which turns a skip into a failure.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{defer_cleanup, run_test, shared_app};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use trovato_kernel::cron::{
    DocumentSource, FullBuildReport, IndexDocument, IndexedField, RebuildAgain, RunOptions,
    RunReport, render_fragment, search_index_full_build, search_index_generation, search_index_run,
};
use trovato_kernel::search::index_queue::{Queued, enqueue};
use trovato_search_index::decode::{Bundle, PageView, decode_bundle, read_bundle, read_published};
use uuid::Uuid;

// =============================================================================
// The fixture corpus
// =============================================================================

const EN_TITLES: &[&str] = &[
    "RustConf keynote recap",
    "Searching a static site",
    "Postgres tuning for small servers",
    "The Quasar release notes",
    "Running a community conference",
    "Speakers wanted for the autumn meetup",
    "Café culture and remote work",
    "A naïve guide to stemming",
    "WebAssembly plugins in practice",
    "Open source governance",
    "Indexing comments with their items",
    "Zürich meetup schedule",
    "Writing for the web",
    "Conferences we loved this year",
    "Kernel threads and async runtimes",
];

const EN_SENTENCES: &[&str] = &[
    "The conference brought together speakers from open source projects across Europe.",
    "Our search index is built ahead of time and served as static files.",
    "Postgres handles the ledger, while the browser runs the search itself.",
    "Running the build twice must give the same bytes every time.",
    "Speakers talked about Rust, WebAssembly and the plugin runtime.",
    "A café near the venue hosted the evening social for attendees.",
    "Stemming lets a search for runs find a page about running.",
    "Contributors to rust-lang shared what the compiler team learned this year.",
    "The O'Reilly booth gave away books, e.g. on systems programming.",
    "Conferences in Zürich and Berlin sold out within a week.",
    "Plugins are compiled to WebAssembly and loaded by the kernel at start.",
    "An open source project needs clear governance and kind reviewers.",
    "The meetup runs every month and welcomes first-time speakers.",
    "Every searchable item becomes one page in its language's index.",
    "A naïve tokenizer splits on spaces; Pagefind also splits hyphenated words.",
    "The keynote covered async runtimes, threads and structured concurrency.",
    "Writers should keep paragraphs short and titles descriptive.",
    "We measured the rebuild on a corpus of twelve hundred pages.",
];

const EN_COMMENTS: &[&str] = &[
    "Great writeup, thanks for sharing the slides.",
    "Will the talks be recorded? I missed the second day.",
    "I flew in on a zeppelin, which was slower than expected.",
    "The café recommendation was spot on.",
    "Could you post the schedule for next year's conference?",
];

const DE_TITLES: &[&str] = &[
    "Konferenz in München",
    "Häuser am Fluss",
    "Bäume im Stadtpark",
    "Die Straße nach Zürich",
    "Freie Software in Behörden",
];

const DE_SENTENCES: &[&str] = &[
    "Die Konferenz brachte Entwickler aus vielen Ländern zusammen.",
    "Konferenzen in Berlin und Hamburg waren schnell ausverkauft.",
    "Die Häuser am Fluss wurden im neunzehnten Jahrhundert gebaut.",
    "Im Stadtpark stehen alte Bäume, manche über zweihundert Jahre alt.",
    "Die Straße führt durch kleine Dörfer und über einen Pass.",
    "Freie Software spart Behörden Lizenzkosten und schafft Unabhängigkeit.",
    "Ein Haus am Wasser braucht ein solides Fundament.",
    "Die Suche läuft vollständig im Browser des Besuchers.",
];

const DE_COMMENTS: &[&str] = &[
    "Vielen Dank für den Bericht!",
    "Gibt es die Vorträge auch als Aufzeichnung?",
];

/// A fixed id per fixture item, so the corpus is the same every run (and the
/// ledger, numbering new items in id order, numbers it the same way).
fn fixture_id(n: usize) -> Uuid {
    Uuid::from_u128(0x0194_17ad_0000_7000_8000_0000_0000_0000 + n as u128)
}

fn pick<'a>(bank: &[&'a str], n: usize) -> &'a str {
    bank[n % bank.len()]
}

/// The 60 fixture items: 45 English, 15 German.
fn fixture() -> Vec<IndexDocument> {
    let mut docs = Vec::new();
    for n in 0..60 {
        let german = n >= 45;
        let (titles, sentences, comments, language) = if german {
            (DE_TITLES, DE_SENTENCES, DE_COMMENTS, "de")
        } else {
            (EN_TITLES, EN_SENTENCES, EN_COMMENTS, "en")
        };
        let title = if n < titles.len() || german && n - 45 < titles.len() {
            pick(titles, n).to_string()
        } else {
            format!("{} ({})", pick(titles, n), n / titles.len())
        };
        let body: Vec<&str> = (0..3).map(|k| pick(sentences, n * 5 + k * 3)).collect();
        let mut fields = vec![IndexedField {
            name: "field_body".into(),
            weight: 1.0,
            text: body.join(" "),
        }];
        if n % 3 == 0 {
            fields.insert(
                0,
                IndexedField {
                    name: "field_summary".into(),
                    weight: 4.0,
                    text: pick(sentences, n + 1).to_string(),
                },
            );
        }
        if n % 5 == 0 {
            fields.push(IndexedField {
                name: "field_city".into(),
                weight: 2.0,
                text: if german { "Zürich" } else { "Berlin" }.into(),
            });
        }
        let comments = if n % 4 == 0 {
            vec![pick(comments, n / 4).to_string()]
        } else {
            Vec::new()
        };
        let id = fixture_id(n);
        docs.push(IndexDocument {
            id,
            item_type: if n % 2 == 0 { "page" } else { "conference" }.into(),
            title,
            language: language.into(),
            url: format!("/item/{id}"),
            fields,
            comments,
            description: pick(sentences, n + 2).to_string(),
            location: String::new(),
            event_dates: String::new(),
            created_display: format!("2026-{:02}-{:02}", 1 + n % 12, 1 + n % 28),
        });
    }
    docs
}

// =============================================================================
// Harness: an isolated ledger, builds, the CLI and Node
// =============================================================================

const LEDGER_TABLES: &[&str] = &[
    "search_page",
    "search_page_terms",
    "search_index_state",
    "search_index_file",
    "search_chunk",
    "search_index_queue",
];

/// A pool whose `search_path` puts a fresh copy of the four search index
/// tables ahead of `public`, dropped when the test ends.
async fn isolated_ledger(db: &PgPool) -> PgPool {
    isolated_ledger_with(db, 4).await
}

/// [`isolated_ledger`] with a pool of `connections`.
async fn isolated_ledger_with(db: &PgPool, connections: u32) -> PgPool {
    let schema = format!("search_gate_{}", Uuid::now_v7().simple());
    // The schema name is a fresh UUID, never input.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(db)
        .await
        .expect("create the gate schema");
    for table in LEDGER_TABLES {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE {schema}.{table} (LIKE public.{table} INCLUDING ALL)"
        )))
        .execute(db)
        .await
        .expect("copy a search index table");
    }
    let options = (*db.connect_options())
        .clone()
        .options([("search_path", format!("{schema},public"))]);
    let pool = PgPoolOptions::new()
        .max_connections(connections)
        .connect_with(options)
        .await
        .expect("connect to the gate schema");
    let cleanup_db = db.clone();
    let cleanup_pool = pool.clone();
    defer_cleanup(move || async move {
        cleanup_pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE"
        )))
        .execute(&cleanup_db)
        .await
        .ok();
    });
    pool
}

/// A fresh directory under the system temporary directory, removed at the end.
fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("b217-gate-{label}-{}", Uuid::now_v7().simple()));
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    let remove = dir.clone();
    defer_cleanup(move || async move {
        std::fs::remove_dir_all(&remove).ok();
    });
    dir
}

/// One full build of `docs` into `out`, as cron runs it: the generation read
/// first, then the build.
async fn build(
    pool: &PgPool,
    out: &Path,
    docs: &[IndexDocument],
) -> anyhow::Result<FullBuildReport> {
    let snapshot = search_index_generation(pool).await?;
    search_index_full_build(pool, out, docs, snapshot).await
}

/// One native full build into a new output directory, read back.
async fn native(pool: &PgPool, docs: &[IndexDocument]) -> (FullBuildReport, Bundle) {
    let out = scratch("native").join("pagefind");
    let report = build(pool, &out, docs)
        .await
        .expect("the native full build");
    (report, read_bundle(&out).expect("read the native bundle"))
}

/// The pinned CLI: `$PAGEFIND_BIN`, else `pagefind` on `PATH`. `None` (and a
/// `SKIPPED` line) without one, unless `TROVATO_REQUIRE_PAGEFIND_CLI` is set.
fn pagefind_cli(gate: &str) -> Option<PathBuf> {
    let found = std::env::var_os("PAGEFIND_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| which("pagefind"));
    if found.is_none() {
        assert!(
            std::env::var_os("TROVATO_REQUIRE_PAGEFIND_CLI").is_none(),
            "{gate}: TROVATO_REQUIRE_PAGEFIND_CLI is set but there is no pagefind CLI"
        );
        println!(
            "SKIPPED {gate}: no pagefind CLI (set PAGEFIND_BIN to a pagefind 1.3.0 binary or put one on PATH)"
        );
    }
    let bin = found?;
    let version = Command::new(&bin)
        .arg("--version")
        .output()
        .expect("pagefind --version");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("pagefind {}", trovato_search_index::PAGEFIND_VERSION),
        "the CLI must be the pinned release"
    );
    Some(bin)
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// The documents as the old cron path exported them, `<id>.html`, then the
/// CLI with its arguments: `--site <dir> --output-path <dir>/_pagefind`.
fn cli(bin: &Path, docs: &[IndexDocument]) -> Bundle {
    let site = scratch("site");
    for doc in docs {
        std::fs::write(site.join(format!("{}.html", doc.id)), render_fragment(doc))
            .expect("write a page");
    }
    let out = site.join("_pagefind");
    let output = Command::new(bin)
        .arg("--site")
        .arg(&site)
        .arg("--output-path")
        .arg(&out)
        .output()
        .expect("run the pagefind CLI");
    assert!(
        output.status.success(),
        "pagefind failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    read_bundle(&out).expect("read the CLI bundle")
}

/// Every page of a bundle by URL, with its language.
fn pages_by_url(bundle: &Bundle) -> BTreeMap<String, (String, PageView)> {
    let mut out = BTreeMap::new();
    for index in decode_bundle(bundle).expect("decode a bundle") {
        for (url, view) in index.pages().expect("pages") {
            assert!(
                out.insert(url.clone(), (index.language.clone(), view))
                    .is_none(),
                "{url} is in two languages"
            );
        }
    }
    out
}

/// The index files of a bundle (everything but the runtime).
fn index_files(bundle: &Bundle) -> BTreeMap<&str, &[u8]> {
    bundle
        .iter()
        .filter(|(name, _)| {
            name.as_str() == "pagefind-entry.json"
                || [".pf_meta", ".pf_index", ".pf_fragment", ".pf_filter"]
                    .iter()
                    .any(|ext| name.ends_with(ext))
        })
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect()
}

// =============================================================================
// Gate 1: byte identical, through the ledger
// =============================================================================

#[test]
fn gate_1_building_twice_gives_byte_identical_output() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let docs = fixture();

        let (first, a) = native(&pool, &docs).await;
        let (second, b) = native(&pool, &docs).await;
        println!(
            "gate 1: fresh build tokenized {}, cached {}; rebuild tokenized {}, cached {}; {} files",
            first.tokenized,
            first.cached,
            second.tokenized,
            second.cached,
            a.len()
        );
        assert_eq!(first.pages, 60);
        assert_eq!(first.tokenized, 60);
        assert_eq!(second.cached, 60, "the rebuild reads the term cache");
        assert_eq!(first.languages, vec![("de".into(), 15), ("en".into(), 45)]);
        assert_eq!(a, b, "a rebuild of the same state is byte identical");

        // Three items leave: their page numbers become tombstones, every other
        // page keeps its number, and the result is again stable.
        let gone: BTreeSet<usize> = [5, 17, 50].into();
        let fewer: Vec<IndexDocument> = docs
            .iter()
            .enumerate()
            .filter(|(n, _)| !gone.contains(n))
            .map(|(_, d)| d.clone())
            .collect();
        let (third, c) = native(&pool, &fewer).await;
        let (_, d) = native(&pool, &fewer).await;
        assert_eq!(third.tombstones, 3);
        assert_eq!(c, d, "a rebuild with tombstones is byte identical");
        let before = pages_by_url(&a);
        for (url, (lang, view)) in pages_by_url(&c) {
            let (old_lang, old) = &before[&url];
            assert_eq!(
                (&lang, view.page_number),
                (old_lang, old.page_number),
                "{url} kept its page"
            );
        }

        // The same state with the term cache emptied: every page tokenized
        // again, the same bytes.
        sqlx::query("DELETE FROM search_page_terms")
            .execute(&pool)
            .await
            .unwrap();
        let (fourth, e) = native(&pool, &fewer).await;
        assert_eq!(fourth.tokenized, 57);
        assert_eq!(
            index_files(&c),
            index_files(&e),
            "cached terms equal fresh ones"
        );
        assert_eq!(c, e);

        // A new item takes the lowest free page number in its language.
        let mut back = fewer.clone();
        back.push(docs[17].clone());
        let (_, f) = native(&pool, &back).await;
        let after = pages_by_url(&f);
        let url = format!("/{}.html", docs[17].id);
        assert_eq!(
            after[&url].1.page_number,
            before[&format!("/{}.html", docs[5].id)].1.page_number
        );
        println!(
            "gate 1: PASS ({} index files, byte identical across rebuilds)",
            index_files(&a).len()
        );
    });
}

/// Publishing into the same directory twice rewrites nothing, and the old
/// generation's files are kept, not deleted.
#[test]
fn a_republish_keeps_every_file_and_old_files_survive_the_window() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let docs = fixture();
        let out = scratch("live").join("pagefind");

        let first = build(&pool, &out, &docs).await.unwrap();
        let second = build(&pool, &out, &docs).await.unwrap();
        assert!(first.written > 0);
        assert_eq!(
            second.written, 0,
            "content addressed files are never rewritten"
        );
        assert_eq!(second.kept, first.written);
        assert_eq!(second.generation, first.generation + 1);

        // One item edited: its fragment and the chunks and meta it touches are
        // new; the superseded files stay on disk inside the retention window.
        let mut edited = docs.clone();
        edited[0].title = "RustConf keynote recap, revised".into();
        let old = read_bundle(&out).unwrap();
        let third = build(&pool, &out, &edited).await.unwrap();
        assert_eq!(third.expired, 0);
        let now = read_bundle(&out).unwrap();
        for name in old.keys() {
            assert!(
                now.contains_key(name),
                "{name} was deleted inside the window"
            );
        }
        let unreferenced: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_file")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(unreferenced > 0, "the superseded files are recorded");

        // Past the window they go.
        sqlx::query("UPDATE search_index_file SET unreferenced_at = now() - interval '25 hours'")
            .execute(&pool)
            .await
            .unwrap();
        let fourth = build(&pool, &out, &edited).await.unwrap();
        assert_eq!(fourth.expired as i64, unreferenced);
        let entry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("pagefind-entry.json")).unwrap())
                .unwrap();
        // Everything the live entry names is still there.
        decode_bundle(&read_bundle(&out).unwrap()).expect("the live bundle decodes");
        assert!(entry["languages"]["en"]["hash"].is_string());
    });
}

/// The URLs the live entry in `out` reaches.
fn live_urls(out: &Path) -> BTreeSet<String> {
    decode_bundle(&read_bundle(out).unwrap())
        .unwrap()
        .iter()
        .flat_map(|index| index.pages().unwrap().into_keys())
        .collect()
}

/// A slow build that exported its documents before a faster one published
/// must not publish over it: the slower export can hold an item the faster
/// one removed.
#[test]
fn a_build_that_exported_before_another_published_is_dropped() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let docs = fixture();
        let out = scratch("race").join("pagefind");
        build(&pool, &out, &docs).await.unwrap();
        let removed = format!("/{}.html", docs[0].id);
        assert!(live_urls(&out).contains(&removed));

        // The slow build reads the generation and exports, item 0 included.
        let slow_snapshot = search_index_generation(&pool).await.unwrap();
        let slow_docs = docs.clone();
        // The fast build exports after item 0 left search, and publishes.
        build(&pool, &out, &docs[1..]).await.unwrap();
        let entry = std::fs::read(out.join("pagefind-entry.json")).unwrap();

        let err = search_index_full_build(&pool, &out, &slow_docs, slow_snapshot)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<RebuildAgain>(),
                Some(RebuildAgain::Superseded { .. })
            ),
            "{err:#}"
        );
        assert_eq!(
            std::fs::read(out.join("pagefind-entry.json")).unwrap(),
            entry,
            "the newer publication stays"
        );
        assert!(
            !live_urls(&out).contains(&removed),
            "the removed item stays out"
        );
        let held: i64 = sqlx::query_scalar("SELECT count(*) FROM search_page WHERE item_id = $1")
            .bind(docs[0].id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(held, 0, "the ledger is the newer build's");
    });
}

/// When the entry is live but recording it fails, the build says so in a way
/// the caller turns into another rebuild, and the next build brings the
/// database and the live entry back into agreement.
#[test]
fn a_publish_that_is_not_recorded_asks_for_another_build() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let docs = fixture();
        let out = scratch("unrecorded").join("pagefind");
        build(&pool, &out, &docs).await.unwrap();

        // The retention bookkeeping fails after the flip.
        sqlx::query("ALTER TABLE search_index_file RENAME COLUMN unreferenced_at TO broken")
            .execute(&pool)
            .await
            .unwrap();
        let mut edited = docs.clone();
        edited[0].title = "RustConf keynote recap, revised".into();
        let err = build(&pool, &out, &edited).await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<RebuildAgain>(),
                Some(RebuildAgain::Unrecorded(_))
            ),
            "{err:#}"
        );

        sqlx::query("ALTER TABLE search_index_file RENAME COLUMN broken TO unreferenced_at")
            .execute(&pool)
            .await
            .unwrap();
        let report = build(&pool, &out, &edited).await.unwrap();
        let entry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("pagefind-entry.json")).unwrap())
                .unwrap();
        let recorded: serde_json::Value =
            sqlx::query_scalar("SELECT entry_meta FROM search_index_state WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap();
        for (language, meta) in recorded.as_object().unwrap() {
            assert_eq!(&entry["languages"][language]["hash"], meta);
        }
        assert_eq!(report.pages, docs.len());
    });
}

/// An empty index is built on the transaction's own connection: with a pool of
/// one, asking the pool for another while holding it would never return.
#[test]
fn an_empty_index_builds_through_a_pool_of_one() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger_with(&app.db, 1).await;
        let out = scratch("empty").join("pagefind");
        let report =
            tokio::time::timeout(std::time::Duration::from_secs(10), build(&pool, &out, &[]))
                .await
                .expect("the empty build finished")
                .unwrap();
        assert_eq!(report.pages, 0);
        let entry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("pagefind-entry.json")).unwrap())
                .unwrap();
        assert_eq!(entry["languages"].as_object().unwrap().len(), 1);
    });
}

// =============================================================================
// Gate 2: decoded equality with the CLI
// =============================================================================

#[test]
fn gate_2_decodes_equal_to_a_cli_build_of_the_same_corpus() {
    run_test(async {
        let Some(bin) = pagefind_cli("gate 2") else {
            return;
        };
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let docs = fixture();
        let (_, native_bundle) = native(&pool, &docs).await;
        let cli_bundle = cli(&bin, &docs);

        let entry = |b: &Bundle| -> serde_json::Value {
            serde_json::from_slice(&b["pagefind-entry.json"]).unwrap()
        };
        let (ours, theirs) = (entry(&native_bundle), entry(&cli_bundle));
        for lang in ["en", "de"] {
            assert_eq!(
                ours["languages"][lang]["wasm"],
                theirs["languages"][lang]["wasm"]
            );
            assert_eq!(
                ours["languages"][lang]["page_count"],
                theirs["languages"][lang]["page_count"]
            );
        }
        assert_eq!(
            ours["languages"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            {
                let mut keys: Vec<_> = theirs["languages"].as_object().unwrap().keys().collect();
                keys.sort();
                keys
            }
        );

        let (want, got) = (pages_by_url(&cli_bundle), pages_by_url(&native_bundle));
        assert_eq!(
            want.keys().collect::<Vec<_>>(),
            got.keys().collect::<Vec<_>>()
        );
        let (mut postings, mut positions) = (0, 0);
        for (url, (lang, page)) in &want {
            let (our_lang, ours) = &got[url];
            assert_eq!(lang, our_lang, "{url}: language");
            assert_eq!(page.word_count, ours.word_count, "{url}: word count");
            assert_eq!(page.fragment, ours.fragment, "{url}: fragment");
            assert_eq!(
                page.fragment_hash, ours.fragment_hash,
                "{url}: fragment file"
            );
            assert_eq!(
                page.words, ours.words,
                "{url}: words, locations and weights"
            );
            postings += page.words.len();
            positions += page
                .words
                .values()
                .map(|l| l.iter().filter(|x| **x >= 0).count())
                .sum::<usize>();
        }
        println!(
            "gate 2: PASS: {} pages in {:?}, {postings} postings, {positions} positions, 0 differences (page numbers remapped by URL)",
            want.len(),
            ours["languages"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
    });
}

// =============================================================================
// Gate 3: the stock pagefind.js returns the same results
// =============================================================================

const EN_QUERIES: &[&str] = &[
    "conference",
    "conferences",
    "speakers",
    "speaker",
    "running",
    "runs",
    "rust",
    "rust webassembly",
    "\"open source\"",
    "\"search index\"",
    "conf",
    "post",
    "web",
    "café",
    "zürich",
    "naïve",
    "quasar",
    "zeppelin",
    "rust-lang",
    "o'reilly",
    "plugins kernel",
    "berlin",
    "the",
];

const DE_QUERIES: &[&str] = &[
    "konferenz",
    "konferenzen",
    "häuser",
    "haus",
    "bäume",
    "straße",
    "zürich",
    "freie software",
    "\"freie software\"",
    "konf",
];

/// The queries `pagefind_web` 1.3.0 itself fails on, on any index of these
/// pages, as (language, query). It cuts a query term at a byte length to
/// compare it with chunk bounds when no chunk matches exactly
/// (`pagefind_web/src/lib.rs`, lines 232-236), which panics inside a multi-byte
/// character. Only these may fail, and they must fail on both indexes; any
/// other failure fails the gate. A Pagefind bump that fixes it shows up here.
const UPSTREAM_FAILURES: &[(&str, &str)] = &[("de", "zürich")];

/// At least this many queries must return results on both sides, so the gate
/// compares results and not, say, the same failure everywhere.
const MIN_QUERIES_WITH_RESULTS: usize = 20;

/// Whether a query's results are the pseudo result for a failed search.
fn failed(results: &[(String, f64)]) -> bool {
    results
        .first()
        .is_some_and(|(url, _)| url.starts_with("ERROR: "))
}

/// Fail unless a query that failed on either side is a known upstream failure
/// that failed on both.
fn check_failure(language: &str, query: &str, ours: &[(String, f64)], theirs: &[(String, f64)]) {
    if failed(ours) || failed(theirs) {
        assert!(
            UPSTREAM_FAILURES.contains(&(language, query)),
            "[{language}] {query}: the search failed ({ours:?} / {theirs:?}), which is not a known pagefind_web failure"
        );
        assert!(
            failed(ours) && failed(theirs),
            "[{language}] {query}: failed on one index only ({ours:?} / {theirs:?})"
        );
    }
}

/// One result: (url, score).
type Results = BTreeMap<String, Vec<(String, f64)>>;

fn node() -> Option<PathBuf> {
    std::env::var_os("NODE_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| which("node"))
}

/// Run the query set through the bundle's own `pagefind.js` in Node.
fn run_queries(node: &Path, bundle: &Bundle, language: &str, queries: &[&str]) -> Results {
    let dir = scratch("bundle");
    for (name, bytes) in bundle {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let queries_file = dir.join("queries.json");
    std::fs::write(&queries_file, serde_json::to_vec(queries).unwrap()).unwrap();
    let script =
        common::project_root().join("crates/kernel/tests/fixtures/search_index_gate/query.mjs");
    let output = Command::new(node)
        .arg(&script)
        .arg(&dir)
        .arg(language)
        .arg(&queries_file)
        .output()
        .expect("run node");
    assert!(
        output.status.success(),
        "query.mjs failed on {} ({language}): {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("query.mjs output");
    assert_eq!(
        parsed["missing"],
        serde_json::json!([]),
        "the browser asked for missing files"
    );
    let mut out = Results::new();
    for (query, results) in parsed["results"].as_object().unwrap() {
        // A query the search backend failed on is one pseudo result naming the
        // error, so both sides must fail the same way to compare equal.
        let results = match results.as_array() {
            Some(results) => results
                .iter()
                .map(|r| {
                    (
                        r["url"].as_str().unwrap().to_string(),
                        r["score"].as_f64().unwrap(),
                    )
                })
                .collect(),
            None => vec![(
                format!("ERROR: {}", results["error"].as_str().unwrap_or("?")),
                -1.0,
            )],
        };
        out.insert(query.clone(), results);
    }
    out
}

/// Results with equal scores as one group: Pagefind orders a tie by page
/// number, which is build order, so within a tie the order says nothing.
fn tie_groups(results: &[(String, f64)]) -> Vec<(f64, BTreeSet<String>)> {
    let mut groups: Vec<(f64, BTreeSet<String>)> = Vec::new();
    for (url, score) in results {
        match groups.last_mut() {
            Some((s, urls)) if *s == *score => {
                urls.insert(url.clone());
            }
            _ => groups.push((*score, BTreeSet::from([url.clone()]))),
        }
    }
    groups
}

/// How many results in `ours` sit outside the positions their tie group
/// holds in `theirs`: a real reordering, not a different order inside a tie
/// (which is page number order, so build order, on either side).
fn moved_positions(ours: &[(String, f64)], theirs: &[(String, f64)]) -> usize {
    let mut range: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut start = 0;
    for (_, urls) in tie_groups(theirs) {
        let end = start + urls.len();
        for url in urls {
            let url: &str = theirs
                .iter()
                .find(|(u, _)| *u == url)
                .map(|(u, _)| u.as_str())
                .unwrap_or_default();
            range.insert(url, (start, end));
        }
        start = end;
    }
    ours.iter()
        .enumerate()
        .filter(|(i, (url, _))| {
            !range
                .get(url.as_str())
                .is_some_and(|(s, e)| s <= i && i < e)
        })
        .count()
}

/// A short label for a result URL in the table: the fixture item's number.
fn label(url: &str) -> String {
    let id = url.trim_start_matches("/item/");
    Uuid::parse_str(id)
        .ok()
        .map(|u| format!("#{}", u.as_u128() - fixture_id(0).as_u128()))
        .unwrap_or_else(|| url.to_string())
}

fn labels(results: &[(String, f64)]) -> String {
    results
        .iter()
        .map(|(u, _)| label(u))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn gate_3_the_stock_pagefind_js_returns_the_same_results() {
    run_test(async {
        let Some(bin) = pagefind_cli("gate 3") else {
            return;
        };
        let Some(node) = node() else {
            assert!(
                std::env::var_os("TROVATO_REQUIRE_PAGEFIND_CLI").is_none(),
                "gate 3: TROVATO_REQUIRE_PAGEFIND_CLI is set but there is no node"
            );
            println!("SKIPPED gate 3: no node (set NODE_BIN or put node on PATH)");
            return;
        };
        let app = shared_app().await;
        let docs = fixture();

        // Dense: a fresh ledger against the CLI on the same pages.
        let pool = isolated_ledger(&app.db).await;
        let (_, native_bundle) = native(&pool, &docs).await;
        let cli_bundle = cli(&bin, &docs);
        // The site before this change: the same pages without `lang`, as the
        // old export wrote them, through the CLI.
        let unlanguaged: Vec<IndexDocument> = docs
            .iter()
            .cloned()
            .map(|mut d| {
                d.language = String::new();
                d
            })
            .collect();
        let old_bundle = cli(&bin, &unlanguaged);

        let mut queries_run = 0;
        let (mut with_results, mut upstream_failed) = (0, 0);
        let mut differences_from_old = Vec::new();
        println!(
            "gate 3, dense: query | native results | CLI | equal | before this change (no lang)"
        );
        for (language, queries) in [("en", EN_QUERIES), ("de", DE_QUERIES)] {
            let ours = run_queries(&node, &native_bundle, language, queries);
            let theirs = run_queries(&node, &cli_bundle, language, queries);
            let old = run_queries(&node, &old_bundle, language, queries);
            for query in queries.iter() {
                let (o, t, b) = (&ours[*query], &theirs[*query], &old[*query]);
                let equal = tie_groups(o) == tie_groups(t);
                let exact = o.iter().map(|r| &r.0).eq(t.iter().map(|r| &r.0));
                println!(
                    "  [{language}] {query:<20} | {:<40} | {:<40} | {} | {}",
                    labels(o),
                    labels(t),
                    if exact {
                        "same order"
                    } else if equal {
                        "same, tie order differs"
                    } else {
                        "DIFFERENT"
                    },
                    labels(b)
                );
                assert!(equal, "{query}: native {o:?} != CLI {t:?}");
                check_failure(language, query, o, t);
                if failed(o) {
                    upstream_failed += 1;
                } else if !o.is_empty() {
                    with_results += 1;
                }
                if o.iter().map(|r| &r.0).ne(b.iter().map(|r| &r.0)) {
                    differences_from_old.push(format!(
                        "[{language}] {query}: before [{}], now [{}]",
                        labels(b),
                        labels(o)
                    ));
                }
                queries_run += 1;
            }
        }
        assert_eq!(
            upstream_failed,
            UPSTREAM_FAILURES.len(),
            "exactly the known pagefind_web failures fail"
        );
        assert!(
            with_results >= MIN_QUERIES_WITH_RESULTS,
            "only {with_results} queries returned results"
        );
        println!(
            "gate 3, dense: PASS: {queries_run} queries, native equal to the CLI on every one; \
             {with_results} returned results, {upstream_failed} failed in pagefind_web on both (known)"
        );
        println!(
            "gate 3: results that differ from the site before this change (language and stemming):"
        );
        for d in &differences_from_old {
            println!("  {d}");
        }

        // 5% tombstones: three of the sixty leave after a dense build, so the
        // native index carries three tombstones; the CLI builds the 57 densely.
        let pool = isolated_ledger(&app.db).await;
        native(&pool, &docs).await;
        let gone: BTreeSet<usize> = [5, 17, 50].into();
        let fewer: Vec<IndexDocument> = docs
            .iter()
            .enumerate()
            .filter(|(n, _)| !gone.contains(n))
            .map(|(_, d)| d.clone())
            .collect();
        let (report, tombstoned) = native(&pool, &fewer).await;
        assert_eq!(report.tombstones, 3);
        let cli_fewer = cli(&bin, &fewer);
        let (mut positions, mut moved, mut sets_differ) = (0, 0, 0);
        for (language, queries) in [("en", EN_QUERIES), ("de", DE_QUERIES)] {
            let ours = run_queries(&node, &tombstoned, language, queries);
            let theirs = run_queries(&node, &cli_fewer, language, queries);
            for query in queries.iter() {
                let (o, t) = (&ours[*query], &theirs[*query]);
                check_failure(language, query, o, t);
                let a: BTreeSet<_> = o.iter().map(|r| &r.0).collect();
                let b: BTreeSet<_> = t.iter().map(|r| &r.0).collect();
                if a != b {
                    sets_differ += 1;
                }
                positions += t.len();
                let here = moved_positions(o, t);
                moved += here;
                if here > 0 {
                    println!(
                        "  5%: [{language}] {query}: native {} | CLI {}",
                        labels(o),
                        labels(t)
                    );
                }
            }
        }
        println!(
            "gate 3, 5% tombstones: {moved} of {positions} results moved out of their tie group, {sets_differ} queries with a different result set"
        );
        assert_eq!(
            sets_differ, 0,
            "tombstones must never add or remove a result"
        );
        // The stated bound: at most a tenth of the result positions may move at
        // 5% tombstones (docs/design/incremental-search-index.md).
        assert!(
            moved * 10 <= positions,
            "{moved} of {positions} positions moved"
        );
    });
}

// =============================================================================
// Part 3: the incremental updater (B218)
// =============================================================================
//
// * Gate 1, incremental: from a full build, a random sequence of at least 200
//   upserts, deletes, language moves and comment changes, applied batch by
//   batch through the change queue and the updater, decodes equal to a full
//   build of the same state through the same ledger after every few batches
//   (page numbers, fragments, word counts, every word's postings), with no
//   fallback taken; long pages split a chunk on the way. Then compaction, and
//   the index is byte for byte a first build of the final state.
// * Gate 3, incremental: the stock `pagefind.js` finds each kind of change
//   right after the update that applied it, and returns the same results as a
//   full rebuild of the same state.
// * Two runners at once apply every queued item exactly once.
// * A live entry the database did not record (a crash between the flip and
//   the commit) takes a full build, logged with the reason.

/// The site as a fixture holds it: every searchable document, by id.
struct Site(std::sync::Mutex<BTreeMap<Uuid, IndexDocument>>);

impl Site {
    fn new(docs: Vec<IndexDocument>) -> Self {
        Self(std::sync::Mutex::new(
            docs.into_iter().map(|d| (d.id, d)).collect(),
        ))
    }
    fn docs(&self) -> Vec<IndexDocument> {
        self.0.lock().unwrap().values().cloned().collect()
    }
    fn with<T>(&self, f: impl FnOnce(&mut BTreeMap<Uuid, IndexDocument>) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
}

#[async_trait::async_trait]
impl DocumentSource for Site {
    async fn documents(&self, ids: Option<&[Uuid]>) -> anyhow::Result<Vec<IndexDocument>> {
        let docs = self.0.lock().unwrap();
        Ok(match ids {
            None => docs.values().cloned().collect(),
            Some(ids) => ids.iter().filter_map(|id| docs.get(id).cloned()).collect(),
        })
    }
}

/// xorshift64*, so the sequence is the same every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Queue `ids` as the kernel's writers do.
async fn queue(pool: &PgPool, ids: &[Uuid]) {
    let items: Vec<Queued> = ids.iter().copied().map(Queued::upsert).collect();
    enqueue(pool, &items, "gate").await.expect("queue items");
}

/// One cron run over the index, compacting past `compaction_percent`.
async fn cron_run(pool: &PgPool, out: &Path, site: &Site, compaction_percent: f64) -> RunReport {
    search_index_run(
        pool,
        out,
        site,
        &RunOptions {
            compaction_percent,
            ..RunOptions::default()
        },
    )
    .await
    .expect("a search index run")
}

/// A copy of `pool`'s gate schema, rows and all, in a schema of its own: the
/// same ledger, term cache and state, for a full build of the same state that
/// leaves the original alone.
async fn fork_ledger(db: &PgPool, pool: &PgPool) -> PgPool {
    let from: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pool)
        .await
        .unwrap();
    let fork = isolated_ledger(db).await;
    let to: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&fork)
        .await
        .unwrap();
    for table in LEDGER_TABLES {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {to}.{table} SELECT * FROM {from}.{table}"
        )))
        .execute(db)
        .await
        .expect("copy a ledger table");
    }
    fork
}

/// Every slot's fragment and word count, and every word's postings, per
/// language: what a reader of the index can tell apart.
type Decoded = BTreeMap<
    String,
    (
        Vec<(serde_json::Value, u32)>,
        BTreeMap<String, Vec<(u64, Vec<i32>)>>,
    ),
>;

fn decoded(bundle: &Bundle) -> Decoded {
    let mut out = BTreeMap::new();
    for index in decode_bundle(bundle).expect("decode") {
        let pages = index
            .meta
            .pages
            .iter()
            .map(|p| (index.fragments[&p.hash].clone(), p.word_count))
            .collect();
        let mut words = BTreeMap::new();
        for chunk in index.chunks.values() {
            for w in &chunk.words {
                let postings = w
                    .pages
                    .iter()
                    .map(|p| (p.page_number, p.locs.clone()))
                    .collect();
                assert!(
                    words.insert(w.word.clone(), postings).is_none(),
                    "{} is in two chunks",
                    w.word
                );
            }
        }
        out.insert(index.language.clone(), (pages, words));
    }
    out
}

/// Assert the published index in `out` decodes equal to a full build of the
/// site through a copy of the same ledger.
async fn assert_equals_full_build(db: &PgPool, pool: &PgPool, out: &Path, site: &Site, when: &str) {
    let fork = fork_ledger(db, pool).await;
    let full_out = scratch("full").join("pagefind");
    build(&fork, &full_out, &site.docs())
        .await
        .expect("the full build of the same state");
    let ours = decoded(&read_published(out).expect("the incremental index reads"));
    let theirs = decoded(&read_published(&full_out).expect("the full index reads"));
    assert_eq!(
        ours.keys().collect::<Vec<_>>(),
        theirs.keys().collect::<Vec<_>>(),
        "{when}: languages"
    );
    for (language, (pages, words)) in &ours {
        let (their_pages, their_words) = &theirs[language];
        assert_eq!(pages.len(), their_pages.len(), "{when}: {language} slots");
        for (n, (a, b)) in pages.iter().zip(their_pages).enumerate() {
            assert_eq!(a, b, "{when}: {language} page {n}");
        }
        if words != their_words {
            let differ: Vec<&String> = words
                .keys()
                .chain(their_words.keys())
                .filter(|w| words.get(*w) != their_words.get(*w))
                .take(5)
                .collect();
            panic!("{when}: {language} postings differ from a full build, e.g. {differ:?}");
        }
    }
}

/// Words no fixture bank holds, so a new one lands between chunks.
fn fresh_word(rng: &mut Rng) -> String {
    const PARTS: &[&str] = &[
        "ab", "cor", "dex", "fi", "gul", "hon", "jat", "kel", "mup", "nor", "ost", "pry", "quam",
        "rix", "sul", "tov", "uvo", "wex", "yal", "zor",
    ];
    let mut w = String::new();
    for _ in 0..3 {
        w.push_str(PARTS[rng.below(PARTS.len())]);
    }
    w
}

/// A long page of `words` words that all start with `zor`, so its locations
/// land in one chunk and a few of them push that chunk past the split limit.
fn dense_text(rng: &mut Rng, words: usize) -> String {
    (0..words)
        .map(|_| {
            let w = fresh_word(rng);
            format!("zor{}", &w[..w.len().min(6)])
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A page of `words` words drawn from the banks and fresh words.
fn some_text(rng: &mut Rng, words: usize, german: bool) -> String {
    let bank = if german { DE_SENTENCES } else { EN_SENTENCES };
    let mut text = String::new();
    while text.split_whitespace().count() < words {
        if !text.is_empty() {
            text.push(' ');
        }
        if rng.below(4) == 0 {
            text.push_str(&fresh_word(rng));
        } else {
            text.push_str(pick(bank, rng.below(bank.len())));
        }
    }
    text
}

fn new_doc(rng: &mut Rng, n: usize, words: usize) -> IndexDocument {
    let german = rng.below(5) == 0;
    let id = Uuid::from_u128(0x0194_17ad_0000_7000_8000_0001_0000_0000 + n as u128);
    IndexDocument {
        id,
        item_type: "page".into(),
        title: format!("{} {}", fresh_word(rng), fresh_word(rng)),
        language: if german { "de" } else { "en" }.into(),
        url: format!("/item/{id}"),
        fields: vec![IndexedField {
            name: "field_body".into(),
            weight: 1.0,
            text: some_text(rng, words, german),
        }],
        comments: Vec::new(),
        description: String::new(),
        location: String::new(),
        event_dates: String::new(),
        created_display: "2026-11-25".into(),
    }
}

#[test]
fn gate_1_incremental_updates_equal_a_full_build_of_the_same_state() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let out = scratch("incremental").join("pagefind");
        let site = Site::new(fixture());
        let first = cron_run(&pool, &out, &site, 100.0).await;
        assert_eq!(
            first.full_builds.len(),
            1,
            "nothing published yet: a full build"
        );

        let mut rng = Rng(0x5EED_0000_B218_0001);
        let mut ops: BTreeMap<&str, usize> = BTreeMap::new();
        let (mut next, mut long_pages) = (0usize, 0usize);
        let (mut splits, mut files_written, mut updates) = (0usize, Vec::new(), 0usize);
        let batches = 60;
        for batch in 1..=batches {
            let mut touched = Vec::new();
            for _ in 0..(1 + rng.below(7)) {
                let roll = rng.below(100);
                let ids: Vec<Uuid> = site.with(|d| d.keys().copied().collect());
                let pick_id = |rng: &mut Rng| ids[rng.below(ids.len())];
                let language_count =
                    |lang: &str| site.with(|d| d.values().filter(|x| x.language == lang).count());
                let kind = match roll {
                    0..=17 => {
                        let id = pick_id(&mut rng);
                        let word = fresh_word(&mut rng);
                        site.with(|d| d.get_mut(&id).unwrap().title = format!("{word} title"));
                        touched.push(id);
                        "edit title"
                    }
                    18..=31 => {
                        let id = pick_id(&mut rng);
                        let german = site.with(|d| d[&id].language == "de");
                        let words = 20 + rng.below(60);
                        let text = some_text(&mut rng, words, german);
                        site.with(|d| d.get_mut(&id).unwrap().fields[0].text = text);
                        touched.push(id);
                        "edit body"
                    }
                    32..=46 => {
                        let id = pick_id(&mut rng);
                        let text = format!(
                            "{} {}",
                            fresh_word(&mut rng),
                            pick(EN_COMMENTS, rng.below(9))
                        );
                        site.with(|d| d.get_mut(&id).unwrap().comments.push(text));
                        touched.push(id);
                        "add comment"
                    }
                    47..=56 => {
                        let id = pick_id(&mut rng);
                        if site.with(|d| d.get_mut(&id).unwrap().comments.pop().is_none()) {
                            continue;
                        }
                        touched.push(id);
                        "remove comment"
                    }
                    57..=70 => {
                        let id = pick_id(&mut rng);
                        let lang = site.with(|d| d[&id].language.clone());
                        if language_count(&lang) <= 6 {
                            continue;
                        }
                        site.with(|d| d.remove(&id));
                        touched.push(id);
                        "delete"
                    }
                    71..=88 => {
                        next += 1;
                        let words = 20 + rng.below(80);
                        let doc = new_doc(&mut rng, next, words);
                        touched.push(doc.id);
                        site.with(|d| d.insert(doc.id, doc));
                        "insert"
                    }
                    89..=94 => {
                        let id = pick_id(&mut rng);
                        let lang = site.with(|d| d[&id].language.clone());
                        if language_count(&lang) <= 6 {
                            continue;
                        }
                        let to = if lang == "en" { "de" } else { "en" };
                        site.with(|d| d.get_mut(&id).unwrap().language = to.into());
                        touched.push(id);
                        "change language"
                    }
                    _ => {
                        if long_pages >= 18 {
                            continue;
                        }
                        long_pages += 1;
                        next += 1;
                        let mut doc = new_doc(&mut rng, next, 2_500);
                        doc.language = "en".into();
                        doc.fields[0].text = dense_text(&mut rng, 6_000);
                        touched.push(doc.id);
                        site.with(|d| d.insert(doc.id, doc));
                        "insert long page"
                    }
                };
                *ops.entry(kind).or_default() += 1;
            }
            queue(&pool, &touched).await;
            let report = cron_run(&pool, &out, &site, 100.0).await;
            assert!(
                report.full_builds.is_empty(),
                "batch {batch}: the updater fell back: {:?}",
                report
                    .full_builds
                    .iter()
                    .map(|(r, _)| r)
                    .collect::<Vec<_>>()
            );
            for update in &report.updates {
                splits += update.chunks_split;
                if update.pages_changed > 0 {
                    files_written.push(update.files_written);
                }
                updates += 1;
            }
            let left: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_queue")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(left, 0, "batch {batch}: the queue is drained");
            if batch % 6 == 0 || batch == batches {
                assert_equals_full_build(&app.db, &pool, &out, &site, &format!("batch {batch}"))
                    .await;
            }
        }
        let total: usize = ops.values().sum();
        println!("gate 1, incremental: {total} changes in {batches} batches: {ops:?}");
        println!(
            "gate 1, incremental: {updates} updates, {splits} chunk splits, files written per \
             update: max {}, mean {:.1}",
            files_written.iter().max().unwrap(),
            files_written.iter().sum::<usize>() as f64 / files_written.len() as f64
        );
        assert!(total >= 200, "only {total} changes");
        assert!(splits > 0, "the long pages must split a chunk");
        let tombstones: i64 =
            sqlx::query_scalar("SELECT count(*) FROM search_page WHERE item_id IS NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(tombstones > 0, "the sequence leaves tombstones to compact");

        // Compaction: past a 0% threshold any tombstone compacts.
        let compacted = cron_run(&pool, &out, &site, 0.0).await;
        assert_eq!(compacted.full_builds.len(), 1, "one compaction");
        assert!(compacted.full_builds[0].1.compacted);
        assert_eq!(compacted.full_builds[0].1.tombstones, 0);

        // Byte for byte a first build of the same content, from an empty
        // ledger and an empty term cache.
        let fresh = isolated_ledger(&app.db).await;
        let fresh_out = scratch("fresh").join("pagefind");
        build(&fresh, &fresh_out, &site.docs())
            .await
            .expect("a first build");
        let ours = read_published(&out).unwrap();
        let theirs = read_published(&fresh_out).unwrap();
        let (a, b) = (index_files(&ours), index_files(&theirs));
        assert_eq!(
            a.keys().collect::<Vec<_>>(),
            b.keys().collect::<Vec<_>>(),
            "after compaction: the same file names"
        );
        for (name, bytes) in &a {
            assert!(*bytes == b[name], "after compaction: {name} differs");
        }
        // And the term cache the updater kept is the one a first build makes.
        let terms = |p: &PgPool| {
            let p = p.clone();
            async move {
                sqlx::query_as::<_, (Uuid, String, Vec<u8>, Vec<u8>)>(
                    "SELECT item_id, language, content_hash, words FROM search_page_terms \
                     ORDER BY item_id",
                )
                .fetch_all(&p)
                .await
                .unwrap()
            }
        };
        assert!(
            terms(&pool).await == terms(&fresh).await,
            "the term caches differ"
        );
        println!(
            "gate 1, incremental: PASS: decoded equal to a full build after every 6th batch; \
             after compaction byte identical to a first build ({} index files)",
            a.len()
        );
    });
}

/// Run `queries` against the live index in `out` and assert they equal a full
/// rebuild's of the same state, tie for tie. Returns the live results.
async fn queries_match_a_full_build(
    node: &Path,
    db: &PgPool,
    pool: &PgPool,
    out: &Path,
    site: &Site,
    language: &str,
    queries: &[&str],
    when: &str,
) -> BTreeMap<String, Vec<(String, f64)>> {
    let fork = fork_ledger(db, pool).await;
    let full_out = scratch("full").join("pagefind");
    build(&fork, &full_out, &site.docs())
        .await
        .expect("full build");
    let ours = run_queries(node, &read_bundle(out).unwrap(), language, queries);
    let theirs = run_queries(node, &read_bundle(&full_out).unwrap(), language, queries);
    for query in queries {
        assert_eq!(
            tie_groups(&ours[*query]),
            tie_groups(&theirs[*query]),
            "{when}: [{language}] {query}: incremental {:?} != full {:?}",
            ours[*query],
            theirs[*query]
        );
    }
    ours
}

fn urls(results: &[(String, f64)]) -> Vec<&str> {
    results.iter().map(|(u, _)| u.as_str()).collect()
}

#[test]
fn gate_3_pagefind_js_finds_each_change_right_after_its_update() {
    run_test(async {
        let Some(node) = node() else {
            assert!(
                std::env::var_os("TROVATO_REQUIRE_PAGEFIND_CLI").is_none(),
                "gate 3, incremental: TROVATO_REQUIRE_PAGEFIND_CLI is set but there is no node"
            );
            println!("SKIPPED gate 3, incremental: no node (set NODE_BIN or put node on PATH)");
            return;
        };
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let out = scratch("gate3-incremental").join("pagefind");
        let site = Site::new(fixture());
        cron_run(&pool, &out, &site, 100.0).await;
        let url = |n: usize| format!("/item/{}", fixture_id(n));
        let apply = |ids: Vec<Uuid>| {
            let pool = pool.clone();
            let out = out.clone();
            let site = &site;
            async move {
                queue(&pool, &ids).await;
                let report = cron_run(&pool, &out, site, 100.0).await;
                assert!(report.full_builds.is_empty(), "no fallback: {report:?}");
                assert_eq!(report.updates.len(), 1);
            }
        };
        let mut queries: Vec<&str> = EN_QUERIES.to_vec();
        queries.extend(["quokkaplatz", "wombatfest", "numbatgarten", "brokolinth"]);

        // An edited title.
        site.with(|d| d.get_mut(&fixture_id(2)).unwrap().title = "Quokkaplatz review".into());
        apply(vec![fixture_id(2)]).await;
        let r = queries_match_a_full_build(
            &node,
            &app.db,
            &pool,
            &out,
            &site,
            "en",
            &queries,
            "title edit",
        )
        .await;
        assert_eq!(
            urls(&r["quokkaplatz"]),
            vec![url(2).as_str()],
            "the new title is found"
        );

        // A new comment.
        site.with(|d| {
            d.get_mut(&fixture_id(3))
                .unwrap()
                .comments
                .push("Wombatfest was great".into());
        });
        apply(vec![fixture_id(3)]).await;
        let r = queries_match_a_full_build(
            &node, &app.db, &pool, &out, &site, "en", &queries, "comment",
        )
        .await;
        assert_eq!(
            urls(&r["wombatfest"]),
            vec![url(3).as_str()],
            "the comment is found"
        );

        // A comment removed again.
        site.with(|d| d.get_mut(&fixture_id(3)).unwrap().comments.pop());
        apply(vec![fixture_id(3)]).await;
        let r = queries_match_a_full_build(
            &node,
            &app.db,
            &pool,
            &out,
            &site,
            "en",
            &queries,
            "comment removed",
        )
        .await;
        assert!(r["wombatfest"].is_empty(), "the removed comment is gone");

        // A deleted (or unpublished: the seam leaves it out either way) item.
        site.with(|d| d.remove(&fixture_id(2)));
        apply(vec![fixture_id(2)]).await;
        let r = queries_match_a_full_build(
            &node, &app.db, &pool, &out, &site, "en", &queries, "delete",
        )
        .await;
        assert!(r["quokkaplatz"].is_empty(), "the deleted item is gone");
        let freed: i32 = sqlx::query_scalar(
            "SELECT page_number FROM search_page WHERE language = 'en' AND item_id IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        // A new item takes the freed slot.
        let mut rng = Rng(7);
        let mut doc = new_doc(&mut rng, 900, 30);
        doc.language = "en".into();
        doc.title = "Numbatgarten opening".into();
        let new_id = doc.id;
        site.with(|d| d.insert(new_id, doc));
        apply(vec![new_id]).await;
        let page: i32 =
            sqlx::query_scalar("SELECT page_number FROM search_page WHERE item_id = $1")
                .bind(new_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(page, freed, "the new item reuses the freed page number");
        let r = queries_match_a_full_build(
            &node, &app.db, &pool, &out, &site, "en", &queries, "insert",
        )
        .await;
        assert_eq!(
            urls(&r["numbatgarten"]),
            vec![format!("/item/{new_id}").as_str()]
        );

        // An item moved from English to German.
        site.with(|d| {
            let doc = d.get_mut(&fixture_id(7)).unwrap();
            doc.language = "de".into();
            doc.title = "Brokolinth Ausstellung".into();
        });
        apply(vec![fixture_id(7)]).await;
        let en = queries_match_a_full_build(
            &node,
            &app.db,
            &pool,
            &out,
            &site,
            "en",
            &queries,
            "language move",
        )
        .await;
        assert!(
            en["brokolinth"].is_empty(),
            "no longer in the English index"
        );
        let mut de_queries: Vec<&str> = DE_QUERIES.to_vec();
        de_queries.retain(|q| !UPSTREAM_FAILURES.iter().any(|(_, f)| f == q));
        de_queries.push("brokolinth");
        let de = queries_match_a_full_build(
            &node,
            &app.db,
            &pool,
            &out,
            &site,
            "de",
            &de_queries,
            "language move",
        )
        .await;
        assert_eq!(
            urls(&de["brokolinth"]),
            vec![url(7).as_str()],
            "found in German"
        );
        println!(
            "gate 3, incremental: PASS: title edit, comment added and removed, delete, slot reuse, language move"
        );
    });
}

#[test]
fn two_runners_at_once_apply_every_queued_item_exactly_once() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger_with(&app.db, 8).await;
        let out = scratch("two-runners").join("pagefind");
        let site = Site::new(fixture());
        cron_run(&pool, &out, &site, 100.0).await;
        let before: i64 = sqlx::query_scalar("SELECT generation FROM search_index_state")
            .fetch_one(&pool)
            .await
            .unwrap();

        let ids: Vec<Uuid> = (0..40).map(fixture_id).collect();
        site.with(|d| {
            for id in &ids {
                d.get_mut(id)
                    .unwrap()
                    .comments
                    .push(format!("echidnaword {id}"));
            }
        });
        queue(&pool, &ids).await;
        let options = RunOptions {
            batch_size: 10,
            compaction_percent: 100.0,
            ..RunOptions::default()
        };
        let (a, b) = tokio::join!(
            search_index_run(&pool, &out, &site, &options),
            search_index_run(&pool, &out, &site, &options)
        );
        let (a, b) = (a.expect("runner a"), b.expect("runner b"));
        let applied: usize = a.updates.iter().chain(&b.updates).map(|u| u.queued).sum();
        let publishes = (a.updates.len() + b.updates.len()) as i64;
        let after: i64 = sqlx::query_scalar("SELECT generation FROM search_index_state")
            .fetch_one(&pool)
            .await
            .unwrap();
        println!(
            "two runners: a applied {} batches (superseded: {}), b {} (superseded: {})",
            a.updates.len(),
            a.superseded,
            b.updates.len(),
            b.superseded
        );
        assert!(a.full_builds.is_empty() && b.full_builds.is_empty());
        // Whatever one runner left (it gave up when superseded), a third run
        // finishes; nothing is applied twice.
        let rest = cron_run(&pool, &out, &site, 100.0).await;
        let applied = applied + rest.updates.iter().map(|u| u.queued).sum::<usize>();
        let publishes = publishes + rest.updates.len() as i64;
        let after = after.max(
            sqlx::query_scalar("SELECT generation FROM search_index_state")
                .fetch_one(&pool)
                .await
                .unwrap(),
        );
        assert_eq!(applied, ids.len(), "every queued item applied exactly once");
        assert_eq!(after - before, publishes, "one generation per publish");
        assert_equals_full_build(&app.db, &pool, &out, &site, "two runners").await;
        // No staging directory left behind.
        let parent = out.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("_build_"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    });
}

#[test]
fn a_live_entry_the_database_did_not_record_takes_a_full_build() {
    run_test(async {
        let app = shared_app().await;
        let pool = isolated_ledger(&app.db).await;
        let out = scratch("unrecorded").join("pagefind");
        let site = Site::new(fixture());
        cron_run(&pool, &out, &site, 100.0).await;

        // As if a publish flipped the entry and crashed before its commit: the
        // database names a meta the live entry does not.
        sqlx::query(
            "UPDATE search_index_state SET entry_meta = jsonb_set(entry_meta, '{en}', '\"en_0000000\"')",
        )
        .execute(&pool)
        .await
        .unwrap();
        site.with(|d| d.get_mut(&fixture_id(1)).unwrap().title = "Platypusweg".into());
        queue(&pool, &[fixture_id(1)]).await;
        let report = cron_run(&pool, &out, &site, 100.0).await;
        assert_eq!(report.full_builds.len(), 1, "{report:?}");
        assert!(
            report.full_builds[0].0.contains("the live entry names"),
            "{}",
            report.full_builds[0].0
        );
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_queue")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "the full build covers the queued item");
        assert_equals_full_build(&app.db, &pool, &out, &site, "after the fallback").await;
        // And the next change is incremental again.
        site.with(|d| d.get_mut(&fixture_id(1)).unwrap().title = "Platypusweg two".into());
        queue(&pool, &[fixture_id(1)]).await;
        let report = cron_run(&pool, &out, &site, 100.0).await;
        assert!(
            report.full_builds.is_empty() && report.updates.len() == 1,
            "{report:?}"
        );
    });
}
