//! Lining two decoded indexes up page by page.

use std::collections::BTreeMap;

use crate::decode::{Bundle, PageView, decode_locs};

/// One way one page differs between the reference and the candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    /// The page's URL.
    pub url: String,
    /// What differs, in words.
    pub what: String,
}

/// The outcome of comparing every page.
#[derive(Debug, Clone, Default)]
pub struct PageReport {
    /// Pages present on both sides and compared.
    pub pages_compared: usize,
    /// (word, page) postings compared.
    pub postings_compared: usize,
    /// Word positions compared, summed over every posting.
    pub positions_compared: usize,
    /// Every difference found. Empty means identical.
    pub mismatches: Vec<Mismatch>,
}

impl PageReport {
    /// Whether nothing differs.
    pub fn is_identical(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Compare every page by URL: the same pages, and for each the same words, the
/// same positions and weights for each word, the same word count, the same
/// fragment (url, content, word_count, filters, meta, anchors) and the same
/// fragment file name. Page numbers are not compared: they are build order.
pub fn compare_pages(
    reference: &BTreeMap<String, PageView>,
    candidate: &BTreeMap<String, PageView>,
) -> PageReport {
    let mut report = PageReport::default();
    for url in reference.keys() {
        if !candidate.contains_key(url) {
            report.mismatches.push(Mismatch {
                url: url.clone(),
                what: "missing from the candidate".into(),
            });
        }
    }
    for url in candidate.keys() {
        if !reference.contains_key(url) {
            report.mismatches.push(Mismatch {
                url: url.clone(),
                what: "missing from the reference".into(),
            });
        }
    }
    for (url, want) in reference {
        let Some(got) = candidate.get(url) else {
            continue;
        };
        report.pages_compared += 1;
        let mut push = |what: String| {
            report.mismatches.push(Mismatch {
                url: url.clone(),
                what,
            });
        };
        if want.word_count != got.word_count {
            push(format!(
                "word_count {} != {}",
                want.word_count, got.word_count
            ));
        }
        if want.fragment != got.fragment {
            push(format!(
                "fragment differs: {} != {}",
                want.fragment, got.fragment
            ));
        }
        if want.fragment_hash != got.fragment_hash {
            push(format!(
                "fragment file {} != {}",
                want.fragment_hash, got.fragment_hash
            ));
        }
        for (word, locs) in &want.words {
            match got.words.get(word) {
                None => push(format!("word {word:?} missing from the candidate")),
                Some(other) if other != locs => push(format!(
                    "word {word:?} locations {:?} != {:?}",
                    decode_locs(locs),
                    decode_locs(other)
                )),
                Some(_) => {}
            }
        }
        for word in got.words.keys() {
            if !want.words.contains_key(word) {
                push(format!("word {word:?} missing from the reference"));
            }
        }
        report.postings_compared += want.words.len();
        report.positions_compared += want
            .words
            .values()
            .map(|locs| locs.iter().filter(|l| **l >= 0).count())
            .sum::<usize>();
    }
    report
}

/// Byte for byte comparison of two whole bundles.
#[derive(Debug, Clone, Default)]
pub struct ByteReport {
    /// Files in the reference.
    pub reference_files: usize,
    /// Files in the candidate.
    pub candidate_files: usize,
    /// Files present in both with identical bytes.
    pub identical: usize,
    /// Present in both, bytes differ.
    pub differing: Vec<String>,
    /// Only in the reference.
    pub only_reference: Vec<String>,
    /// Only in the candidate.
    pub only_candidate: Vec<String>,
}

impl ByteReport {
    /// Whether the two bundles are the same set of files with the same bytes.
    pub fn is_identical(&self) -> bool {
        self.differing.is_empty()
            && self.only_reference.is_empty()
            && self.only_candidate.is_empty()
    }

    /// Whether every **index** file (meta, word chunks, fragments, filters,
    /// entry) is present on both sides with identical bytes. The runtime files
    /// (`pagefind.js`, the UI bundles, `wasm.*.pagefind`) are not index data:
    /// they are copied verbatim from whichever build of Pagefind wrote the
    /// bundle, and the release CLI for one platform embeds a WASM compiled on
    /// that platform, so they can differ while the index is identical.
    pub fn index_identical(&self) -> bool {
        self.differing
            .iter()
            .chain(&self.only_reference)
            .chain(&self.only_candidate)
            .all(|name| !is_index_file(name))
    }

    /// The differing files that are runtime, not index.
    pub fn runtime_differences(&self) -> Vec<&str> {
        self.differing
            .iter()
            .chain(&self.only_reference)
            .chain(&self.only_candidate)
            .filter(|name| !is_index_file(name))
            .map(String::as_str)
            .collect()
    }
}

/// Whether a bundle path is index data rather than the browser runtime.
pub fn is_index_file(name: &str) -> bool {
    name == "pagefind-entry.json"
        || [".pf_meta", ".pf_index", ".pf_fragment", ".pf_filter"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

/// Compare two bundles file by file.
pub fn compare_bytes(reference: &Bundle, candidate: &Bundle) -> ByteReport {
    let mut report = ByteReport {
        reference_files: reference.len(),
        candidate_files: candidate.len(),
        ..ByteReport::default()
    };
    for (name, bytes) in reference {
        match candidate.get(name) {
            None => report.only_reference.push(name.clone()),
            Some(other) if other == bytes => report.identical += 1,
            Some(_) => report.differing.push(name.clone()),
        }
    }
    for name in candidate.keys() {
        if !reference.contains_key(name) {
            report.only_candidate.push(name.clone());
        }
    }
    report
}
