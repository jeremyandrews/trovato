//! A dev tool for the incremental search index (B216, part 1 of 3).
//!
//! Not shipped: nothing in the kernel depends on this crate. It exists to
//! answer one question with evidence before a native indexer is written: does
//! Pagefind's own crate, given **one page at a time**, produce exactly the
//! words, positions, weights, meta and fragment that a full `pagefind` CLI
//! build produces for that page? If it does, a per page term cache can be
//! filled by the stock tokenizer and only the index assembly has to be native.
//!
//! * [`decode`] reads a built index back: gunzip, strip the `pagefind_dcd`
//!   signature, CBOR decode into structs that mirror Pagefind 1.3.0's own
//!   `MetaIndex` and `WordIndex`, and JSON decode the fragments.
//! * [`build`] makes indexes three ways: the CLI over a directory, the crate's
//!   `PagefindIndex` over one page, and the crate over many pages in a given
//!   order.
//! * [`compare`] lines two decoded indexes up page by page, by URL, so page
//!   numbers (which are build order positions) drop out of the comparison.
//! * [`parity`] is the whole proof, run by the integration test and by the
//!   `pagefind-harness parity` subcommand.
//!
//! See `docs/design/incremental-search-index.md`.

pub mod build;
pub mod compare;
pub mod decode;
pub mod parity;
pub mod stress;

/// The Pagefind release this harness is pinned to, and that the kernel's
/// Dockerfile downloads. A bump changes both, and reruns the gates.
pub const PINNED_PAGEFIND_VERSION: &str = "1.3.0";
