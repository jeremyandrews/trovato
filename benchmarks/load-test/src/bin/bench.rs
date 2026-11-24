// A command-line tool run by hand: a failure (no server, an unreadable
// baseline) is reported by `expect` and ends the run, which is all it can do.
#![allow(clippy::expect_used)]
//! Timing benchmark: median and p99 latency of a few pages, several runs.
//!
//! Each run makes `--warmup` untimed requests to a page, then `--requests`
//! timed ones from `--concurrency` workers, and computes the median and the
//! 99th percentile of the timed ones. The pages are measured one after the
//! other, never together, so one page's work does not slow another's. The
//! report is the median over the runs of each figure, with every run's
//! numbers beside it, as JSON (`--out`) and as a table on stdout.
//!
//! With `--baseline <file>` the table also shows the change against a report
//! saved earlier (for example `benchmarks/baseline.json`).
//!
//! Timings depend on the machine, so this is run by hand or on a schedule,
//! never on a pull request. `scripts/bench.sh` wraps it.
//!
//! ```sh
//! cargo run --release -p trovato-loadtest --bin bench -- \
//!   --base-url http://127.0.0.1:3000 \
//!   --page item=/item/<id> --page front=/ --page feed=/blog/feed.xml \
//!   --runs 3 --out /tmp/bench.json
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug)]
#[command(about = "Median and p99 latency of a few pages over several runs")]
struct Args {
    /// Base URL of a running Trovato server.
    #[arg(long, default_value = "http://127.0.0.1:3000")]
    base_url: String,

    /// A page to measure, `name=path`. Repeat for more pages.
    #[arg(long = "page", required = true)]
    pages: Vec<String>,

    /// Timed requests per page per run.
    #[arg(long, default_value_t = 2000)]
    requests: usize,

    /// Untimed requests per page before each run's timed ones.
    #[arg(long, default_value_t = 200)]
    warmup: usize,

    /// Requests in flight at once.
    #[arg(long, default_value_t = 8)]
    concurrency: usize,

    /// Runs; the report is the median over them.
    #[arg(long, default_value_t = 3)]
    runs: usize,

    /// A label for the report (a branch, a commit, "tracing on").
    #[arg(long, default_value = "")]
    label: String,

    /// Write the report here as JSON.
    #[arg(long)]
    out: Option<std::path::PathBuf>,

    /// Compare with a report written earlier.
    #[arg(long)]
    baseline: Option<std::path::PathBuf>,
}

/// One page's figures for one run, in milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Run {
    median_ms: f64,
    p99_ms: f64,
    errors: usize,
}

/// One page over all runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Page {
    path: String,
    median_ms: f64,
    p99_ms: f64,
    runs: Vec<Run>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Report {
    label: String,
    requests: usize,
    concurrency: usize,
    pages: BTreeMap<String, Page>,
}

/// The `q` quantile (0..=1) of sorted `values`, nearest rank.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// `count` requests to `url` from `concurrency` workers; the latencies of the
/// successful ones, and how many failed.
async fn hammer(
    client: &reqwest::Client,
    url: &str,
    count: usize,
    concurrency: usize,
) -> (Vec<f64>, usize) {
    let next = Arc::new(AtomicUsize::new(0));
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..concurrency.max(1) {
        let (client, url, next) = (client.clone(), url.to_string(), next.clone());
        workers.spawn(async move {
            let (mut times, mut errors) = (Vec::new(), 0usize);
            while next.fetch_add(1, Ordering::Relaxed) < count {
                let started = Instant::now();
                let ok = match client.get(&url).send().await {
                    Ok(response) => {
                        let ok = response.status().is_success();
                        // The body is part of the page.
                        ok && response.bytes().await.is_ok()
                    }
                    Err(_) => false,
                };
                if ok {
                    times.push(started.elapsed().as_secs_f64() * 1000.0);
                } else {
                    errors += 1;
                }
            }
            (times, errors)
        });
    }
    let (mut times, mut errors) = (Vec::with_capacity(count), 0);
    while let Some(done) = workers.join_next().await {
        let (t, e) = done.expect("bench worker");
        times.extend(t);
        errors += e;
    }
    times.sort_by(f64::total_cmp);
    (times, errors)
}

