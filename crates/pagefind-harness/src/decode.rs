//! Reading a built Pagefind index back into structs.
//!
//! The decoder now lives in `trovato-search-index` (`crates/search-index`),
//! which the native build, its gates and this harness share. This module
//! re-exports it so the harness reads indexes exactly as the kernel's gates do.

pub use trovato_search_index::decode::*;
