// TROVATO: vendored from Pagefind 1.3.0, `pagefind/src/fossick/mod.rs`
// (crate `pagefind` 1.3.0 on crates.io). MIT licensed, Copyright 2022
// CloudCannon; the notice is in `../LICENSE-PAGEFIND`. Changes from upstream
// are marked `TROVATO:`. They are all I/O and wiring: pages are read from a
// string synchronously (upstream reads files or strings through tokio), the
// `extended` (charabia) feature is left out as the released CLI leaves it out,
// URL building from a file path is dropped (the caller names the URL), and
// `hashbrown::HashMap` is `std`'s. The tokenizing itself is unchanged.
use anyhow::Result;
use either::Either;
// TROVATO: was `use hashbrown::HashMap;`
use lazy_static::lazy_static;
use pagefind_stem::{Algorithm, Stemmer};
use regex::Regex;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ops::Mul;

use super::SearchOptions;
use crate::vendor::fragments::{PageAnchorData, PageFragment, PageFragmentData};
use parser::DomParser;

use self::parser::DomParserResult;
use self::splitting::get_discrete_words;

lazy_static! {
    static ref NEWLINES: Regex = Regex::new("(\n|\r\n)+").unwrap();
    static ref TRIM_NEWLINES: Regex = Regex::new("^[\n\r\\s]+|[\n\r\\s]+$").unwrap();
    static ref EXTRANEOUS_SPACES: Regex = Regex::new("\\s{2,}").unwrap();
    static ref PRIVATE_PAGEFIND: Regex = Regex::new("___PAGEFIND_[\\S]+\\s?").unwrap();
    // TODO: i18n?
    static ref SPECIAL_CHARS: Regex = Regex::new("[^\\w]").unwrap();
}

pub mod parser;
mod splitting;

#[derive(Debug, Clone, PartialEq)]
pub struct FossickedWord {
    pub position: u32,
    pub weight: u8,
}

#[derive(Debug, Clone)]
pub struct FossickedData {
    pub url: String,
    pub fragment: PageFragment,
    pub word_data: HashMap<String, Vec<FossickedWord>>,
    pub sort: BTreeMap<String, String>,
    pub has_custom_body: bool,
    pub force_inclusion: bool,
    pub has_html_element: bool,
    pub has_old_bundle_reference: bool,
    pub language: String,
}

#[derive(Debug)]
pub struct Fossicker {
    // TROVATO: upstream also holds a file path and a root path to read from and
    // to build the URL from; here the page is always a string with its URL.
    page_url: Option<String>,
    synthetic_content: Option<String>,
    data: Option<DomParserResult>,
}

impl Fossicker {
    // TROVATO: upstream's `new_synthetic` also takes an optional file path.
    pub fn new_synthetic(page_url: Option<String>, contents: String) -> Self {
        Self {
            page_url,
            synthetic_content: Some(contents),
            data: None,
        }
    }

    // TROVATO: synchronous. Upstream reads the same bytes through a tokio
    // `BufReader` into the same 20,000 byte buffer, which hands the rewriter
    // the string in 20,000 byte slices; this does exactly that.
    fn read_synthetic(&mut self, options: &SearchOptions) {
        let Some(contents) = self.synthetic_content.as_ref() else {
            return;
        };

        let mut rewriter = DomParser::new(options);

        for chunk in contents.as_bytes().chunks(20000) {
            if rewriter.write(chunk).is_err() {
                // TROVATO: upstream logs "Failed to parse file" here and leaves
                // the page without data, which drops it from the index.
                return;
            }
        }

        let mut data = rewriter.wrap();
        if let Some(forced_language) = &options.force_language {
            data.language = forced_language.clone();
        }

        self.data = Some(data);
    }

