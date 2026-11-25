//! The browser bundle and the entry file.
//!
//! The runtime files (`pagefind.js`, the UI scripts and styles, and the WASM
//! search backend per language) are copied byte for byte from the `pagefind`
//! crate at exactly 1.3.0, the release whose index format the codec writes, so
//! a bundle and its index always match. `pagefind.js` is the one file the
//! crate generates rather than ships: it minifies `pagefind_web.js` and
//! `pagefind_coupled_search.js` behind a version line at build time; the copy
//! here is that output. `crates/pagefind-harness` checks every one of them
//! against the crate (`pagefind-harness bundle` regenerates them on a bump).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::PAGEFIND_VERSION;
use crate::assemble::LanguageIndex;

/// Every file of a bundle, by its path relative to the bundle root with `/`
/// separators.
pub type Bundle = BTreeMap<String, Vec<u8>>;

/// The entry file: the one fixed-name file that changes on every publish.
pub const ENTRY_FILE: &str = "pagefind-entry.json";

macro_rules! asset {
    ($path:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/pagefind-1.3.0/",
            $path
        ))
    };
}

/// The fixed-name runtime files every bundle carries, by name. Written
/// uncompressed, as Pagefind writes them (the WASM file is already gzip).
pub const RUNTIME_FILES: &[(&str, &[u8])] = &[
    ("pagefind.js", asset!("pagefind.js")),
    ("pagefind-highlight.js", asset!("pagefind-highlight.js")),
    ("pagefind-ui.js", asset!("pagefind-ui.js")),
    ("pagefind-ui.css", asset!("pagefind-ui.css")),
    ("pagefind-modular-ui.js", asset!("pagefind-modular-ui.js")),
    ("pagefind-modular-ui.css", asset!("pagefind-modular-ui.css")),
    (
        "wasm.unknown.pagefind",
        asset!("wasm/pagefind_web_bg.unknown.1.3.0.wasm.gz"),
    ),
];

/// The language-specific WASM backend for a language key, by its base
/// language (`en` for `en-gb`), as Pagefind 1.3.0's `lang_wasm_path` finds it.
/// `None` for `unknown` and for a language Pagefind has no stemmer for.
pub fn language_wasm(language: &str) -> Option<&'static [u8]> {
    if language == "unknown" {
        return None;
    }
    let base = language.split('-').next().unwrap_or(language);
    Some(match base {
        "ar" => asset!("wasm/pagefind_web_bg.ar.1.3.0.wasm.gz"),
        "ca" => asset!("wasm/pagefind_web_bg.ca.1.3.0.wasm.gz"),
        "da" => asset!("wasm/pagefind_web_bg.da.1.3.0.wasm.gz"),
        "de" => asset!("wasm/pagefind_web_bg.de.1.3.0.wasm.gz"),
        "el" => asset!("wasm/pagefind_web_bg.el.1.3.0.wasm.gz"),
        "en" => asset!("wasm/pagefind_web_bg.en.1.3.0.wasm.gz"),
        "es" => asset!("wasm/pagefind_web_bg.es.1.3.0.wasm.gz"),
        "eu" => asset!("wasm/pagefind_web_bg.eu.1.3.0.wasm.gz"),
        "fi" => asset!("wasm/pagefind_web_bg.fi.1.3.0.wasm.gz"),
        "fr" => asset!("wasm/pagefind_web_bg.fr.1.3.0.wasm.gz"),
        "ga" => asset!("wasm/pagefind_web_bg.ga.1.3.0.wasm.gz"),
        "hi" => asset!("wasm/pagefind_web_bg.hi.1.3.0.wasm.gz"),
        "hu" => asset!("wasm/pagefind_web_bg.hu.1.3.0.wasm.gz"),
        "hy" => asset!("wasm/pagefind_web_bg.hy.1.3.0.wasm.gz"),
        "id" => asset!("wasm/pagefind_web_bg.id.1.3.0.wasm.gz"),
        "it" => asset!("wasm/pagefind_web_bg.it.1.3.0.wasm.gz"),
        "lt" => asset!("wasm/pagefind_web_bg.lt.1.3.0.wasm.gz"),
        "ne" => asset!("wasm/pagefind_web_bg.ne.1.3.0.wasm.gz"),
        "nl" => asset!("wasm/pagefind_web_bg.nl.1.3.0.wasm.gz"),
        "no" => asset!("wasm/pagefind_web_bg.no.1.3.0.wasm.gz"),
        "pt" => asset!("wasm/pagefind_web_bg.pt.1.3.0.wasm.gz"),
        "ro" => asset!("wasm/pagefind_web_bg.ro.1.3.0.wasm.gz"),
        "ru" => asset!("wasm/pagefind_web_bg.ru.1.3.0.wasm.gz"),
        "sr" => asset!("wasm/pagefind_web_bg.sr.1.3.0.wasm.gz"),
        "sv" => asset!("wasm/pagefind_web_bg.sv.1.3.0.wasm.gz"),
        "ta" => asset!("wasm/pagefind_web_bg.ta.1.3.0.wasm.gz"),
        "tr" => asset!("wasm/pagefind_web_bg.tr.1.3.0.wasm.gz"),
        "yi" => asset!("wasm/pagefind_web_bg.yi.1.3.0.wasm.gz"),
        _ => return None,
    })
}

