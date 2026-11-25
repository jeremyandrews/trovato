//! Assembling one language's index from its page slots.
//!
//! This is `pagefind/src/index/mod.rs` `build_indexes` at 1.3.0, rewritten to
//! take pages already tokenized ([`PageTerms`]) at page numbers the caller
//! chose, rather than numbering them itself in the order it was handed them.
//! Everything else is upstream's rule, step for step: postings in page order,
//! words in byte order, a new chunk whenever the running sum over postings of
//! `locs.len() + 1` reaches 20,000, chunk boundaries shortened with
//! `get_prefixes`, and file names cut from the content hash, collisions
//! extended in build order.
//!
//! A slot may be a tombstone: a page number that no live page holds. It is
//! written as a page with no words whose fragment is the language's one shared
//! tombstone fragment (empty url, content and meta), so no meta file ever
//! names a missing file and no word posts to it.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};

use crate::PAGEFIND_VERSION;
use crate::codec::{
    MetaChunk, MetaIndex, MetaPage, PackedPage, PackedWord, WordIndex, full_hash, meta_name, pack,
    prefix_chars, short_name, to_cbor,
};
use crate::tokenize::PageTerms;

/// Pagefind 1.3.0's word chunk cut (`chunk_index(word_map, 20000)`).
pub const CHUNK_LOCATIONS: usize = 20_000;

/// One page number's occupant.
#[derive(Debug, Clone, Copy)]
pub enum Slot<'a> {
    /// A live page.
    Page(&'a PageTerms),
    /// A freed page number.
    Tombstone,
}

/// One language's index, encoded.
#[derive(Debug, Clone)]
pub struct LanguageIndex {
    /// The language key.
    pub language: String,
    /// The meta file's name stem, `<lang>_<hash>`, as `pagefind-entry.json`
    /// names it.
    pub meta_hash: String,
    /// The number of page slots, tombstones included.
    pub page_count: usize,
    /// The number of distinct words.
    pub word_count: usize,
    /// Every file, by its path in the bundle, gzipped as Pagefind writes it:
    /// the meta file, the fragments and the word chunks.
    pub files: BTreeMap<String, Vec<u8>>,
    /// The word chunk plan this build cut, in order.
    pub chunks: Vec<MetaChunk>,
}

/// The fragment a tombstone slot points at: Pagefind's `PageFragmentData`
/// with everything empty.
pub fn tombstone_fragment() -> String {
    "{\"url\":\"\",\"content\":\"\",\"word_count\":0,\"filters\":{},\"meta\":{},\"anchors\":[]}"
        .to_string()
}

