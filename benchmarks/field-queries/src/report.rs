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

/// Whether the `quantile` of a query's warm runs falls on a failed run.
///
/// A failed run has no time: it timed out, or never answered. Counting it as
/// slower than every success, the percentile is a failure as soon as its rank
/// among all runs is past the last success. Two timeouts in 30 runs leave
/// p50 measured, and make p95 and p99 "timed out": reporting the p95 of the
/// 28 that finished would hide exactly the slow tail it exists to show.
fn censored(q: &QueryResult, quantile: f64) -> bool {
    let total = q.warm.n + q.warm_failures;
    if total == 0 {
        return true;
    }
    let rank = ((quantile * total as f64).ceil() as usize).clamp(1, total);
    rank > q.warm.n
}

/// What a failed run was, in the few words a table cell has.
fn failure(q: &QueryResult) -> &'static str {
    match q.error.as_deref() {
        Some(e) if e.contains("statement timeout") => "timed out",
        Some(e) if e.contains("operator does not exist") => "failed: `text >= integer`",
        Some(_) => "failed",
        None => "-",
    }
}

/// One warm percentile: its time, or how the run at its rank failed.
fn percentile(q: &QueryResult, quantile: f64, value: f64) -> String {
    if censored(q, quantile) {
        failure(q).into()
    } else {
        ms(value)
    }
}

/// A query's warm p50 / p95 / p99, each a time or a failure, and how many
/// of its timed runs failed when any did.
fn warm(q: &QueryResult) -> String {
    // A failed median means p95 and p99 failed too: one word for all three.
    let cells = if censored(q, 0.50) {
        failure(q).to_string()
    } else {
        format!(
            "{} / {} / {}",
            ms(q.warm.p50_ms),
            percentile(q, 0.95, q.warm.p95_ms),
            percentile(q, 0.99, q.warm.p99_ms)
        )
    };
    if q.warm_failures > 0 {
        format!(
            "{cells} ({} of {} runs failed)",
            q.warm_failures,
            q.warm.n + q.warm_failures
        )
    } else {
        cells
    }
}

