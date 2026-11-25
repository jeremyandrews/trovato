//! Pagefind 1.3.0's tokenizer, vendored.
//!
//! `fossick` (HTML in, words with positions and weights out, plus the page's
//! fragment) and the `fragments` types are copied from the `pagefind` crate at
//! exactly 1.3.0, the release the codec writes. They are vendored rather than
//! depended on because `pagefind` 1.3.0 always pulls in `actix-web` for
//! `pagefind --serve`, and with it RUSTSEC-2026-0443 and RUSTSEC-2026-0258,
//! neither fixable inside its version ranges. See
//! `docs/design/incremental-search-index.md`.
//!
//! Upstream: <https://github.com/CloudCannon/pagefind/tree/v1.3.0/pagefind/src>,
//! MIT licensed, Copyright 2022 CloudCannon; the notice is in
//! `LICENSE-PAGEFIND` beside this file. Every change is marked `TROVATO:` and
//! none touches the tokenizing. `crates/pagefind-harness` proves it: the
//! vendored tokenizer gives the same words, positions, weights, word count and
//! fragment as the `pagefind` crate and the `pagefind` CLI on every page of its
//! corpora.
#![allow(
    clippy::all,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::manual_let_else,
    clippy::uninlined_format_args,
    clippy::semicolon_if_nothing_returned
)]

pub mod fossick;
pub mod fragments;

/// The slice of Pagefind's `SearchOptions` that `fossick` reads.
///
/// TROVATO: upstream's `SearchOptions` carries the whole CLI configuration and
/// a logger. The tokenizer reads only these three, and Trovato runs it with
/// the CLI's defaults, which `Default` gives.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// The element treated as the document root: `html`.
    pub root_selector: String,
    /// Extra selectors to leave out: none.
    pub exclude_selectors: Vec<String>,
    /// A language that overrides every page's own: none.
    pub force_language: Option<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            root_selector: "html".into(),
            exclude_selectors: Vec::new(),
            force_language: None,
        }
    }
}