/// Build one language's index from its slots, page number `i` being
/// `slots[i]`.
pub fn build_language(language: &str, slots: &[Slot<'_>]) -> Result<LanguageIndex> {
    if u32::try_from(slots.len()).is_err() {
        bail!(
            "Language {language} has too many documents to index, must be < {}",
            u32::MAX
        );
    }
    let tombstone = tombstone_fragment();

    let mut word_map: HashMap<&str, PackedWord> = HashMap::new();
    // Short name -> full name, for collisions; and each page's short name.
    let mut fragment_names: HashMap<String, String> = HashMap::new();
    let mut fragment_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut meta_pages = Vec::with_capacity(slots.len());

    for (page_number, slot) in slots.iter().enumerate() {
        let page_number = page_number as u64;
        let (encoded, word_count) = match slot {
            Slot::Page(page) => {
                for (word, locs) in &page.words {
                    let posting = PackedPage {
                        page_number,
                        locs: locs.clone(),
                    };
                    word_map
                        .entry(word.as_str())
                        .or_insert_with(|| PackedWord {
                            word: word.clone(),
                            pages: Vec::new(),
                        })
                        .pages
                        .push(posting);
                }
                (page.fragment.as_str(), page.word_count)
            }
            Slot::Tombstone => (tombstone.as_str(), 0),
        };

        let full = format!("{language}_{}", full_hash(encoded.as_bytes()));
        let mut short = short_name(&full, language);
        // Upstream extends a colliding name one character at a time, in build
        // order. Two byte-identical fragments share one file (upstream intends
        // this; at 1.3.0 its loop never ends on one, which no CLI build can
        // therefore contain, while every tombstone here is one).
        loop {
            match fragment_names.get(&short) {
                Some(existing) if *existing == full => break,
                Some(_) => short = prefix_chars(&full, short.len() + 1),
                None => {
                    fragment_names.insert(short.clone(), full.clone());
                    fragment_files.insert(
                        format!("fragment/{short}.pf_fragment"),
                        pack(encoded.as_bytes())?,
                    );
                    break;
                }
            }
        }
        meta_pages.push(MetaPage {
            hash: short,
            word_count,
        });
    }

    let word_count = word_map.len();
    let chunks = chunk_index(word_map.into_values().collect(), CHUNK_LOCATIONS);
    let mut chunk_meta = chunk_meta(&chunks);

    let mut files = fragment_files;
    let mut chunk_names: HashMap<String, ()> = HashMap::new();
    for (i, chunk) in chunks.into_iter().enumerate() {
        let bytes = to_cbor(&WordIndex { words: chunk })?;
        let hash = format!("{language}_{}", full_hash(&bytes));
        let mut short = short_name(&hash, language);
        // Upstream's word chunk collision rule, as written: two characters
        // longer per step (`&hash[0..=short.len() + 1]`), stopping at the full
        // name.
        while chunk_names.contains_key(&short) {
            short = prefix_chars(&hash, short.len() + 2);
            if short.len() == hash.len() {
                break;
            }
        }
        chunk_names.insert(short.clone(), ());
        files.insert(format!("index/{short}.pf_index"), pack(&bytes)?);
        if let Some(meta) = chunk_meta.get_mut(i) {
            meta.hash = short;
        }
    }

    let meta = MetaIndex {
        version: PAGEFIND_VERSION.to_string(),
        pages: meta_pages,
        index_chunks: chunk_meta.clone(),
        filters: Vec::new(),
        sorts: Vec::new(),
    };
    let meta_bytes = to_cbor(&meta)?;
    let meta_hash = meta_name(&meta_bytes, language);
    files.insert(format!("pagefind.{meta_hash}.pf_meta"), pack(&meta_bytes)?);

    Ok(LanguageIndex {
        language: language.to_string(),
        meta_hash,
        page_count: slots.len(),
        word_count,
        files,
        chunks: chunk_meta,
    })
}

/// Upstream's `chunk_index`: words in byte order, a cut whenever the running
/// sum over postings of `locs.len() + 1` reaches `chunk_size`.
fn chunk_index(mut words: Vec<PackedWord>, chunk_size: usize) -> Vec<Vec<PackedWord>> {
    words.sort_by(|a, b| a.word.cmp(&b.word));

    let mut index_chunks = Vec::new();
    let mut index_chunk = Vec::new();
    let mut index_chunk_size = 0;
    for word in words {
        index_chunk_size += word.pages.iter().map(|p| p.locs.len() + 1).sum::<usize>();
        index_chunk.push(word);
        if index_chunk_size >= chunk_size {
            index_chunks.push(std::mem::take(&mut index_chunk));
            index_chunk_size = 0;
        }
    }
    if !index_chunk.is_empty() {
        index_chunks.push(index_chunk);
    }
    index_chunks
}

/// Upstream's `chunk_meta`: each chunk's first and last word, then every
/// neighbouring pair of boundaries shortened with [`get_prefixes`].
fn chunk_meta(indexes: &[Vec<PackedWord>]) -> Vec<MetaChunk> {
    let mut named: Vec<MetaChunk> = indexes
        .iter()
        .map(|chunk| MetaChunk {
            from: chunk.first().map_or(String::new(), |w| w.word.clone()),
            to: chunk.last().map_or(String::new(), |w| w.word.clone()),
            hash: String::new(),
        })
        .collect();
    for i in 1..named.len() {
        let (left, right) = named.split_at_mut(i);
        if let (Some(a), Some(b)) = (left.last_mut(), right.first_mut()) {
            let (to, from) = get_prefixes((&a.to, &b.from));
            a.to = to;
            b.from = from;
        }
    }
    named
}

/// Upstream's `get_prefixes`: the shortest prefixes, in characters, that still
/// tell two neighbouring chunk boundaries apart.
pub fn get_prefixes((a, b): (&str, &str)) -> (String, String) {
    let common_prefix_length: usize = b
        .chars()
        .zip(a.chars())
        .take_while(|&(a, b)| a == b)
        .count();
    let a_prefix = a.chars().take(common_prefix_length + 1).collect::<String>();
    let b_prefix = b.chars().take(common_prefix_length + 1).collect::<String>();
    (a_prefix, b_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(word: &str, pages: &[(u64, Vec<i32>)]) -> PackedWord {
        PackedWord {
            word: word.into(),
            pages: pages
                .iter()
                .map(|(page_number, locs)| PackedPage {
                    page_number: *page_number,
                    locs: locs.clone(),
                })
                .collect(),
        }
    }

    // Upstream's own `build_index_chunks` and `build_chunk_meta` tests.
    fn test_words() -> Vec<PackedWord> {
        vec![
            word("apple", &[(1, vec![20, 40, 60]), (5, vec![3, 6, 9])]),
            word("apricot", &[(5, vec![45, 3432, 6003])]),
            word("banana", &[(5, vec![100, 500, 900, 566])]),
            word("peach", &[(5, vec![383, 2, 678])]),
        ]
    }

    #[test]
    fn build_index_chunks() {
        let chunks = chunk_index(test_words(), 8);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0][0].word, "apple");
        assert_eq!(chunks[1][0].word, "apricot");
        assert_eq!(chunks[1][1].word, "banana");
        assert_eq!(chunks[2][0].word, "peach");
    }

    #[test]
    fn build_chunk_meta() {
        let meta = chunk_meta(&chunk_index(test_words(), 8));
        let ranges: Vec<(&str, &str)> = meta
            .iter()
            .map(|c| (c.from.as_str(), c.to.as_str()))
            .collect();
        assert_eq!(ranges, vec![("apple", "app"), ("apr", "b"), ("p", "peach")]);
    }

    #[test]
    fn common_prefix() {
        assert_eq!(
            get_prefixes(("apple", "apricot")),
            ("app".into(), "apr".into())
        );
        assert_eq!(
            get_prefixes(("cataraman", "yacht")),
            ("c".into(), "y".into())
        );
        assert_eq!(
            get_prefixes(("cath", "cathartic")),
            ("cath".into(), "catha".into())
        );
    }

    #[test]
    fn tombstones_share_one_fragment_and_post_no_words() {
        let page = crate::tokenize::tokenize(
            "/a.html",
            "<html lang=\"en\"><body><p data-pagefind-body>hello world</p></body></html>",
        )
        .ok()
        .flatten();
        let Some(page) = page else {
            panic!("the page has words");
        };
        let slots = [Slot::Tombstone, Slot::Page(&page), Slot::Tombstone];
        let Ok(index) = build_language("en", &slots) else {
            panic!("build");
        };
        assert_eq!(index.page_count, 3);
        let fragments = index
            .files
            .keys()
            .filter(|k| k.starts_with("fragment/"))
            .count();
        assert_eq!(fragments, 2, "one page fragment and one shared tombstone");
        let Ok(decoded) = crate::decode::decode_bundle(
            &crate::bundle::assemble_bundle(std::slice::from_ref(&index)).unwrap_or_default(),
        ) else {
            panic!("decode");
        };
        let Ok(pages) = decoded[0].pages() else {
            panic!("pages");
        };
        assert_eq!(pages.len(), 1, "tombstones are not pages: {pages:?}");
        let live = &pages["/a.html"];
        assert_eq!(live.page_number, 1);
        assert_eq!(decoded[0].meta.pages[0].word_count, 0);
        assert_eq!(decoded[0].meta.pages[0].hash, decoded[0].meta.pages[2].hash);
    }
}
