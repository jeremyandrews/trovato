//! Markdown tables of one run, or of a before and an after side by side.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::{Latency, QueryResult, Results};

fn load(path: &PathBuf) -> Result<Results> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn mb(bytes: i64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_048_576.0)
}

fn ms(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn speedup(before: f64, after: f64) -> String {
    if after <= 0.0 {
        return "n/a".into();
    }
    format!("{:.1}x", before / after)
}

fn cold(l: Option<&Latency>) -> (String, String) {
    match l {
        Some(l) if l.n > 0 => (ms(l.p50_ms), ms(l.p95_ms)),
        _ => ("-".into(), "-".into()),
    }
}

/// Print the report for `files` (one run, or before and after).
pub fn print(files: &[PathBuf]) -> Result<()> {
    let runs: Vec<Results> = files.iter().map(load).collect::<Result<_>>()?;
    match runs.as_slice() {
        [one] => print_one(one),
        [before, after] => print_pair(before, after),
        _ => bail!("give one results file, or two (before, then after)"),
    }
    Ok(())
}

fn print_fixture(r: &Results) {
    println!("Fixture: `{}`", r.fixture);
    println!();
    println!("| Item type | Items | Published |");
    println!("|---|---:|---:|");
    for (t, n, p) in &r.item_counts {
        println!("| `{t}` | {n} | {p} |");
    }
    println!();
    println!(
        "Revisions: {}. Average stored `fields` of a `bench_main` item: {:.0} bytes (compressed, as stored).",
        r.revisions, r.avg_fields_bytes
    );
    println!();
    println!("{}", r.postgres_version);
    println!();
    println!("| Setting | Value |");
    println!("|---|---|");
    for (k, v) in &r.settings {
        println!("| `{k}` | {v} |");
    }
    println!();
}

fn print_sizes(r: &Results) {
    println!("| Relation | Table | TOAST | Indexes | Total |");
    println!("|---|---:|---:|---:|---:|");
    for s in &r.sizes {
        println!(
            "| `{}` | {} | {} | {} | {} |",
            s.name,
            mb(s.table_bytes),
            mb(s.toast_bytes),
            mb(s.index_bytes),
            mb(s.total_bytes)
        );
    }
    println!();
    println!("| Index on `item` | Size |");
    println!("|---|---:|");
    for (name, bytes) in &r.item_indexes {
        println!("| `{name}` | {} |", mb(*bytes));
    }
    println!();
    println!("Hot set ({}):", r.hot_set_note);
    println!();
    if !r.hot_set.is_empty() {
        println!("| Relation | In shared buffers |");
        println!("|---|---:|");
        let total: i64 = r.hot_set.iter().map(|(_, b)| b).sum();
        for (name, bytes) in &r.hot_set {
            println!("| `{name}` | {} |", mb(*bytes));
        }
        println!("| total | {} |", mb(total));
        println!();
    }
}

fn row(q: &QueryResult) -> String {
    let (c50, c95) = cold(q.cold.as_ref());
    let buffers = format!("{} hit / {} read", q.plan.shared_hit, q.plan.shared_read);
    format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {buffers} | {} |",
        q.id,
        q.what,
        q.rows,
        q.total.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
        q.statements,
        ms(q.warm.p50_ms),
        ms(q.warm.p95_ms),
        ms(q.warm.p99_ms),
        c50,
        c95,
        if let Some(e) = &q.error {
            let e = e.replace('|', "/");
            let short = e.rsplit(": ").next().unwrap_or(&e).to_string();
            format!(
                "ERROR ({} warm, {} cold runs failed): {}",
                q.warm_failures, q.cold_failures, short
            )
        } else {
            format!("`{}`", q.plan.shape)
        }
    )
}

fn print_one(r: &Results) {
    println!("## Run `{}`", r.label);
    println!();
    print_fixture(r);
    println!("{}", r.cold_note);
    println!();
    println!(
        "| Query | What | Rows | Total | Statements | Warm p50 ms | Warm p95 ms | Warm p99 ms | Cold p50 ms | Cold p95 ms | Buffers (slowest statement) | Plan (slowest statement) |"
    );
    println!("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|");
    for q in &r.queries {
        println!("{}", row(q));
    }
    println!();
    println!(
        "Item save (`ItemService::update`, {} saves of a `bench_main` item): p50 {} ms, p99 {} ms.",
        r.save.n,
        ms(r.save.p50_ms),
        ms(r.save.p99_ms)
    );
    println!();
    print_sizes(r);
}

fn print_pair(before: &Results, after: &Results) {
    println!("## Before and after");
    println!();
    println!(
        "| Query | What | Before warm p50 / p95 / p99 ms | After warm p50 / p95 / p99 ms | Speedup (p95) | Before cold p50 / p95 | After cold p50 / p95 | Before plan | After plan |"
    );
    println!("|---|---|---|---|---:|---|---|---|---|");
    for b in &before.queries {
        let Some(a) = after.queries.iter().find(|a| a.id == b.id) else {
            continue;
        };
        let (bc50, bc95) = cold(b.cold.as_ref());
        let (ac50, ac95) = cold(a.cold.as_ref());
        println!(
            "| {} | {} | {} / {} / {} | {} / {} / {} | {} | {} / {} | {} / {} | `{}` | `{}` |",
            b.id,
            b.what,
            ms(b.warm.p50_ms),
            ms(b.warm.p95_ms),
            ms(b.warm.p99_ms),
            ms(a.warm.p50_ms),
            ms(a.warm.p95_ms),
            ms(a.warm.p99_ms),
            speedup(b.warm.p95_ms, a.warm.p95_ms),
            bc50,
            bc95,
            ac50,
            ac95,
            b.plan.shape,
            a.plan.shape
        );
    }
    println!();
    println!(
        "Item save p50 / p99: before {} / {} ms, after {} / {} ms.",
        ms(before.save.p50_ms),
        ms(before.save.p99_ms),
        ms(after.save.p50_ms),
        ms(after.save.p99_ms)
    );
    println!();
    println!("### After: sizes");
    println!();
    print_sizes(after);
}