fn change(now: f64, before: f64) -> String {
    if before > 0.0 {
        format!("{:+.1}%", (now - before) / before * 100.0)
    } else {
        "-".to_string()
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    // Read before measuring: a bad path should not cost a whole run.
    let baseline: Option<Report> = args.baseline.as_ref().map(|p| {
        let text = std::fs::read_to_string(p).expect("read the baseline");
        serde_json::from_str(&text).expect("parse the baseline")
    });
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(args.concurrency.max(1))
        .timeout(Duration::from_secs(60))
        .build()
        .expect("HTTP client");

    let pages: Vec<(String, String)> = args
        .pages
        .iter()
        .map(|p| {
            let (name, path) = p
                .split_once('=')
                .unwrap_or_else(|| panic!("--page wants name=path, got {p:?}"));
            (name.to_string(), path.to_string())
        })
        .collect();

    let mut report = Report {
        label: args.label.clone(),
        requests: args.requests,
        concurrency: args.concurrency,
        pages: BTreeMap::new(),
    };
    for (name, path) in &pages {
        let url = format!("{}{}", args.base_url.trim_end_matches('/'), path);
        let mut runs = Vec::new();
        for run in 1..=args.runs {
            hammer(&client, &url, args.warmup, args.concurrency).await;
            let (times, errors) = hammer(&client, &url, args.requests, args.concurrency).await;
            let r = Run {
                median_ms: quantile(&times, 0.5),
                p99_ms: quantile(&times, 0.99),
                errors,
            };
            eprintln!(
                "{name} run {run}: median {:.2} ms, p99 {:.2} ms, {} errors",
                r.median_ms, r.p99_ms, r.errors
            );
            runs.push(r);
        }
        let medians: Vec<f64> = runs.iter().map(|r| r.median_ms).collect();
        let p99s: Vec<f64> = runs.iter().map(|r| r.p99_ms).collect();
        report.pages.insert(
            name.clone(),
            Page {
                path: path.clone(),
                median_ms: median(&medians),
                p99_ms: median(&p99s),
                runs,
            },
        );
    }

    println!(
        "| page | median ms | p99 ms | runs (median / p99) | errors |{}",
        if baseline.is_some() {
            " vs baseline (median / p99) |"
        } else {
            ""
        }
    );
    println!(
        "|---|---|---|---|---|{}",
        if baseline.is_some() { "---|" } else { "" }
    );
    for (name, page) in &report.pages {
        let runs = page
            .runs
            .iter()
            .map(|r| format!("{:.2} / {:.2}", r.median_ms, r.p99_ms))
            .collect::<Vec<_>>()
            .join(", ");
        let errors: usize = page.runs.iter().map(|r| r.errors).sum();
        let vs = baseline
            .as_ref()
            .and_then(|b| b.pages.get(name))
            .map(|b| {
                format!(
                    " {} / {} |",
                    change(page.median_ms, b.median_ms),
                    change(page.p99_ms, b.p99_ms)
                )
            })
            .unwrap_or_else(|| {
                if baseline.is_some() {
                    " - |".to_string()
                } else {
                    String::new()
                }
            });
        println!(
            "| {name} | {:.2} | {:.2} | {runs} | {errors} |{vs}",
            page.median_ms, page.p99_ms
        );
    }
    if let Some(out) = &args.out {
        std::fs::write(
            out,
            serde_json::to_string_pretty(&report).expect("serialize"),
        )
        .expect("write the report");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_are_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(quantile(&v, 0.5), 50.0);
        assert_eq!(quantile(&v, 0.99), 99.0);
        assert_eq!(quantile(&[3.0], 0.99), 3.0);
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    }
}