    fn parse_digest(
        &mut self,
    ) -> (
        String,
        HashMap<String, Vec<FossickedWord>>,
        Vec<(String, String, String, u32)>,
        usize,
    ) {
        let mut map: HashMap<String, Vec<FossickedWord>> = HashMap::new();
        let mut anchors = Vec::new();
        // TODO: push this error handling up a level and return an Err from parse_digest
        if self.data.as_ref().is_none() {
            return ("".into(), map, anchors, 0); // empty page result, will be dropped from search
        }
        let data = self.data.as_ref().unwrap();
        let stemmer = get_stemmer(&data.language);

        let mut content = String::with_capacity(data.digest.len());

        let mut store_word = |full_word: &str, word_index: usize, word_weight: u8| {
            let word = if let Some(stemmer) = &stemmer {
                stemmer.stem(&full_word).into_owned()
            } else {
                full_word.to_string()
            };

            let entry = FossickedWord {
                position: word_index.try_into().unwrap(),
                weight: word_weight,
            };
            if let Some(repeat) = map.get_mut(&word) {
                repeat.push(entry);
            } else {
                map.insert(word, vec![entry]);
            }
        };

        // TODO: Consider reading newlines and jump the word_index up some amount,
        // so that separate bodies of text don't return exact string
        // matches across the boundaries. Or otherwise use some marker byte for the boundary.

        // TODO: Configure this or use segmenting across all languages
        let segment_chunks = data.digest.split_whitespace();

        // TROVATO: upstream's `#[cfg(feature = "extended")]` segmenting branch
        // is left out; this is its `#[cfg(not(feature = "extended"))]` branch.
        let coarse_segments =
            segment_chunks.map(|s| Either::<&str, core::slice::Iter<&str>>::Left(s));

        let mut total_word_index = 0;
        let mut max_word_index = 0;
        let weight_multiplier = 24.0;
        let weight_max = 10.0;
        debug_assert!(((weight_max * weight_multiplier) as u8) < std::u8::MAX);

        let mut weight_stack: Vec<u8> = vec![(1.0 * weight_multiplier) as u8];

        let mut track_word = |word: &str, append_whitespace: bool| {
            if word.chars().next() == Some('_') {
                if word.starts_with("___PAGEFIND_ANCHOR___") {
                    if let Some((element_name, anchor_id)) =
                        word.replace("___PAGEFIND_ANCHOR___", "").split_once(':')
                    {
                        let element_text = data
                            .anchor_content
                            .get(anchor_id)
                            .map(|t| normalize_content(t))
                            .unwrap_or_default();

                        if let Some((_, element_id)) = anchor_id.split_once(':') {
                            anchors.push((
                                element_name.to_string(),
                                element_id.to_string(),
                                normalize_content(&element_text),
                                total_word_index as u32,
                            ));
                        }
                    }
                    return;
                }

                if word.starts_with("___PAGEFIND_WEIGHT___") {
                    let weight = word
                        .replace("___PAGEFIND_WEIGHT___", "")
                        .parse::<f32>()
                        .ok()
                        .unwrap_or(1.0);
                    if weight <= 0.0 {
                        weight_stack.push(0);
                    } else {
                        weight_stack.push(
                            (weight.clamp(0.0, weight_max).mul(weight_multiplier) as u8).max(1),
                        );
                    }
                    return;
                }

                // Auto weights are provided by the parser, and should only
                // apply if we aren't inside an explicitly weighted block,
                // in which case we should just inherit that weight.
                if word.starts_with("___PAGEFIND_AUTO_WEIGHT___") {
                    if weight_stack.len() == 1 {
                        let weight = word
                            .replace("___PAGEFIND_AUTO_WEIGHT___", "")
                            .parse::<f32>()
                            .ok()
                            .unwrap_or(1.0);
                        weight_stack
                            .push(weight.clamp(0.0, weight_max).mul(weight_multiplier) as u8);
                    } else {
                        weight_stack.push(weight_stack.last().cloned().unwrap_or_default());
                    }
                    return;
                }

                if word.starts_with("___END_PAGEFIND_WEIGHT___") {
                    weight_stack.pop();
                    return;
                }
            }

            // We use zero-width spaces as boundary values for some languages,
            // so we make sure that all are removed from the source content before going into the index.
            let normalized_word = word.replace('\u{200B}', "");
            if normalized_word.is_empty() {
                return;
            }

            content.push_str(&word.replace('\u{200B}', ""));
            if append_whitespace {
                content.push(' ');
            }
            let normalized_word = SPECIAL_CHARS
                .replace_all(word, "")
                .into_owned()
                .to_lowercase();

            let word_weight = weight_stack.last().unwrap_or(&1);
            if !normalized_word.is_empty() {
                store_word(&normalized_word, total_word_index, *word_weight);
            }

            // For words that may be CompoundWords, also index them as their constituent parts
            if normalized_word != word {
                let (word_parts, extras) = get_discrete_words(word);
                // Only proceed if the word was broken into multiple parts
                if word_parts.contains(|c: char| c.is_whitespace()) {
                    let part_words: Vec<_> = word_parts.split_whitespace().collect();

                    if !part_words.is_empty() {
                        // Index constituents of a compound word as a proportion of the
                        // weight of the full word.
                        let per_weight = (word_weight
                            / part_words.len().try_into().unwrap_or(std::u8::MAX))
                        .max(1);

                        // Only index two+ character words
                        for part_word in part_words.into_iter().filter(|w| w.len() > 1) {
                            store_word(part_word, total_word_index, per_weight);
                        }
                    }
                }
                // Additionally store any special extra characters we are given
                if let Some(extras) = extras {
                    for extra in extras {
                        store_word(&extra, total_word_index, *word_weight);
                    }
                }
            }

            max_word_index = total_word_index;
            total_word_index += 1;
        };

        for segment in coarse_segments {
            match segment {
                Either::Left(word) => {
                    track_word(word, true);
                }
                Either::Right(words) => {
                    let mut words = words.peekable();
                    while let Some(word) = words.next() {
                        track_word(word, words.peek().is_none());
                    }
                }
            };
        }
        if content.ends_with('\u{200B}') {
            content.pop();
        }
        if content.ends_with(' ') {
            content.pop();
        }
        (content, map, anchors, max_word_index + 1)
    }

