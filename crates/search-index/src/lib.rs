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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Result, bail};

pub use assemble::{LanguageIndex, Slot, build_language, build_language_avoiding};
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
    build_bundle_avoiding(languages, &BTreeSet::new())
}

/// [`build_bundle`] with the bundle paths in `taken` treated as used by
/// different bytes (see [`build_language_avoiding`]).
pub fn build_bundle_avoiding(
    languages: &BTreeMap<String, Vec<Slot<'_>>>,
    taken: &BTreeSet<String>,
) -> Result<(Vec<LanguageIndex>, Bundle)> {
    let mut indexes = Vec::with_capacity(languages.len());
    for (language, slots) in languages {
        indexes.push(build_language_avoiding(language, slots, taken)?);
    }
    let bundle = assemble_bundle(&indexes)?;
    Ok((indexes, bundle))
}

/// How many times [`build_bundle_for_live`] re-encodes before giving up. Each
/// round lengthens only names that clashed, so one round is the norm and a
/// second needs a further 7-character clash on a lengthened name.
const MAX_NAMING_ROUNDS: usize = 8;

/// Build the bundle to be published into `live`: [`build_bundle`], then again
/// with every name that `live` already holds with different bytes avoided,
/// until no name clashes. On a directory with no clash, or none at all, the
/// output is exactly [`build_bundle`]'s.
pub fn build_bundle_for_live(
    languages: &BTreeMap<String, Vec<Slot<'_>>>,
    live: &Path,
) -> Result<(Vec<LanguageIndex>, Bundle)> {
    let mut taken = BTreeSet::new();
    for _ in 0..MAX_NAMING_ROUNDS {
        let (indexes, bundle) = build_bundle_avoiding(languages, &taken)?;
        let clashes = publish::conflicting_names(&bundle, live)?;
        if clashes.is_empty() {
            return Ok((indexes, bundle));
        }
        taken.extend(clashes);
    }
    bail!(
        "could not name the index files apart from {} after {MAX_NAMING_ROUNDS} rounds",
        live.display()
    )
}