#[derive(Serialize)]
struct Entry<'a> {
    version: &'static str,
    languages: BTreeMap<&'a str, EntryLanguage<'a>>,
}

#[derive(Serialize)]
struct EntryLanguage<'a> {
    hash: &'a str,
    wasm: Option<&'a str>,
    page_count: usize,
}

/// `pagefind-entry.json` for these language indexes:
/// `{version, languages: {lang: {hash, wasm, page_count}}}`.
///
/// Languages are written in key order. Pagefind writes them in its hash map's
/// order, which is the same for one language and arbitrary for more; the
/// browser reads the object by key either way.
pub fn entry_json(indexes: &[LanguageIndex]) -> Result<String> {
    let languages: Vec<(&str, &str, usize)> = indexes
        .iter()
        .map(|index| {
            (
                index.language.as_str(),
                index.meta_hash.as_str(),
                index.page_count,
            )
        })
        .collect();
    entry_json_for(&languages)
}

/// [`entry_json`] from each language's `(language, meta stem, page slots)`, for
/// an update that rewrote only some languages.
pub fn entry_json_for(languages: &[(&str, &str, usize)]) -> Result<String> {
    let entry = Entry {
        version: PAGEFIND_VERSION,
        languages: languages
            .iter()
            .map(|(language, hash, page_count)| {
                let wasm = language_wasm(language).map(|_| *language);
                (
                    *language,
                    EntryLanguage {
                        hash,
                        wasm,
                        page_count: *page_count,
                    },
                )
            })
            .collect(),
    };
    serde_json::to_string(&entry).context("serialize pagefind-entry.json")
}

/// The whole bundle for these language indexes: the runtime files, each
/// language's WASM, every index file, and the entry file.
pub fn assemble_bundle(indexes: &[LanguageIndex]) -> Result<Bundle> {
    let mut bundle = Bundle::new();
    for (name, bytes) in RUNTIME_FILES {
        bundle.insert((*name).to_string(), bytes.to_vec());
    }
    for index in indexes {
        if let Some(wasm) = language_wasm(&index.language) {
            bundle.insert(format!("wasm.{}.pagefind", index.language), wasm.to_vec());
        }
        for (path, bytes) in &index.files {
            bundle.insert(path.clone(), bytes.clone());
        }
    }
    bundle.insert(ENTRY_FILE.to_string(), entry_json(indexes)?.into_bytes());
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stemmed_language_has_its_wasm_and_unknown_has_none() {
        assert!(language_wasm("en").is_some());
        assert!(language_wasm("en-gb").is_some());
        assert!(language_wasm("de").is_some());
        assert!(language_wasm("unknown").is_none());
        assert!(language_wasm("xx").is_none());
    }

    #[test]
    fn the_entry_file_has_pagefinds_shape() {
        let index = LanguageIndex {
            language: "en".into(),
            meta_hash: "en_123".into(),
            page_count: 3,
            word_count: 0,
            files: BTreeMap::new(),
            chunks: Vec::new(),
            plan: Vec::new(),
        };
        assert_eq!(
            entry_json(&[index]).ok().as_deref(),
            Some(
                "{\"version\":\"1.3.0\",\"languages\":{\"en\":{\"hash\":\"en_123\",\"wasm\":\"en\",\"page_count\":3}}}"
            )
        );
    }
}
