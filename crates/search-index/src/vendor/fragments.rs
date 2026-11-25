// TROVATO: vendored from Pagefind 1.3.0, `pagefind/src/fragments/mod.rs` (crate `pagefind`
// 1.3.0 on crates.io). MIT licensed, Copyright 2022 CloudCannon; the notice is
// in `./LICENSE-PAGEFIND`. Changes from upstream are marked `TROVATO:`.
use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Serialize, Debug, Clone)]
pub struct PageAnchorData {
    pub element: String,
    pub id: String,
    pub text: String,
    pub location: u32,
}

#[derive(Serialize, Debug, Clone)]
pub struct PageFragmentData {
    pub url: String,
    pub content: String,
    pub word_count: usize,
    pub filters: BTreeMap<String, Vec<String>>,
    pub meta: BTreeMap<String, String>,
    pub anchors: Vec<PageAnchorData>,
}

#[derive(Debug, Clone)]
pub struct PageFragment {
    pub page_number: usize,
    pub data: PageFragmentData,
}