/// The p95 speedup from `b` to `a`, only when both p95s are times.
fn pair_speedup(b: &QueryResult, a: &QueryResult) -> String {
    match (censored(b, 0.95), censored(a, 0.95)) {
        (false, false) => speedup(b.warm.p95_ms, a.warm.p95_ms),
        (true, false) => "was failing".into(),
        (_, true) => "fails after".into(),
    }
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
        [before, after] => print_pair(before, after)?,
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
    if !r.field_indexes.is_empty() {
        println!("| Field index (B207) | Kind | State | Build time | Size |");
        println!("|---|---|---|---:|---:|");
        for (field, kind, state, ms, bytes) in &r.field_indexes {
            println!(
                "| `{field}` | {kind} | {state} | {:.1} s | {} |",
                ms.unwrap_or(0) as f64 / 1000.0,
                mb(*bytes)
            );
        }
        println!();
    }
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
        percentile(q, 0.50, q.warm.p50_ms),
        percentile(q, 0.95, q.warm.p95_ms),
        percentile(q, 0.99, q.warm.p99_ms),
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

/// Refuse to compare runs of different fixtures: a speedup between 100k and
/// 1M items, or between two seeds, measures the data, not the change.
fn same_fixture(before: &Results, after: &Results) -> Result<()> {
    if before.fixture != after.fixture {
        bail!(
            "the runs are of different fixtures: `{}` before, `{}` after",
            before.fixture,
            after.fixture
        );
    }
    if before.item_counts != after.item_counts {
        bail!(
            "the runs counted different items ({:?} before, {:?} after): the same fixture changed between them",
            before.item_counts,
            after.item_counts
        );
    }
    Ok(())
}

/// The before and after rows, by query id, with a query that only one run
/// has kept as a row of its own rather than dropped.
fn pair_rows(before: &Results, after: &Results) -> Vec<String> {
    let mut rows = Vec::new();
    for b in &before.queries {
        let Some(a) = after.queries.iter().find(|a| a.id == b.id) else {
            rows.push(format!(
                "| {} | {} | {} | not run | - | - | - | `{}` | - |",
                b.id,
                b.what,
                warm(b),
                b.plan.shape
            ));
            continue;
        };
        let (bc50, bc95) = cold(b.cold.as_ref());
        let (ac50, ac95) = cold(a.cold.as_ref());
        rows.push(format!(
            "| {} | {} | {} | {} | {} | {} / {} | {} / {} | `{}` | `{}` |",
            b.id,
            b.what,
            warm(b),
            warm(a),
            pair_speedup(b, a),
            bc50,
            bc95,
            ac50,
            ac95,
            b.plan.shape,
            a.plan.shape
        ));
    }
    for a in &after.queries {
        if !before.queries.iter().any(|b| b.id == a.id) {
            rows.push(format!(
                "| {} | {} | not run | {} | - | - | - | - | `{}` |",
                a.id,
                a.what,
                warm(a),
                a.plan.shape
            ));
        }
    }
    rows
}

fn print_pair(before: &Results, after: &Results) -> Result<()> {
    same_fixture(before, after)?;
    println!("## Before and after");
    println!();
    println!(
        "| Query | What | Before warm p50 / p95 / p99 ms | After warm p50 / p95 / p99 ms | Speedup (p95) | Before cold p50 / p95 | After cold p50 / p95 | Before plan | After plan |"
    );
    println!("|---|---|---|---|---:|---|---|---|---|");
    for row in pair_rows(before, after) {
        println!("{row}");
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
    Ok(())
}

// Tests are allowed to use unwrap/expect freely.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const TIMEOUT: &str = "canceling statement due to statement timeout";

    fn query(id: &str, mut samples: Vec<f64>, failures: usize, error: Option<&str>) -> QueryResult {
        QueryResult {
            id: id.into(),
            what: format!("query {id}"),
            warm: Latency::of(&mut samples),
            warm_failures: failures,
            error: error.map(str::to_string),
            ..QueryResult::default()
        }
    }

    #[test]
    fn a_query_with_no_samples_is_a_failure_not_zero() {
        // Every timed run timed out.
        let q = query("Q03", vec![], 30, Some(TIMEOUT));
        assert_eq!(warm(&q), "timed out (30 of 30 runs failed)");
        // A results file from before failures were counted: no samples and
        // no count, only the error.
        let q = query("Q17", vec![], 0, Some(TIMEOUT));
        assert_eq!(warm(&q), "timed out");
        let q = query(
            "Q04",
            vec![],
            1,
            Some("operator does not exist: text >= integer"),
        );
        assert!(
            warm(&q).starts_with("failed: `text >= integer`"),
            "{}",
            warm(&q)
        );
        assert!(!row(&q).contains("0.00"), "{}", row(&q));
    }

    #[test]
    fn timeouts_count_above_every_sample() {
        // 28 runs of 40 ms and two timeouts: the median is a time, p95 and
        // p99 fall on the timeouts.
        let q = query("Q09", vec![40.0; 28], 2, Some(TIMEOUT));
        assert!(!censored(&q, 0.50));
        assert!(censored(&q, 0.95));
        assert!(censored(&q, 0.99));
        assert_eq!(
            warm(&q),
            "40.0 / timed out / timed out (2 of 30 runs failed)"
        );

        // Two timeouts in 100 runs reach p99 only: one is just the maximum.
        let q = query("Q10", vec![40.0; 98], 2, Some(TIMEOUT));
        assert!(!censored(
            &query("Q10", vec![40.0; 99], 1, Some(TIMEOUT)),
            0.99
        ));
        assert!(!censored(&q, 0.95));
        assert!(censored(&q, 0.99));

        // No failures: all three are times.
        let q = query("Q01", vec![10.0; 30], 0, None);
        assert_eq!(warm(&q), "10.0 / 10.0 / 10.0");
    }

    #[test]
    fn a_speedup_needs_a_measured_p95_on_both_sides() {
        let fast = query("Q09", vec![40.0; 30], 0, None);
        let slow = query("Q09", vec![4000.0; 30], 0, None);
        let timing_out = query("Q09", vec![40.0; 28], 2, Some(TIMEOUT));
        let failing = query("Q09", vec![], 30, Some(TIMEOUT));
        assert_eq!(pair_speedup(&slow, &fast), "100.0x");
        assert_eq!(pair_speedup(&failing, &fast), "was failing");
        assert_eq!(pair_speedup(&slow, &timing_out), "fails after");
        assert_eq!(pair_speedup(&failing, &failing), "fails after");
    }

    fn run(fixture: serde_json::Value, main_items: i64, queries: Vec<QueryResult>) -> Results {
        Results {
            fixture,
            item_counts: vec![("bench_main".into(), main_items, main_items)],
            queries,
            ..Results::default()
        }
    }

    #[test]
    fn runs_of_different_fixtures_are_not_compared() {
        let q = || vec![query("Q01", vec![10.0; 30], 0, None)];
        let small = serde_json::json!({"main_items": 100_000, "seed": 207});
        let large = serde_json::json!({"main_items": 1_000_000, "seed": 207});
        let err = same_fixture(
            &run(small.clone(), 100_000, q()),
            &run(large, 1_000_000, q()),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("different fixtures"), "{err}");
        // The same marker, but the data changed in between.
        let err = same_fixture(
            &run(small.clone(), 100_000, q()),
            &run(small.clone(), 99_000, q()),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("changed between them"), "{err}");
        assert!(same_fixture(&run(small.clone(), 100_000, q()), &run(small, 100_000, q())).is_ok());
    }

    #[test]
    fn a_query_only_one_run_has_is_shown() {
        let fixture = serde_json::json!({"seed": 207});
        let before = run(
            fixture.clone(),
            10,
            vec![
                query("Q01", vec![10.0; 30], 0, None),
                query("Q02", vec![20.0; 30], 0, None),
            ],
        );
        let after = run(
            fixture,
            10,
            vec![
                query("Q01", vec![1.0; 30], 0, None),
                query("Q25", vec![2.0; 30], 0, None),
            ],
        );
        let rows = pair_rows(&before, &after);
        assert_eq!(rows.len(), 3, "{rows:#?}");
        assert!(
            rows[0].starts_with("| Q01 ") && rows[0].contains("10.0x"),
            "{}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("| Q02 ") && rows[1].contains("| not run |"),
            "{}",
            rows[1]
        );
        assert!(
            rows[2].starts_with("| Q25 ") && rows[2].contains("| not run |"),
            "{}",
            rows[2]
        );
    }
}
