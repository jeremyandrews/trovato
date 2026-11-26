//! Reading a built Pagefind 1.3.0 index back into structs.
//!
//! Copied from `crates/pagefind-harness` (B216), which used it to prove the
//! tokenizer, and moved here so the native build, its gates and the harness
//! share one decoder. Positions and weights are decoded the way
//! `pagefind_web/src/index.rs` decodes them; fragments are read as JSON.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

pub use crate::bundle::Bundle;
pub use crate::codec::{
    MetaChunk, MetaFilter, MetaIndex, MetaPage, MetaSort, PackedPage, PackedWord, SIGNATURE,
    WordIndex, decode_locs, unpack,
};
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
            // A tombstone: a freed page number with no url and no words.
            if view.url.is_empty() && view.word_count == 0 {
                if !view.words.is_empty() {
                    bail!("tombstone page {} has words", view.page_number);
                }
                continue;
            }
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

/// Every file a reader of the published index in `dir` can be sent to, by
/// bundle path: `pagefind-entry.json`, each meta file it names, and every
/// fragment and word chunk those metas name.
///
/// This is the reader's invariant, checked: it fails if the entry is missing
/// or unreadable, or if any meta it names, or any file such a meta names, is
/// not on disk. Publishing keeps it true at every moment, including between
/// any two writes of an interrupted publish.
pub fn published_files(dir: &Path) -> Result<std::collections::BTreeSet<String>> {
    let entry_path = dir.join(crate::bundle::ENTRY_FILE);
    let entry: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&entry_path).with_context(|| format!("read {}", entry_path.display()))?,
    )
    .context("parse pagefind-entry.json")?;
    let languages = entry
        .get("languages")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("pagefind-entry.json has no languages"))?;
    let mut files = std::collections::BTreeSet::new();
    files.insert(crate::bundle::ENTRY_FILE.to_string());
    for (language, lang) in languages {
        let hash = lang
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("language {language} has no hash"))?;
        let meta_file = format!("pagefind.{hash}.pf_meta");
        let bytes = std::fs::read(dir.join(&meta_file))
            .with_context(|| format!("the entry names {meta_file}, which is not on disk"))?;
        let meta: MetaIndex =
            minicbor::decode(&unpack(&bytes)?).map_err(|e| anyhow!("decode {meta_file}: {e}"))?;
        files.insert(meta_file.clone());
        let named = meta
            .index_chunks
            .iter()
            .map(|c| format!("index/{}.pf_index", c.hash))
            .chain(
                meta.pages
                    .iter()
                    .map(|p| format!("fragment/{}.pf_fragment", p.hash)),
            )
            .chain(
                meta.filters
                    .iter()
                    .map(|f| format!("filter/{}.pf_filter", f.hash)),
            );
        for path in named {
            if !dir.join(&path).is_file() {
                bail!("{meta_file} names {path}, which is not on disk");
            }
            files.insert(path);
        }
    }
    Ok(files)
}

/// The published index in `dir` as a bundle: exactly the files
/// [`published_files`] lists, without the runtime and without anything kept
/// on disk only for older readers.
pub fn read_published(dir: &Path) -> Result<Bundle> {
    let mut out = Bundle::new();
    for path in published_files(dir)? {
        let bytes = std::fs::read(dir.join(&path)).with_context(|| format!("read {path}"))?;
        out.insert(path, bytes);
    }
    Ok(out)
}
