#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The tokenizer proof, over the committed corpus.
//!
//! `corpus/` is 60 real items from a long lived local Trovato database, written
//! by `pagefind-harness export --sample 60`, which calls the kernel's own
//! `indexable_documents` and `render_fragment` and names each file `<id>.html`,
//! exactly as `build_index_inner` does.
//!
//! Two proofs:
//!
//! * against the crate's own full build: needs nothing, always runs;
//! * against the `pagefind` CLI: needs the CLI (`$PAGEFIND_BIN`, else
//!   `pagefind` on `PATH`) and **skips with a message** without it.
//!
//! This crate is its own workspace, excluded from the root one, so CI does not
//! run either (see docs/design/incremental-search-index.md). Run it with
//!
//!   ```text
//!   PAGEFIND_BIN=/path/to/pagefind-1.3.0 cargo test \
//!       --manifest-path crates/pagefind-harness/Cargo.toml \
//!       --test tokenizer_parity -- --nocapture
//!   ```

use std::path::PathBuf;

use trovato_pagefind_harness::PINNED_PAGEFIND_VERSION;
use trovato_pagefind_harness::build::{find_cli, read_site};
use trovato_pagefind_harness::parity::{against_cli, against_crate};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn the_corpus_holds_at_least_fifty_exported_items() {
    let pages = read_site(&corpus()).expect("read the corpus");
    assert!(pages.len() >= 50, "only {} pages", pages.len());
    for page in &pages {
        assert!(
            page.html.contains("data-pagefind-body"),
            "{} is not a render_fragment export",
            page.file_name
        );
    }
}

#[test]
fn each_page_alone_matches_the_crates_full_build() {
    let pages = read_site(&corpus()).expect("read the corpus");
    let report = runtime()
        .block_on(against_crate(&pages))
        .expect("run the proof");
    println!(
        "per page vs the crate's full build: {} pages, {} postings, {} positions, {} mismatches",
        report.pages_compared,
        report.postings_compared,
        report.positions_compared,
        report.mismatches.len()
    );
    assert_eq!(report.pages_compared, pages.len());
    assert!(report.positions_compared > 0);
    assert!(report.is_identical(), "{:#?}", report.mismatches);
}

#[test]
fn each_page_alone_matches_a_full_cli_build() {
    let Some(bin) = find_cli() else {
        println!(
            "SKIPPED: no pagefind CLI (set PAGEFIND_BIN to a pagefind {PINNED_PAGEFIND_VERSION} binary \
             or put one on PATH); the CLI comparison did not run"
        );
        return;
    };
    let parity = runtime()
        .block_on(against_cli(&bin, &corpus()))
        .expect("run the proof");
    println!("CLI: {} at {}", parity.cli_version, bin.display());
    println!(
        "per page vs the CLI: {} pages, {} postings, {} positions, {} mismatches",
        parity.pages.pages_compared,
        parity.pages.postings_compared,
        parity.pages.positions_compared,
        parity.pages.mismatches.len()
    );
    println!("CLI meta files: {:?}", parity.meta_files);
    assert_eq!(
        parity.cli_version,
        format!("pagefind {PINNED_PAGEFIND_VERSION}"),
        "the CLI must be the pinned release"
    );
    // The generated pages carry no `lang`, so the index is built as `unknown`.
    assert_eq!(parity.languages, vec!["unknown".to_string()]);
    assert!(
        parity.pages.is_identical(),
        "{:#?}",
        parity.pages.mismatches
    );
    let bytes = &parity.bytes_in_cli_order;
    assert!(
        bytes.index_identical(),
        "the crate in the CLI's page order must write the CLI's index files byte for byte: {bytes:#?}"
    );
    if !bytes.is_identical() {
        println!(
            "runtime files that differ (not index data): {:?}",
            bytes.runtime_differences()
        );
    }
}

/// The stress corpus (long text, punctuation, entities, scripts, emoji;
/// several word chunks), through the kernel's `render_fragment`, against the
/// crate's full build. Needs nothing.
#[test]
fn the_stress_corpus_matches_the_crates_full_build() {
    let pages = trovato_pagefind_harness::stress::pages();
    let report = runtime()
        .block_on(against_crate(&pages))
        .expect("run the proof");
    println!(
        "stress vs the crate's full build: {} pages, {} postings, {} positions, {} mismatches",
        report.pages_compared,
        report.postings_compared,
        report.positions_compared,
        report.mismatches.len()
    );
    assert!(report.positions_compared > 20_000, "must cross a chunk cut");
    assert!(report.is_identical(), "{:#?}", report.mismatches);
}

/// The stress corpus against the CLI. Skips without one.
#[test]
fn the_stress_corpus_matches_a_full_cli_build() {
    let Some(bin) = find_cli() else {
        println!("SKIPPED: no pagefind CLI; the stress CLI comparison did not run");
        return;
    };
    let dir = std::env::temp_dir().join(format!("b216-stress-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    trovato_pagefind_harness::stress::write(&dir).expect("write the stress corpus");
    let parity = runtime().block_on(against_cli(&bin, &dir));
    std::fs::remove_dir_all(&dir).ok();
    let parity = parity.expect("run the proof");
    println!(
        "stress vs the CLI: {} pages, {} postings, {} positions, {} mismatches",
        parity.pages.pages_compared,
        parity.pages.postings_compared,
        parity.pages.positions_compared,
        parity.pages.mismatches.len()
    );
    assert!(
        parity.pages.is_identical(),
        "{:#?}",
        parity.pages.mismatches
    );
    assert!(parity.bytes_in_cli_order.index_identical());
}
