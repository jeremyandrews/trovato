//! Reading a built Pagefind index back into structs.
//!
//! The structs mirror `pagefind/src/index/index_metadata.rs` and
//! `pagefind/src/index/index_words.rs` at 1.3.0 field for field and index for
//! index (`#[n(..)]`), with `Decode` where Pagefind derives `Encode`. Both use
//! minicbor's default struct encoding, a fixed length array, which is what
//! `pagefind_web`'s hand written decoder (`metadata.rs`, `index.rs`) reads.
//!
//! At 1.3.0 a posting's page number is the page's plain position in
//! `MetaIndex::pages`, not a delta from the previous posting; delta encoding,
//! `meta_locs` and diacritic variants arrive in later releases (1.6.0 has all
//! three), which is one reason the native codec is pinned to one version.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use minicbor::Decode;

/// What Pagefind writes inside the gzip stream ahead of every compressed file.
pub const SIGNATURE: &[u8] = b"pagefind_dcd";

/// The `pagefind.<lang>_<hash>.pf_meta` file.
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaIndex {
    /// The generator version, `1.3.0`.
    #[n(0)]
    pub version: String,
    /// One entry per page, in page number order.
    #[n(1)]
    pub pages: Vec<MetaPage>,
    /// The word index chunks, in word order, with the word range each covers.
    #[n(2)]
    pub index_chunks: Vec<MetaChunk>,
    /// The filter index files.
    #[n(3)]
    pub filters: Vec<MetaFilter>,
    /// Pre-sorted page orders, one per sort key.
    #[n(4)]
    pub sorts: Vec<MetaSort>,
}

/// One word index chunk: load `index/<hash>.pf_index` for a word between
/// `from` and `to` (both shortened to the shortest distinguishing prefix of
/// the neighbouring chunk boundary).
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaChunk {
    /// First word, as a prefix.
    #[n(0)]
    pub from: String,
    /// Last word, as a prefix.
    #[n(1)]
    pub to: String,
    /// File name stem.
    #[n(2)]
    pub hash: String,
}

/// One page slot.
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaPage {
    /// The fragment file name stem, `<lang>_<short sha1>`.
    #[n(0)]
    pub hash: String,
    /// Words on the page: the BM25 document length.
    #[n(1)]
    pub word_count: u32,
}

/// One filter index file.
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaFilter {
    /// The filter name.
    #[n(0)]
    pub filter: String,
    /// File name stem.
    #[n(1)]
    pub hash: String,
}

/// One sort key's page order.
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaSort {
    /// The sort key.
    #[n(0)]
    pub sort: String,
    /// Page numbers in sorted order.
    #[n(1)]
    pub pages: Vec<u64>,
}

/// One `index/<hash>.pf_index` file.
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct WordIndex {
    /// Words in byte order.
    #[n(0)]
    pub words: Vec<PackedWord>,
}

/// One word's postings.
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct PackedWord {
    /// The (stemmed, when the language has a stemmer) word.
    #[n(0)]
    pub word: String,
    /// One entry per page the word is on, in page number order.
    #[n(1)]
    pub pages: Vec<PackedPage>,
}

/// One word's locations on one page.
#[derive(Decode, Debug, Clone, PartialEq, Eq)]
pub struct PackedPage {
    /// The page's position in [`MetaIndex::pages`].
    #[n(0)]
    pub page_number: u64,
    /// Word positions, with a weight change written inline as `-(weight) - 1`
    /// ahead of the positions it applies to. Weight 25 (Pagefind's 1.0) is the
    /// starting weight and is sorted first.
    #[n(1)]
    pub locs: Vec<i32>,
}

/// `locs` as `pagefind_web` reads them: `(weight, position)` pairs.
pub fn decode_locs(locs: &[i32]) -> Vec<(u8, u32)> {
    let mut weight: u8 = 25;
    let mut out = Vec::with_capacity(locs.len());
    for &loc in locs {
        if loc < 0 {
            let abs = -(i64::from(loc) + 1);
            weight = u8::try_from(abs.min(255)).unwrap_or(u8::MAX);
        } else {
            out.push((weight, loc.unsigned_abs()));
        }
    }
    out
}

/// Gunzip a Pagefind file and strip its signature.
pub fn unpack(gz: &[u8]) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    GzDecoder::new(gz)
        .read_to_end(&mut raw)
        .context("gunzip a pagefind file")?;
    match raw.strip_prefix(SIGNATURE) {
        Some(body) => Ok(body.to_vec()),
        None => bail!("file does not start with the pagefind_dcd signature"),
    }
}

/// Every file of a built bundle, by its path relative to the bundle root with
/// `/` separators.
pub type Bundle = BTreeMap<String, Vec<u8>>;

/// Read a bundle from disk.
pub fn read_bundle(dir: &Path) -> Result<Bundle> {
    fn walk(root: &Path, dir: &Path, out: &mut Bundle) -> Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(root, &path, out)?;
            } else {
                let rel = path
                    .strip_prefix(root)?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.insert(rel, std::fs::read(&path)?);
            }
        }
        Ok(())
    }
    let mut out = Bundle::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

/// One language's index, decoded.
#[derive(Debug, Clone)]
pub struct DecodedIndex {
    /// The language key in `pagefind-entry.json`: `unknown` when the pages
    /// carry no `lang`.
    pub language: String,
    /// The meta file's path in the bundle (`pagefind.<hash>.pf_meta`).
    pub meta_file: String,
    /// The meta file.
    pub meta: MetaIndex,
    /// Each word chunk, by file name stem.
    pub chunks: BTreeMap<String, WordIndex>,
    /// Each fragment's JSON, by file name stem.
    pub fragments: BTreeMap<String, serde_json::Value>,
}

