//! The tokenizer proof.
//!
//! (a) the whole corpus through a reference build (the CLI, or the crate over
//! every page); (b) each page alone through `PagefindIndex::add_html_file` and
//! `get_files`, decoded; (c) the reference decoded, pages matched by URL, and
//! every page's words, positions, weights, word count, meta and fragment
//! asserted identical.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::build::{SourcePage, api_build, cli_build, single_page};
use crate::compare::{ByteReport, PageReport, compare_bytes, compare_pages};
use crate::decode::{Bundle, PageView, decode_bundle, read_bundle};

/// Every page of a bundle, by URL, across every language it holds.
pub fn bundle_pages(bundle: &Bundle) -> Result<(Vec<String>, BTreeMap<String, PageView>)> {
    let mut languages = Vec::new();
    let mut pages = BTreeMap::new();
    for index in decode_bundle(bundle)? {
        languages.push(index.language.clone());
        for (url, view) in index.pages()? {
            if pages.insert(url.clone(), view).is_some() {
                bail!("url {url} is in two languages");
            }
        }
    }
    Ok((languages, pages))
}

/// Step (b): each page alone. Each must come out as exactly one page.
pub async fn per_page(pages: &[SourcePage]) -> Result<BTreeMap<String, PageView>> {
    let mut out = BTreeMap::new();
    for page in pages {
        let bundle = single_page(page).await?;
        let (_, views) = bundle_pages(&bundle)?;
        if views.len() != 1 {
            bail!(
                "{} indexed alone gave {} pages, not 1",
                page.file_name,
                views.len()
            );
        }
        out.extend(views);
    }
    Ok(out)
}

/// The proof against a CLI build.
#[derive(Debug)]
pub struct CliParity {
    /// `pagefind --version`.
    pub cli_version: String,
    /// The languages the CLI built (from `pagefind-entry.json`).
    pub languages: Vec<String>,
    /// The CLI's meta file name, which carries the language.
    pub meta_files: Vec<String>,
    /// The CLI's wall clock.
    pub cli_time: Duration,
    /// Per page tokenizing against the CLI, by URL.
    pub pages: PageReport,
    /// The crate over every page, in the CLI's page order, against the CLI's
    /// bundle, byte for byte.
    pub bytes_in_cli_order: ByteReport,
}

/// Run the proof against the CLI at `bin` over the pages in `site`.
pub async fn against_cli(bin: &Path, site: &Path) -> Result<CliParity> {
    let pages = crate::build::read_site(site)?;
    if pages.is_empty() {
        bail!("no .html files in {}", site.display());
    }
    let out = std::env::temp_dir().join(format!(
        "b216-pagefind-cli-{}",
        uuid::Uuid::now_v7().simple()
    ));
    let cli_time = cli_build(bin, site, &out)?;
    let reference = read_bundle(&out);
    std::fs::remove_dir_all(&out).ok();
    let reference = reference?;

    let indexes = decode_bundle(&reference)?;
    let meta_files = indexes.iter().map(|i| i.meta_file.clone()).collect();
    let (languages, cli_pages) = bundle_pages(&reference)?;

    let singles = per_page(&pages).await?;
    let page_report = compare_pages(&cli_pages, &singles);

    // The CLI's page order, recovered from its page numbers, then the same
    // pages through the crate in that order.
    let mut order: Vec<&PageView> = cli_pages.values().collect();
    order.sort_by_key(|p| p.page_number);
    let by_url: BTreeMap<String, &SourcePage> = pages
        .iter()
        .map(|p| (format!("/{}", p.file_name), p))
        .collect();
    let ordered = order
        .iter()
        .map(|view| {
            by_url
                .get(&view.url)
                .map(|p| (*p).clone())
                .ok_or_else(|| anyhow!("CLI page {} is not in the site", view.url))
        })
        .collect::<Result<Vec<_>>>()?;
    let api = api_build(&ordered)
        .await
        .context("crate build in CLI order")?;
    let bytes_in_cli_order = compare_bytes(&reference, &api);

    Ok(CliParity {
        cli_version: crate::build::cli_version(bin).unwrap_or_default(),
        languages,
        meta_files,
        cli_time,
        pages: page_report,
        bytes_in_cli_order,
    })
}

/// The proof against the crate's own full build, which needs no CLI.
pub async fn against_crate(pages: &[SourcePage]) -> Result<PageReport> {
    let reference = api_build(pages).await?;
    let (_, full) = bundle_pages(&reference)?;
    let singles = per_page(pages).await?;
    Ok(compare_pages(&full, &singles))
}
