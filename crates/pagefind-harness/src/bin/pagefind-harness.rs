//! `pagefind-harness`: export, measure and prove (B216 dev tool).
//!
//! ```text
//! # Export a site's searchable items exactly as the cron job does.
//! pagefind-harness export --out /tmp/site [--sample 60]
//! # Time today's full rebuild, three runs, split into export, CLI and swap.
//! pagefind-harness measure --runs 3 --output /tmp/pagefind-out
//! # The same, through the native build that replaced the CLI (B217).
//! pagefind-harness measure-native --runs 3 --output /tmp/pagefind-native [--cold]
//! # The tokenizer proof over a directory of exported pages.
//! pagefind-harness parity --site crates/pagefind-harness/corpus
//! # Regenerate the vendored browser bundle from the pinned crate.
//! pagefind-harness bundle --out crates/search-index/assets/pagefind-1.3.0
//! ```
//!
//! `export` and `measure` read the site the usual way (`DATABASE_URL`,
//! `REDIS_URL`, `PLUGINS_DIR`, …) and build the full `AppState`, so the export
//! goes through the same `indexable_documents` and `render_fragment`, the same
//! access seam and the same plugins as the cron job. Both turn
//! `MIGRATE_ON_START` off, so neither changes the schema; they still start the
//! server state as `serve` would (plugin discovery included), so run them
//! against a copy of a site, not the site. `export` refuses a non-empty
//! directory. The CLI is `$PAGEFIND_BIN`, else `pagefind` on `PATH`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use trovato_kernel::Config;
use trovato_kernel::cron::{IndexDocument, indexable_documents, render_fragment};
use trovato_kernel::state::AppState;
use trovato_pagefind_harness::build::{cli_version, find_cli, read_site};
use trovato_pagefind_harness::parity::{against_cli, against_crate};

#[derive(Parser)]
#[command(about = "Pagefind index dev tool (B216). Not part of the kernel.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write every searchable item as `<id>.html`, exactly as the cron job does.
    Export {
        /// Directory to write into (created).
        #[arg(long)]
        out: PathBuf,
        /// Keep a deterministic sample of this many items instead of all.
        #[arg(long)]
        sample: Option<usize>,
    },
    /// Time today's full rebuild: export, CLI, swap.
    Measure {
        /// How many rebuilds.
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// The directory the index is swapped into (stands in for
        /// `PAGEFIND_OUTPUT_DIR`).
        #[arg(long)]
        output: PathBuf,
    },
    /// Time the native full build (B217): `indexable_documents`, then the
    /// ledger, term cache, encode and publish, as cron now runs it.
    MeasureNative {
        /// How many rebuilds.
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// The directory the index is published into (stands in for
        /// `PAGEFIND_OUTPUT_DIR`).
        #[arg(long)]
        output: PathBuf,
        /// Empty the term cache before every run, so every page is tokenized.
        #[arg(long)]
        cold: bool,
    },
    /// Per page tokenizing against a full CLI build (and the crate's own).
    Parity {
        /// A directory of exported `<id>.html` pages.
        #[arg(long)]
        site: PathBuf,
    },
    /// Write the pinned crate's browser bundle (runtime files and every
    /// language's WASM) into a directory, as `crates/search-index/assets/`
    /// holds it. Run on a Pagefind bump.
    Bundle {
        /// Directory to write into.
        #[arg(long)]
        out: PathBuf,
    },
    /// Write the deterministic stress corpus (see `stress.rs`) into a directory.
    Stress {
        /// Directory to write into (created).
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match cli.command {
            Command::Export { out, sample } => export(&out, sample).await,
            Command::Measure { runs, output } => measure(runs, &output).await,
            Command::MeasureNative { runs, output, cold } => {
                measure_native(runs, &output, cold).await
            }
            Command::Parity { site } => parity(&site).await,
            Command::Bundle { out } => bundle(&out).await,
            Command::Stress { out } => {
                trovato_pagefind_harness::stress::write(&out)?;
                println!(
                    "wrote {} stress pages to {}",
                    trovato_pagefind_harness::stress::pages().len(),
                    out.display()
                );
                Ok(())
            }
        }
    })
}

