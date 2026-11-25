//! Making indexes: with the CLI, and with the `pagefind` crate.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use pagefind::api::PagefindIndex;

use crate::decode::Bundle;

/// One exported page: its file name in the export directory (`<id>.html`,
/// which is also the source path the CLI computes its URL from) and its HTML.
#[derive(Debug, Clone)]
pub struct SourcePage {
    /// `<id>.html`.
    pub file_name: String,
    /// The fragment `render_fragment` wrote.
    pub html: String,
}

/// Read every `*.html` in a directory, sorted by file name.
pub fn read_site(dir: &Path) -> Result<Vec<SourcePage>> {
    let mut pages = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("html") {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let html = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            pages.push(SourcePage { file_name, html });
        }
    }
    pages.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(pages)
}

/// The `pagefind` CLI: `$PAGEFIND_BIN` when set, else the first `pagefind` on
/// `PATH`. `None` when there is neither.
pub fn find_cli() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("PAGEFIND_BIN") {
        let bin = PathBuf::from(bin);
        return bin.is_file().then_some(bin);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("pagefind"))
        .find(|candidate| candidate.is_file())
}

/// The CLI's `--version`, for the report and the pin check.
pub fn cli_version(bin: &Path) -> Result<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .context("run pagefind --version")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run the CLI exactly as `build_index_inner` does: `--site <dir>
/// --output-path <out>`, nothing else. Returns its wall clock.
pub fn cli_build(bin: &Path, site: &Path, out: &Path) -> Result<Duration> {
    let start = Instant::now();
    let output = std::process::Command::new(bin)
        .arg("--site")
        .arg(site)
        .arg("--output-path")
        .arg(out)
        .output()
        .context("run the pagefind CLI")?;
    let took = start.elapsed();
    if !output.status.success() {
        bail!(
            "pagefind CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(took)
}

/// Pages through the crate's service API, in the order given, then
/// `get_files`. Default service options, which match the CLI's defaults.
pub async fn api_build(pages: &[SourcePage]) -> Result<Bundle> {
    let mut index = PagefindIndex::new(None)?;
    for page in pages {
        index
            .add_html_file(Some(page.file_name.clone()), None, page.html.clone())
            .await
            .with_context(|| format!("add {}", page.file_name))?;
    }
    let files = index.get_files().await?;
    Ok(files
        .into_iter()
        .map(|f| {
            let name = f
                .filename
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            (name, f.contents)
        })
        .collect())
}

/// One page alone: `add_html_file`, then `get_files`.
pub async fn single_page(page: &SourcePage) -> Result<Bundle> {
    api_build(std::slice::from_ref(page)).await
}