    /// Removes private Pagefind sentinel values from content that would otherwise leak.
    /// This should probably be handled better by not inserting these flags here in the first place,
    /// though there's a chance we do want to process them when we arrive at indexing metadata.
    fn tidy_meta_and_filters(&mut self) {
        if let Some(data) = self.data.as_mut() {
            for filter in data.filters.values_mut() {
                for filter_val in filter.iter_mut() {
                    match PRIVATE_PAGEFIND.replace_all(filter_val, "") {
                        std::borrow::Cow::Borrowed(_) => { /* no-op, no replace happened */ }
                        std::borrow::Cow::Owned(s) => *filter_val = s,
                    }
                }
            }

            for meta in data.meta.values_mut() {
                match PRIVATE_PAGEFIND.replace_all(meta, "") {
                    std::borrow::Cow::Borrowed(_) => { /* no-op, no replace happened */ }
                    std::borrow::Cow::Owned(s) => *meta = s,
                }
            }
        }
    }

    // TROVATO: synchronous, and the URL is the one given (upstream falls back to
    // building one from the file path).
    pub fn fossick(mut self, options: &SearchOptions) -> Result<FossickedData> {
        if self.synthetic_content.is_some() && self.data.is_none() {
            self.read_synthetic(options);
        };

        let (content, word_data, anchors, word_count) = self.parse_digest();
        self.tidy_meta_and_filters();

        let Some(data) = self.data else {
            anyhow::bail!("the page could not be parsed");
        };
        let Some(url) = self.page_url.clone() else {
            anyhow::bail!("Tried to index file with no specified URL or file path, ignoring.");
        };

        Ok(FossickedData {
            url: url.clone(),
            has_custom_body: data.has_custom_body,
            force_inclusion: data.force_inclusion,
            has_html_element: data.has_html_element,
            has_old_bundle_reference: data.has_old_bundle_reference,
            language: data.language,
            fragment: PageFragment {
                page_number: 0, // This page number is updated later once determined
                data: PageFragmentData {
                    url,
                    content,
                    filters: data.filters,
                    meta: data.meta,
                    word_count,
                    anchors: anchors
                        .into_iter()
                        .map(|(element, id, text, location)| PageAnchorData {
                            element,
                            id,
                            location,
                            text,
                        })
                        .collect(),
                },
            },
            word_data,
            sort: data.sort,
        })
    }
}

