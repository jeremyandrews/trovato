//! Trovato's native Pagefind index codec, pinned to Pagefind 1.3.0.
//!
//! Trovato builds its static search index itself rather than by running the
//! `pagefind` CLI, so that page numbers can be stable (a page ledger in the
//! kernel) and a later change can rewrite only what it touched. The output is
//! a Pagefind 1.3.0 bundle that the stock `pagefind.js` reads.
//!
//! * [`vendor`] is Pagefind 1.3.0's own tokenizer (`fossick`), vendored, so
//!   the words, positions, weights and fragments are Pagefind's, not a
//!   reimplementation, and without the `pagefind` crate's `actix-web`.
//! * [`tokenize`](mod@tokenize) runs it over one page: [`tokenize::PageTerms`] is
//!   everything the index needs from that page, independent of its number.
//! * [`assemble`] writes one language's index from page slots numbered by the
//!   caller: the meta file, word chunks and fragments, byte for byte as
//!   Pagefind 1.3.0 writes them for the same pages in the same order.
//! * [`bundle`] adds the browser bundle from the same release and the entry
//!   file; [`publish`] puts a bundle live in a safe order; [`decode`] reads
//!   one back.
//!
//! Proved against the `pagefind` crate and CLI by `crates/pagefind-harness`
//! and by the kernel's search index gate tests. See
//! `docs/design/incremental-search-index.md`.

pub mod assemble;
pub mod bundle;
pub mod codec;
pub mod decode;
pub mod publish;
pub mod tokenize;
pub mod vendor;

use std::collections::BTreeMap;

use anyhow::Result;

pub use assemble::{LanguageIndex, Slot, build_language};
pub use bundle::{Bundle, assemble_bundle};
pub use tokenize::{PageTerms, tokenize};

/// The Pagefind release whose tokenizer, index format and browser bundle this
/// crate carries. Moves with `crates/pagefind-harness`'s `pagefind` pin, the
/// CLI the gates compare against, and the vendored files, never alone.
pub const PAGEFIND_VERSION: &str = "1.3.0";

/// Build every language's index and the whole bundle around them, from each
/// language's slots in page number order.
pub fn build_bundle(
    languages: &BTreeMap<String, Vec<Slot<'_>>>,
) -> Result<(Vec<LanguageIndex>, Bundle)> {
    let mut indexes = Vec::with_capacity(languages.len());
    for (language, slots) in languages {
        indexes.push(build_language(language, slots)?);
    }
    let bundle = assemble_bundle(&indexes)?;
    Ok((indexes, bundle))
}