async fn app_state() -> Result<AppState> {
    let mut config = Config::from_env().context("load configuration")?;
    // Never change the schema from a dev tool: a pending kernel or plugin
    // migration is `trovato migrate`'s job, not an export's.
    config.migrate_on_start = false;
    AppState::new(&config)
        .await
        .context("initialize application state")
}

/// Write documents as `build_index_inner` does.
async fn write_documents(dir: &Path, documents: &[IndexDocument]) -> Result<()> {
    for document in documents {
        let html = render_fragment(document);
        let file_path = dir.join(format!("{}.html", document.id));
        tokio::fs::write(&file_path, html)
            .await
            .context("failed to write pagefind HTML fragment")?;
    }
    Ok(())
}

async fn export(out: &Path, sample_size: Option<usize>) -> Result<()> {
    let state = app_state().await?;
    let mut documents = indexable_documents(state.db(), state.items()).await?;
    let total = documents.len();
    if let Some(n) = sample_size {
        documents = trovato_pagefind_harness::export::sample(documents, n);
    }
    trovato_pagefind_harness::export::prepare_empty_dir(out)?;
    write_documents(out, &documents).await?;
    let with_comments = documents.iter().filter(|d| !d.comments.is_empty()).count();
    let comments: usize = documents.iter().map(|d| d.comments.len()).sum();
    let mut types: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &documents {
        *types.entry(d.item_type.as_str()).or_default() += 1;
    }
    println!(
        "exported {} of {total} searchable items to {} ({with_comments} with comments, {comments} comments)",
        documents.len(),
        out.display()
    );
    println!("by type: {types:?}");
    Ok(())
}

/// Bytes and files under a directory, by file kind.
fn sizes(dir: &Path) -> Result<BTreeMap<String, (usize, u64)>> {
    fn walk(dir: &Path, out: &mut BTreeMap<String, (usize, u64)>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(&path, out)?;
            } else {
                let kind = path
                    .extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let slot = out.entry(kind).or_default();
                slot.0 += 1;
                slot.1 += entry.metadata()?.len();
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out)?;
    Ok(out)
}

async fn measure(runs: usize, output: &Path) -> Result<()> {
    let bin = find_cli().ok_or_else(|| anyhow!("no pagefind CLI: set PAGEFIND_BIN"))?;
    println!("pagefind CLI: {} ({})", bin.display(), cli_version(&bin)?);
    let init = Instant::now();
    let state = app_state().await?;
    println!(
        "AppState init (not part of a rebuild): {:.2?}",
        init.elapsed()
    );

    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pagefind".into());
    tokio::fs::create_dir_all(&parent).await?;

    println!("| run | items | export | CLI | swap + cleanup | total |");
    println!("|---|---|---|---|---|---|");
    for run in 1..=runs {
        // `build_index`, step for step.
        let id = uuid::Uuid::now_v7().simple();
        let temp = parent.join(format!(".{name}_build_{id}"));
        let backup = parent.join(format!(".{name}_old_{id}"));
        let total = Instant::now();

        let t = Instant::now();
        tokio::fs::create_dir_all(&temp).await?;
        let documents = indexable_documents(state.db(), state.items()).await?;
        write_documents(&temp, &documents).await?;
        let export_time = t.elapsed();

        let t = Instant::now();
        let output_path = temp.join("_pagefind");
        let out = tokio::time::timeout(
            Duration::from_secs(120),
            tokio::process::Command::new(&bin)
                .arg("--site")
                .arg(&temp)
                .arg("--output-path")
                .arg(&output_path)
                .output(),
        )
        .await
        .map_err(|_| anyhow!("pagefind CLI timed out"))??;
        if !out.status.success() {
            bail!(
                "pagefind CLI failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let cli_time = t.elapsed();

        let t = Instant::now();
        // The kernel's swap, except that the new index is checked first, so a
        // CLI that exits 0 without writing one leaves the old index in place.
        let swapped = trovato_pagefind_harness::export::swap_into(&output_path, output, &backup);
        tokio::fs::remove_dir_all(&temp).await.ok();
        swapped?;
        let swap_time = t.elapsed();

        println!(
            "| {run} | {} | {:.0} ms | {:.0} ms | {:.0} ms | {:.0} ms |",
            documents.len(),
            export_time.as_secs_f64() * 1000.0,
            cli_time.as_secs_f64() * 1000.0,
            swap_time.as_secs_f64() * 1000.0,
            total.elapsed().as_secs_f64() * 1000.0,
        );
    }

    println!();
    println!("index at {}:", output.display());
    let mut files = 0;
    let mut bytes = 0;
    for (kind, (n, b)) in sizes(output)? {
        println!("  .{kind}: {n} files, {b} bytes");
        files += n;
        bytes += b;
    }
    println!("  total: {files} files, {bytes} bytes");
    let mut metas: Vec<String> = std::fs::read_dir(output)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".pf_meta") || n.starts_with("wasm."))
        .collect();
    metas.sort();
    println!("  language evidence: {metas:?}");
    let entry = std::fs::read_to_string(output.join("pagefind-entry.json"))?;
    println!("  pagefind-entry.json: {entry}");
    Ok(())
}

