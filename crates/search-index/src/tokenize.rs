//! One page in, its whole contribution to an index out.
//!
//! [`tokenize()`] runs the vendored Pagefind 1.3.0 tokenizer over one page's HTML
//! and returns everything the index needs from that page and nothing that
//! depends on the other pages: its language, its word count, its fragment JSON,
//! and its words with their locations already encoded. The page number is not
//! part of it; the assembler supplies that. That independence is what lets the
//! term cache keep a page's terms between builds.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use minicbor::{Decode, Encode};

use crate::vendor::SearchOptions;
use crate::vendor::fossick::{FossickedWord, Fossicker};

/// One page, tokenized.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct PageTerms {
    /// The Pagefind language key: the page's `<html lang>`, lowercased, or
    /// `unknown` when it has none.
    #[n(0)]
    pub language: String,
    /// The page's word count, the BM25 document length.
    #[n(1)]
    pub word_count: u32,
    /// The fragment JSON, exactly as Pagefind writes it.
    #[n(2)]
    pub fragment: String,
    /// Every word on the page with its `locs`, encoded as Pagefind writes them
    /// into a posting: positions, with a weight change written inline as
    /// `-(weight) - 1`, the default weight first.
    #[n(3)]
    pub words: BTreeMap<String, Vec<i32>>,
}

impl PageTerms {
    /// The words as the term cache stores them: a CBOR map of word to `locs`.
    pub fn words_cbor(&self) -> Result<Vec<u8>> {
        crate::codec::to_cbor(&self.words)
    }

    /// Words read back from [`PageTerms::words_cbor`].
    pub fn words_from_cbor(bytes: &[u8]) -> Result<BTreeMap<String, Vec<i32>>> {
        minicbor::decode(bytes).map_err(|e| anyhow::anyhow!("decode cached words: {e}"))
    }
}

/// Tokenize one page as Pagefind 1.3.0 would, with `url` as the page's URL.
///
/// `None` when the page has no words: Pagefind leaves such a page out of the
/// index altogether (`build_indexes` keeps only pages whose word data is not
/// empty), so the caller must too.
///
/// Every Trovato page has a `data-pagefind-body`, so Pagefind's other filter
/// (drop pages without one once any page has one) never removes a page here.
pub fn tokenize(url: &str, html: &str) -> Result<Option<PageTerms>> {
    let options = SearchOptions::default();
    let data = Fossicker::new_synthetic(Some(url.to_string()), html.to_string())
        .fossick(&options)
        .with_context(|| format!("tokenize {url}"))?;
    if data.word_data.is_empty() {
        return Ok(None);
    }
    let fragment =
        serde_json::to_string(&data.fragment.data).context("serialize the page fragment")?;
    let word_count = u32::try_from(data.fragment.data.word_count)
        .with_context(|| format!("{url} has too many words"))?;

    let mut words = BTreeMap::new();
    for (word, positions) in data.word_data {
        words.insert(word, encode_locs(positions)?);
    }
    Ok(Some(PageTerms {
        language: data.language,
        word_count,
        fragment,
        words,
    }))
}

/// One word's positions on one page, encoded as `pagefind/src/index/mod.rs`
/// `build_indexes` encodes them at 1.3.0.
fn encode_locs(mut positions: Vec<FossickedWord>) -> Result<Vec<i32>> {
    // A page weight of 1 is encoded as 25 upstream; the sort puts weight 25
    // first and every other weight after it in ascending order. (The tokenizer
    // writes Pagefind's 1.0 as 24, so in practice every list starts with a
    // weight change. This is upstream's behaviour, copied, not a fix.)
    positions.sort_by_cached_key(|p| if p.weight == 25 { 0 } else { p.weight });

    let mut current_weight = 25;
    let mut weighted = Vec::with_capacity(positions.len());
    for FossickedWord { position, weight } in positions {
        let position = i32::try_from(position).context("a word position past i32")?;
        if weight != current_weight {
            weighted.extend([-i32::from(weight) - 1, position]);
            current_weight = weight;
        } else {
            weighted.push(position);
        }
    }
    Ok(weighted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(lang: &str, body: &str) -> String {
        format!(
            "<html lang=\"{lang}\"><head><title>t</title></head><body>\
             <div data-pagefind-body>{body}</div></body></html>"
        )
    }

    #[test]
    fn a_page_tokenizes_with_its_language_and_weights() {
        let terms = tokenize(
            "/a.html",
            &page(
                "en",
                "<h1 data-pagefind-weight=\"10.0\">Running dogs</h1><p>A dog runs.</p>",
            ),
        )
        .ok()
        .flatten();
        let Some(terms) = terms else {
            panic!("the page has words");
        };
        assert_eq!(terms.language, "en");
        assert_eq!(terms.word_count, 5);
        // English stems: "running" and "runs" are both "run", "dogs" is "dog".
        assert!(terms.words.contains_key("run"), "{:?}", terms.words);
        assert!(terms.words.contains_key("dog"), "{:?}", terms.words);
        assert!(!terms.words.contains_key("running"));
        // "run" at position 0 (weight 10.0, 240) and 4 (weight 1.0, 24): the
        // default 24 is not 25, so both carry a change, ascending.
        assert_eq!(terms.words["run"], vec![-25, 4, -241, 0]);
        assert!(
            terms
                .fragment
                .starts_with("{\"url\":\"/a.html\",\"content\":")
        );
    }

    #[test]
    fn a_page_without_a_language_is_unknown_and_unstemmed() {
        let Some(terms) = tokenize("/b.html", "<html><body><p>Running dogs</p></body></html>")
            .ok()
            .flatten()
        else {
            panic!("the page has words");
        };
        assert_eq!(terms.language, "unknown");
        assert!(terms.words.contains_key("running"));
    }

    #[test]
    fn a_page_with_no_words_is_left_out() {
        assert_eq!(tokenize("/c.html", &page("en", "")).ok().flatten(), None);
    }

    #[test]
    fn cached_words_round_trip() {
        let Some(terms) = tokenize("/d.html", &page("de", "<p>Häuser und Bäume</p>"))
            .ok()
            .flatten()
        else {
            panic!("the page has words");
        };
        let bytes = terms.words_cbor().unwrap_or_default();
        assert_eq!(PageTerms::words_from_cbor(&bytes).ok(), Some(terms.words));
    }
}
