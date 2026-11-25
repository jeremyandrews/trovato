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
//! This crate is its own workspace, excluded from the root one. CI's `Pagefind
//! Harness` job runs both, with the pinned CLI downloaded, and fails if either
//! skips (see docs/design/incremental-search-index.md). Locally, run it with
//!
//!   ```text
//!   PAGEFIND_BIN=/path/to/pagefind-1.3.0 cargo test \
//!       --manifest-path crates/pagefind-harness/Cargo.toml \
//!       --test tokenizer_parity -- --nocapture
//!   ```

use std::path::PathBuf;

use trovato_pagefind_harness::PINNED_PAGEFIND_VERSION;
use trovato_pagefind_harness::build::{find_cli, read_site};
use trovato_pagefind_harness::parity::{CliParity, CrateParity, against_cli, against_crate};

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
    let parity = runtime()
        .block_on(against_crate(&pages))
        .expect("run the proof");
    report_crate("corpus", &parity);
    let report = &parity.crate_pages;
    assert_eq!(report.pages_compared, pages.len());
    assert!(report.positions_compared > 0);
    assert_crate_parity(&parity);
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
    // The committed corpus was exported before pages carried `lang`, so the
    // CLI builds it as `unknown`.
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
    assert_vendored_cli(&parity);
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
    let parity = runtime()
        .block_on(against_crate(&pages))
        .expect("run the proof");
    report_crate("stress", &parity);
    assert!(
        parity.crate_pages.positions_compared > 20_000,
        "must cross a chunk cut"
    );
    assert_crate_parity(&parity);
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
    assert_vendored_cli(&parity);
}

fn report_crate(label: &str, parity: &CrateParity) {
    for (what, report) in [
        ("the crate per page", &parity.crate_pages),
        ("the vendored tokenizer per page", &parity.vendored_pages),
    ] {
        println!(
            "{label}: {what} vs the crate's full build: {} pages, {} postings, {} positions, {} mismatches",
            report.pages_compared,
            report.postings_compared,
            report.positions_compared,
            report.mismatches.len()
        );
    }
    let b = &parity.native_bytes;
    println!(
        "{label}: the native build vs the crate's full build, same page order: {} of {} files byte identical; differing {:?}, only crate {:?}, only native {:?}",
        b.identical, b.reference_files, b.differing, b.only_reference, b.only_candidate
    );
}

/// The vendored tokenizer gives the crate's pages, and the native assembler,
/// fed the same pages in the same order, writes the crate's index files byte
/// for byte (and the runtime files too: they are the crate's own, copied).
fn assert_crate_parity(parity: &CrateParity) {
    assert!(
        parity.crate_pages.is_identical(),
        "{:#?}",
        parity.crate_pages.mismatches
    );
    assert!(
        parity.vendored_pages.is_identical(),
        "{:#?}",
        parity.vendored_pages.mismatches
    );
    assert_eq!(
        parity.vendored_pages.pages_compared,
        parity.crate_pages.pages_compared
    );
    assert!(
        parity.native_bytes.is_identical(),
        "{:#?}",
        parity.native_bytes
    );
}

/// The same two proofs against the CLI.
fn assert_vendored_cli(parity: &CliParity) {
    println!(
        "the vendored tokenizer per page vs the CLI: {} pages, {} postings, {} positions, {} mismatches",
        parity.vendored_pages.pages_compared,
        parity.vendored_pages.postings_compared,
        parity.vendored_pages.positions_compared,
        parity.vendored_pages.mismatches.len()
    );
    let b = &parity.native_bytes_in_cli_order;
    println!(
        "the native build in the CLI's page order vs the CLI: {} of {} files byte identical; differing {:?}, only CLI {:?}, only native {:?}",
        b.identical, b.reference_files, b.differing, b.only_reference, b.only_candidate
    );
    assert!(
        parity.vendored_pages.is_identical(),
        "{:#?}",
        parity.vendored_pages.mismatches
    );
    assert!(
        b.index_identical(),
        "the native build in the CLI's page order must write the CLI's index files byte for byte: {b:#?}"
    );
}

/// The browser bundle `trovato-search-index` compiles in is the pinned
/// crate's, file for file: every runtime file and every language's WASM.
#[test]
fn the_vendored_browser_bundle_is_the_pinned_crates() {
    use trovato_pagefind_harness::bundle::{WASM_LANGUAGES, asset_name, crate_runtime_files};
    let from_crate = runtime()
        .block_on(crate_runtime_files())
        .expect("build the crate's bundle");
    let mut compared = 0;
    for (name, bytes) in trovato_search_index::bundle::RUNTIME_FILES {
        assert_eq!(
            from_crate.get(&asset_name(name)),
            Some(&bytes.to_vec()),
            "{name} differs from the pinned crate's"
        );
        compared += 1;
    }
    for lang in WASM_LANGUAGES {
        let ours = trovato_search_index::bundle::language_wasm(lang).expect("a wasm");
        assert_eq!(
            from_crate.get(&asset_name(&format!("wasm.{lang}.pagefind"))),
            Some(&ours.to_vec()),
            "the {lang} WASM differs from the pinned crate's"
        );
        compared += 1;
    }
    println!("browser bundle: {compared} files identical to the pinned crate's");
    assert_eq!(compared, from_crate.len());
}