async fn parity(site: &Path) -> Result<()> {
    let pages = read_site(site)?;
    println!("{} pages in {}", pages.len(), site.display());

    // What one page costs through the crate's public API: a whole
    // `PagefindIndex`, `add_html_file`, `get_files` (which also minifies
    // `pagefind.js` every time), then the decode.
    let t = Instant::now();
    let singles = trovato_pagefind_harness::parity::per_page(&pages).await?;
    let took = t.elapsed();
    println!(
        "per page through the crate API: {} pages in {:.2?} ({:.2} ms a page)",
        singles.len(),
        took,
        took.as_secs_f64() * 1000.0 / singles.len().max(1) as f64
    );

    let own = against_crate(&pages).await?;
    for (label, report) in [
        ("crate per page", &own.crate_pages),
        ("vendored per page", &own.vendored_pages),
    ] {
        println!(
            "{label} vs the crate's full build: {} pages, {} postings, {} positions, {} mismatches",
            report.pages_compared,
            report.postings_compared,
            report.positions_compared,
            report.mismatches.len()
        );
    }
    print_bytes("native build vs the crate's full build", &own.native_bytes);
    let own_ok = own.crate_pages.is_identical()
        && own.vendored_pages.is_identical()
        && own.native_bytes.index_identical();

    let Some(bin) = find_cli() else {
        println!("SKIPPED the CLI comparison: no pagefind CLI (set PAGEFIND_BIN)");
        return if own_ok {
            Ok(())
        } else {
            bail!(
                "mismatches: {:#?} {:#?} {:#?}",
                own.crate_pages.mismatches,
                own.vendored_pages.mismatches,
                own.native_bytes
            )
        };
    };
    let cli = against_cli(&bin, site).await?;
    println!("CLI: {} ({:.2?})", cli.cli_version, cli.cli_time);
    println!(
        "CLI languages: {:?}, meta files: {:?}",
        cli.languages, cli.meta_files
    );
    for (label, report) in [("crate", &cli.pages), ("vendored", &cli.vendored_pages)] {
        println!(
            "{label} per page vs the CLI: {} pages, {} postings, {} positions, {} mismatches",
            report.pages_compared,
            report.postings_compared,
            report.positions_compared,
            report.mismatches.len()
        );
    }
    for m in cli
        .pages
        .mismatches
        .iter()
        .chain(&cli.vendored_pages.mismatches)
        .chain(&own.crate_pages.mismatches)
        .chain(&own.vendored_pages.mismatches)
        .take(20)
    {
        println!("  MISMATCH {}: {}", m.url, m.what);
    }
    print_bytes(
        "crate in CLI page order vs the CLI",
        &cli.bytes_in_cli_order,
    );
    print_bytes(
        "native build in CLI page order vs the CLI",
        &cli.native_bytes_in_cli_order,
    );
    if own_ok
        && cli.pages.is_identical()
        && cli.vendored_pages.is_identical()
        && cli.bytes_in_cli_order.index_identical()
        && cli.native_bytes_in_cli_order.index_identical()
    {
        println!("PASS");
        Ok(())
    } else {
        bail!("FAIL")
    }
}