fn normalize_content(content: &str) -> String {
    let content = html_escape::decode_html_entities(content);
    let content = TRIM_NEWLINES.replace_all(&content, "");
    let content = NEWLINES.replace_all(&content, " ");
    let content = EXTRANEOUS_SPACES.replace_all(&content, " ");

    content.to_string()
}

// TODO: These language codes are duplicated with pagefind_web's Cargo.toml
fn get_stemmer(lang: &str) -> Option<Stemmer> {
    match lang.split('-').next().unwrap() {
        "ar" => Some(Stemmer::create(Algorithm::Arabic)),
        "hy" => Some(Stemmer::create(Algorithm::Armenian)),
        "eu" => Some(Stemmer::create(Algorithm::Basque)),
        "ca" => Some(Stemmer::create(Algorithm::Catalan)),
        "da" => Some(Stemmer::create(Algorithm::Danish)),
        "nl" => Some(Stemmer::create(Algorithm::Dutch)),
        "en" => Some(Stemmer::create(Algorithm::English)),
        "fi" => Some(Stemmer::create(Algorithm::Finnish)),
        "fr" => Some(Stemmer::create(Algorithm::French)),
        "de" => Some(Stemmer::create(Algorithm::German)),
        "el" => Some(Stemmer::create(Algorithm::Greek)),
        "hi" => Some(Stemmer::create(Algorithm::Hindi)),
        "hu" => Some(Stemmer::create(Algorithm::Hungarian)),
        "id" => Some(Stemmer::create(Algorithm::Indonesian)),
        "ga" => Some(Stemmer::create(Algorithm::Irish)),
        "it" => Some(Stemmer::create(Algorithm::Italian)),
        "lt" => Some(Stemmer::create(Algorithm::Lithuanian)),
        "ne" => Some(Stemmer::create(Algorithm::Nepali)),
        "no" => Some(Stemmer::create(Algorithm::Norwegian)),
        "pt" => Some(Stemmer::create(Algorithm::Portuguese)),
        "ro" => Some(Stemmer::create(Algorithm::Romanian)),
        "ru" => Some(Stemmer::create(Algorithm::Russian)),
        "sr" => Some(Stemmer::create(Algorithm::Serbian)),
        "es" => Some(Stemmer::create(Algorithm::Spanish)),
        "sv" => Some(Stemmer::create(Algorithm::Swedish)),
        "ta" => Some(Stemmer::create(Algorithm::Tamil)),
        "tr" => Some(Stemmer::create(Algorithm::Turkish)),
        "yi" => Some(Stemmer::create(Algorithm::Yiddish)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizing_content() {
        let input = "\nHello  Wor\n ld? \n \n";
        let output = normalize_content(input);

        assert_eq!(&output, "Hello Wor ld?");
    }

    // TROVATO: upstream builds a `Fossicker` from a CLI configuration and reads
    // it through tokio; the defaults are the same.
    fn test_fossick(s: String) -> Fossicker {
        let opts = SearchOptions::default();
        let mut f = Fossicker::new_synthetic(Some("/test/".into()), s);
        f.read_synthetic(&opts);
        f
    }

    #[test]
    fn parse_file() {
        let mut f =
            test_fossick(["<html><body>", "<p>Hello World!</p>", "</body></html>"].concat());

        let (digest, words, _, _) = f.parse_digest();

        assert_eq!(digest, "Hello World!".to_string());
        assert_eq!(
            words,
            HashMap::from_iter([
                (
                    "hello".to_string(),
                    vec![FossickedWord {
                        position: 0,
                        weight: 1 * 24
                    }]
                ),
                (
                    "world".to_string(),
                    vec![FossickedWord {
                        position: 1,
                        weight: 1 * 24
                    }]
                )
            ])
        );
    }

    #[test]
    fn parse_weighted_file() {
        let mut f = test_fossick(
            [
                "<html><body>",
                "<div>The",
                "<p data-pagefind-weight='2'>Quick Brown</p>",
                "Fox",
                "<p data-pagefind-weight='0.5'>Jumps Over</p>",
                "<p data-pagefind-weight='0.00001'>Ryan</p></div>",
                "</body></html>",
            ]
            .concat(),
        );

        let (digest, words, _, _) = f.parse_digest();

        assert_eq!(digest, "The Quick Brown. Fox Jumps Over. Ryan.".to_string());
        assert_eq!(
            words,
            HashMap::from_iter([
                (
                    "the".to_string(),
                    vec![FossickedWord {
                        position: 0,
                        weight: 1 * 24
                    }]
                ),
                (
                    "quick".to_string(),
                    vec![FossickedWord {
                        position: 1,
                        weight: 2 * 24
                    }]
                ),
                (
                    "brown".to_string(),
                    vec![FossickedWord {
                        position: 2,
                        weight: 2 * 24
                    }]
                ),
                (
                    "fox".to_string(),
                    vec![FossickedWord {
                        position: 3,
                        weight: 1 * 24
                    }]
                ),
                (
                    "jumps".to_string(),
                    vec![FossickedWord {
                        position: 4,
                        weight: 12
                    }]
                ),
                (
                    "over".to_string(),
                    vec![FossickedWord {
                        position: 5,
                        weight: 12
                    }]
                ),
                (
                    "ryan".to_string(),
                    vec![FossickedWord {
                        position: 6,
                        weight: 1
                    }]
                )
            ])
        );
    }

    #[test]
    fn parse_auto_weighted_file() {
        let mut f = test_fossick(
            [
                "<html><body>",
                "<h1>Pagefind</h1>",
                "<h2>Pagefind</h2>",
                "<h3>Pagefind</h3>",
                "<h4>Pagefind</h4>",
                "<h5>Pagefind</h5>",
                "<h6>Pagefind</h6>",
                "<p>Pagefind</p>",
                "<div data-pagefind-weight='0'><h1>Pagefind</h1></div>",
                "</body></html>",
            ]
            .concat(),
        );

        let (_, words, _, _) = f.parse_digest();

        assert_eq!(
            words,
            HashMap::from_iter([(
                "pagefind".to_string(),
                vec![
                    FossickedWord {
                        position: 0,
                        weight: 7 * 24
                    },
                    FossickedWord {
                        position: 1,
                        weight: 6 * 24
                    },
                    FossickedWord {
                        position: 2,
                        weight: 5 * 24
                    },
                    FossickedWord {
                        position: 3,
                        weight: 4 * 24
                    },
                    FossickedWord {
                        position: 4,
                        weight: 3 * 24
                    },
                    FossickedWord {
                        position: 5,
                        weight: 2 * 24
                    },
                    FossickedWord {
                        position: 6,
                        weight: 1 * 24
                    },
                    FossickedWord {
                        position: 7,
                        weight: 0 * 24
                    }
                ]
            )])
        );
    }

    #[test]
    fn parse_bad_weights() {
        let mut f = test_fossick(
            [
                "<html><body>",
                "<p data-pagefind-weight='lots'>The</p>",
                "<p data-pagefind-weight='99999999'>Quick</p>",
                "<p data-pagefind-weight='-1234'>Brown</p>",
                "<p data-pagefind-weight='65.4'>Fox</p>",
                "</body></html>",
            ]
            .concat(),
        );

        let (_, words, _, _) = f.parse_digest();

        assert_eq!(
            words,
            HashMap::from_iter([
                (
                    "the".to_string(),
                    vec![FossickedWord {
                        position: 0,
                        weight: 24
                    }]
                ),
                (
                    "quick".to_string(),
                    vec![FossickedWord {
                        position: 1,
                        weight: 240
                    }]
                ),
                (
                    "brown".to_string(),
                    vec![FossickedWord {
                        position: 2,
                        weight: 0
                    }]
                ),
                (
                    "fox".to_string(),
                    vec![FossickedWord {
                        position: 3,
                        weight: 240
                    }]
                )
            ])
        );
    }

    #[test]
    fn parse_nbsp() {
        let mut f = test_fossick(
            [
                "<html lang='ja'><body>",
                "<p>Hello&nbsp;👋</p>",
                "</body></html>",
            ]
            .concat(),
        );

        let (_, words, _, _) = f.parse_digest();

        let mut words = words.keys().collect::<Vec<_>>();
        words.sort();
        assert_eq!(words, vec!["hello", "👋"]);
    }
}