/// One page's whole contribution to an index, independent of its page number.
#[derive(Debug, Clone, PartialEq)]
pub struct PageView {
    /// The fragment's `url`.
    pub url: String,
    /// Its page number in this index (not compared).
    pub page_number: usize,
    /// `MetaPage::word_count`.
    pub word_count: u32,
    /// `MetaPage::hash`, the fragment's file name stem.
    pub fragment_hash: String,
    /// The fragment JSON: url, content, word_count, filters, meta, anchors.
    pub fragment: serde_json::Value,
    /// Every word on the page with its raw `locs`.
    pub words: BTreeMap<String, Vec<i32>>,
}

/// Decode every language a bundle holds, from its `pagefind-entry.json`.
pub fn decode_bundle(bundle: &Bundle) -> Result<Vec<DecodedIndex>> {
    let entry: serde_json::Value = serde_json::from_slice(
        bundle
            .get("pagefind-entry.json")
            .ok_or_else(|| anyhow!("bundle has no pagefind-entry.json"))?,
    )
    .context("parse pagefind-entry.json")?;
    let languages = entry
        .get("languages")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("pagefind-entry.json has no languages"))?;

    let mut out = Vec::new();
    for (language, lang) in languages {
        let hash = lang
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("language {language} has no hash"))?;
        let meta_file = format!("pagefind.{hash}.pf_meta");
        let meta_bytes = unpack(
            bundle
                .get(&meta_file)
                .ok_or_else(|| anyhow!("missing {meta_file}"))?,
        )?;
        let meta: MetaIndex =
            minicbor::decode(&meta_bytes).map_err(|e| anyhow!("decode {meta_file}: {e}"))?;

        let mut chunks = BTreeMap::new();
        for chunk in &meta.index_chunks {
            let path = format!("index/{}.pf_index", chunk.hash);
            let bytes = unpack(
                bundle
                    .get(&path)
                    .ok_or_else(|| anyhow!("meta points at missing {path}"))?,
            )?;
            let words: WordIndex =
                minicbor::decode(&bytes).map_err(|e| anyhow!("decode {path}: {e}"))?;
            chunks.insert(chunk.hash.clone(), words);
        }

        let mut fragments = BTreeMap::new();
        for page in &meta.pages {
            let path = format!("fragment/{}.pf_fragment", page.hash);
            let bytes = unpack(
                bundle
                    .get(&path)
                    .ok_or_else(|| anyhow!("meta points at missing {path}"))?,
            )?;
            let json: serde_json::Value =
                serde_json::from_slice(&bytes).with_context(|| format!("parse {path}"))?;
            fragments.insert(page.hash.clone(), json);
        }

        out.push(DecodedIndex {
            language: language.clone(),
            meta_file,
            meta,
            chunks,
            fragments,
        });
    }
    out.sort_by(|a, b| a.language.cmp(&b.language));
    Ok(out)
}

impl DecodedIndex {
    /// Every page, keyed by URL, with its words gathered from every chunk.
    pub fn pages(&self) -> Result<BTreeMap<String, PageView>> {
        let mut by_number: Vec<PageView> = Vec::with_capacity(self.meta.pages.len());
        for (n, page) in self.meta.pages.iter().enumerate() {
            let fragment = self
                .fragments
                .get(&page.hash)
                .cloned()
                .ok_or_else(|| anyhow!("page {n} has no fragment"))?;
            let url = fragment
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("fragment {} has no url", page.hash))?
                .to_string();
            by_number.push(PageView {
                url,
                page_number: n,
                word_count: page.word_count,
                fragment_hash: page.hash.clone(),
                fragment,
                words: BTreeMap::new(),
            });
        }
        for chunk in self.chunks.values() {
            for word in &chunk.words {
                for posting in &word.pages {
                    let n = usize::try_from(posting.page_number)?;
                    let view = by_number.get_mut(n).ok_or_else(|| {
                        anyhow!("word {:?} points at missing page {n}", word.word)
                    })?;
                    if view
                        .words
                        .insert(word.word.clone(), posting.locs.clone())
                        .is_some()
                    {
                        bail!("word {:?} listed twice for page {n}", word.word);
                    }
                }
            }
        }
        let mut out = BTreeMap::new();
        for view in by_number {
            let url = view.url.clone();
            if out.insert(url.clone(), view).is_some() {
                bail!("two pages share the url {url}");
            }
        }
        Ok(out)
    }

    /// Page URLs in page number order: the order the build saw the pages in.
    pub fn page_order(&self) -> Result<Vec<String>> {
        let mut pages: Vec<PageView> = self.pages()?.into_values().collect();
        pages.sort_by_key(|p| p.page_number);
        Ok(pages.into_iter().map(|p| p.url).collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn locs_decode_as_pagefind_web_reads_them() {
        // Two positions at the default weight, then a change to weight 250
        // (Pagefind's 10.0, written -251) for one more.
        assert_eq!(
            decode_locs(&[3, 9, -251, 0]),
            vec![(25, 3), (25, 9), (250, 0)]
        );
    }

    #[test]
    fn unpack_refuses_a_file_without_the_signature() {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"not pagefind").expect("write");
        let bytes = gz.finish().expect("finish");
        assert!(unpack(&bytes).is_err());
    }
}
