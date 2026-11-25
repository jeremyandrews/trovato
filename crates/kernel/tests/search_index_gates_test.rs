#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The three compatibility gates of `docs/design/incremental-search-index.md`
//! for the native full build (B217, part 2 of 3).
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
use trovato_kernel::cron::{IndexDocument, IndexedField, render_fragment, search_index_full_build};
use trovato_search_index::decode::{Bundle, PageView, decode_bundle, read_bundle};
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
];

/// A pool whose `search_path` puts a fresh copy of the four search index
/// tables ahead of `public`, dropped when the test ends.
async fn isolated_ledger(db: &PgPool) -> PgPool {
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
        .max_connections(4)
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

/// One native full build into a new output directory, read back.
async fn native(
    pool: &PgPool,
    docs: &[IndexDocument],
) -> (trovato_kernel::cron::FullBuildReport, Bundle) {
    let out = scratch("native").join("pagefind");
    let report = search_index_full_build(pool, &out, docs)
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

        let first = search_index_full_build(&pool, &out, &docs).await.unwrap();
        let second = search_index_full_build(&pool, &out, &docs).await.unwrap();
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
        let third = search_index_full_build(&pool, &out, &edited).await.unwrap();
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
        let fourth = search_index_full_build(&pool, &out, &edited).await.unwrap();
        assert_eq!(fourth.expired as i64, unreferenced);
        let entry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("pagefind-entry.json")).unwrap())
                .unwrap();
        // Everything the live entry names is still there.
        decode_bundle(&read_bundle(&out).unwrap()).expect("the live bundle decodes");
        assert!(entry["languages"]["en"]["hash"].is_string());
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
        println!(
            "gate 3, dense: PASS: {queries_run} queries, native equal to the CLI on every one"
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
