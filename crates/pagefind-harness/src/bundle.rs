//! The browser bundle the pinned `pagefind` crate writes, for checking (and on
//! a bump, regenerating) the copy `trovato-search-index` carries.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};

use crate::build::{SourcePage, api_build};

/// Every language Pagefind 1.3.0 ships a WASM backend for.
pub const WASM_LANGUAGES: &[&str] = &[
    "ar", "ca", "da", "de", "el", "en", "es", "eu", "fi", "fr", "ga", "hi", "hu", "hy", "id", "it",
    "lt", "ne", "nl", "no", "pt", "ro", "ru", "sr", "sv", "ta", "tr", "yi",
];

/// The runtime files a bundle carries, by the name the crate writes them under.
pub const RUNTIME_NAMES: &[&str] = &[
    "pagefind.js",
    "pagefind-highlight.js",
    "pagefind-ui.js",
    "pagefind-ui.css",
    "pagefind-modular-ui.js",
    "pagefind-modular-ui.css",
    "wasm.unknown.pagefind",
];

/// The asset path `crates/search-index/assets/pagefind-1.3.0/` keeps a bundle
/// file under.
pub fn asset_name(bundle_name: &str) -> String {
    match bundle_name
        .strip_prefix("wasm.")
        .and_then(|s| s.strip_suffix(".pagefind"))
    {
        Some(lang) => format!("wasm/pagefind_web_bg.{lang}.1.3.0.wasm.gz"),
        None => bundle_name.to_string(),
    }
}

fn page(lang: Option<&str>) -> SourcePage {
    let lang = lang.map(|l| format!(" lang=\"{l}\"")).unwrap_or_default();
    SourcePage {
        file_name: "a.html".into(),
        html: format!("<html{lang}><body><p data-pagefind-body>bundle</p></body></html>"),
    }
}

/// Every runtime file and every language's WASM, as the pinned crate writes
/// them, keyed by asset name.
pub async fn crate_runtime_files() -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    let unknown = api_build(&[page(None)]).await?;
    for name in RUNTIME_NAMES {
        let bytes = unknown
            .get(*name)
            .ok_or_else(|| anyhow!("the crate wrote no {name}"))?;
        out.insert(asset_name(name), bytes.clone());
    }
    for lang in WASM_LANGUAGES {
        let built = api_build(&[page(Some(lang))]).await?;
        let name = format!("wasm.{lang}.pagefind");
        let bytes = built
            .get(&name)
            .ok_or_else(|| anyhow!("the crate wrote no {name}"))?;
        out.insert(asset_name(&name), bytes.clone());
    }
    Ok(out)
}