/// One byte for byte comparison, with every differing file named.
fn print_bytes(label: &str, b: &trovato_pagefind_harness::compare::ByteReport) {
    println!(
        "{label}, byte for byte: {} of {} files identical, {} differ, {} only in the reference, {} only in the candidate",
        b.identical,
        b.reference_files,
        b.differing.len(),
        b.only_reference.len(),
        b.only_candidate.len()
    );
    for f in b
        .differing
        .iter()
        .chain(&b.only_reference)
        .chain(&b.only_candidate)
        .take(20)
    {
        let kind = if trovato_pagefind_harness::compare::is_index_file(f) {
            "index"
        } else {
            "runtime, not index data"
        };
        println!("  BYTES DIFFER {f} ({kind})");
    }
    println!(
        "  index files byte identical: {}",
        if b.index_identical() { "yes" } else { "NO" }
    );
}

/// Write the browser bundle the pinned `pagefind` crate writes (every runtime
/// file, and the WASM backend of every language it ships) into `out`, under
/// the names `crates/search-index/assets/pagefind-1.3.0/` uses.
async fn bundle(out: &Path) -> Result<()> {
    let files = trovato_pagefind_harness::bundle::crate_runtime_files().await?;
    for (name, bytes) in &files {
        let path = out.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    println!("wrote {} runtime files to {}", files.len(), out.display());
    Ok(())
}

async fn measure_native(runs: usize, output: &Path, cold: bool) -> Result<()> {
    let init = Instant::now();
    let state = app_state().await?;
    println!(
        "AppState init (not part of a rebuild): {:.2?}",
        init.elapsed()
    );
    println!(
        "| run | items | export | tokenize{} | encode | publish | total |",
        if cold { " (cold)" } else { "" }
    );
    println!("|---|---|---|---|---|---|---|");
    for run in 1..=runs {
        if cold {
            sqlx_delete_terms(&state).await?;
        }
        let total = Instant::now();
        let t = Instant::now();
        let snapshot = trovato_kernel::cron::search_index_generation(state.db()).await?;
        let documents = indexable_documents(state.db(), state.items()).await?;
        let export_time = t.elapsed();
        let report =
            trovato_kernel::cron::search_index_full_build(state.db(), output, &documents, snapshot)
                .await?;
        println!(
            "| {run} | {} | {:.0} ms | {} ms ({} tokenized, {} cached) | {} ms | {} ms | {:.0} ms |",
            report.pages,
            export_time.as_secs_f64() * 1000.0,
            report.tokenize_ms,
            report.tokenized,
            report.cached,
            report.encode_ms,
            report.publish_ms,
            total.elapsed().as_secs_f64() * 1000.0,
        );
    }
    println!();
    println!("index at {}:", output.display());
    for (kind, (n, b)) in sizes(output)? {
        println!("  .{kind}: {n} files, {b} bytes");
    }
    println!(
        "  entry: {}",
        std::fs::read_to_string(output.join("pagefind-entry.json"))?
    );
    Ok(())
}

/// Empty the term cache, so the next build tokenizes every page.
async fn sqlx_delete_terms(state: &AppState) -> Result<()> {
    let pool = state.db();
    sqlx::query("DELETE FROM search_page_terms")
        .execute(pool)
        .await
        .context("empty the term cache")?;
    Ok(())
}
