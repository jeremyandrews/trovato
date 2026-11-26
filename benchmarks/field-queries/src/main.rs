//! `field-bench`: how fast Gather's field filters, sorts and facets are on an
//! item type with 200 fields and millions of items.
//!
//! See `docs/performance/field-queries.md` for what it measures, the results,
//! and the exact commands that reproduce them. In short:
//!
//! ```sh
//! export DATABASE_URL=postgres://trovato:trovato@127.0.0.1:55207/field_bench
//! export REDIS_URL=redis://127.0.0.1:56207
//! cargo run --release -p trovato-field-bench -- seed --main-items 1000000 --other-items 1000000
//! cargo run --release -p trovato-field-bench -- run --label before --out before.json \
//!     --cold-command 'docker restart b207-pg && docker run --rm --privileged alpine sh -c "sync; echo 3 > /proc/sys/vm/drop_caches"'
//! cargo run --release -p trovato-field-bench -- report before.json
//! ```
//!
//! It only ever writes to a database whose name starts with `field_bench`,
//! and creates that database when it is missing: never the shared test
//! database, never a site.

mod matrix;
mod report;
mod seed;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{AssertSqlSafe, PgPool};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use trovato_kernel::config::Config;
use trovato_kernel::gather::QueryContext;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::matrix::{Case, Run};
use crate::seed::{MAIN_TYPE, SeedOptions};

#[derive(Parser)]
#[command(name = "field-bench", about = "Gather field query benchmark")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create (if missing), migrate and seed the benchmark database.
    Seed {
        /// Items of the 200 field type.
        #[arg(long, default_value_t = 1_000_000)]
        main_items: i64,
        /// Items spread over the four other types.
        #[arg(long, default_value_t = 1_000_000)]
        other_items: i64,
        /// The seed every value is derived from.
        #[arg(long, default_value_t = 207)]
        seed: u64,
        /// Rows per statement (at least 1).
        #[arg(long, default_value_t = 2_000, value_parser = clap::value_parser!(i64).range(1..))]
        batch: i64,
        /// Statements in flight at once.
        #[arg(long, default_value_t = 16)]
        parallel: usize,
    },
    /// Run the query matrix and the save measurement; write the results.
    Run {
        /// A name for this run, e.g. `before` or `after`.
        #[arg(long)]
        label: String,
        /// Where to write the results (JSON).
        #[arg(long)]
        out: PathBuf,
        /// Timed warm runs of each query.
        #[arg(long, default_value_t = 30)]
        iterations: usize,
        /// Timed cold runs of each query (each after `--cold-command`).
        #[arg(long, default_value_t = 30)]
        cold_iterations: usize,
        /// Shell command that empties Postgres's buffers and the OS page
        /// cache. Without it the cold runs are skipped and the results say
        /// so.
        #[arg(long)]
        cold_command: Option<String>,
        /// Where to write each query's `EXPLAIN (ANALYZE, BUFFERS)` text.
        #[arg(long)]
        explain_dir: Option<PathBuf>,
        /// Item saves to time.
        #[arg(long, default_value_t = 100)]
        saves: usize,
        /// Run only these query ids (comma separated).
        #[arg(long)]
        only: Option<String>,
    },
    /// Print the results of one run, or of a before and an after, as Markdown.
    Report {
        /// One or two results files: before, then after.
        files: Vec<PathBuf>,
    },
    /// Drop the benchmark database.
    Drop,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match cli.command {
            Command::Report { files } => report::print(&files),
            Command::Drop => drop_database().await,
            Command::Seed {
                main_items,
                other_items,
                seed,
                batch,
                parallel,
            } => {
                let opts = SeedOptions {
                    main_items,
                    other_items,
                    seed,
                    batch,
                    parallel,
                };
                run_seed(opts).await
            }
            Command::Run {
                label,
                out,
                iterations,
                cold_iterations,
                cold_command,
                explain_dir,
                saves,
                only,
            } => {
                let opts = RunOptions {
                    label,
                    out,
                    iterations,
                    cold_iterations,
                    cold_command,
                    explain_dir,
                    saves,
                    only,
                };
                run_matrix(opts).await
            }
        }
    })
}

/// `DATABASE_URL`, refused unless its database is a benchmark one.
fn bench_url() -> Result<(String, String)> {
    let url =
        std::env::var("DATABASE_URL").context("DATABASE_URL must name the benchmark database")?;
    bench_database(&url)
}

/// The maintenance database URL and the database name of `url`, refused
/// unless that name starts with `field_bench`.
///
/// The name is the last path segment before the query string: the query
/// (`?application_name=/field_bench`, say) can hold a slash of its own, and
/// the name checked must be the one the connection opens.
fn bench_database(url: &str) -> Result<(String, String)> {
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (url, None),
    };
    let (base, name) = path
        .rsplit_once('/')
        .filter(|(base, name)| base.contains("://") && !base.ends_with('/') && !name.is_empty())
        .context("DATABASE_URL must include a database name")?;
    if !name.starts_with("field_bench") {
        bail!(
            "refusing to touch database `{name}`: the benchmark only writes to a database whose \
             name starts with `field_bench`"
        );
    }
    let admin = match query {
        Some(query) => format!("{base}/postgres?{query}"),
        None => format!("{base}/postgres"),
    };
    Ok((admin, name.to_string()))
}

async fn create_database() -> Result<()> {
    let (admin_url, name) = bench_url()?;
    let admin = PgPool::connect(&admin_url).await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&name)
            .fetch_one(&admin)
            .await?;
    if !exists {
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await?;
        eprintln!("created database {name}");
    }
    Ok(())
}

async fn drop_database() -> Result<()> {
    let (admin_url, name) = bench_url()?;
    let admin = PgPool::connect(&admin_url).await?;
    sqlx::query(AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
    )))
    .execute(&admin)
    .await?;
    eprintln!("dropped database {name}");
    Ok(())
}

async fn app_state() -> Result<AppState> {
    bench_url()?;
    let mut config = Config::from_env().context("failed to load configuration")?;
    config.database_max_connections = config.database_max_connections.max(40);
    AppState::new(&config)
        .await
        .context("failed to initialize application state")
}

async fn run_seed(opts: SeedOptions) -> Result<()> {
    opts.check()?;
    create_database().await?;
    let state = app_state().await?;
    let pool = state.db();
    let existing: Option<Value> =
        sqlx::query_scalar("SELECT value FROM site_config WHERE key = $1")
            .bind(seed::MARKER)
            .fetch_optional(pool)
            .await?;
    if let Some(existing) = existing {
        if existing == serde_json::to_value(opts)? {
            eprintln!("this database already holds exactly this fixture; nothing written");
            return Ok(());
        }
        bail!("this database holds another fixture ({existing}); drop it first");
    }
    let started = Instant::now();
    let phases = seed::seed(pool, opts).await?;
    for (name, secs) in &phases {
        eprintln!("{name:<24} {secs:>10.1} s");
    }
    eprintln!(
        "{:<24} {:>10.1} s",
        "total",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

struct RunOptions {
    label: String,
    out: PathBuf,
    iterations: usize,
    cold_iterations: usize,
    cold_command: Option<String>,
    explain_dir: Option<PathBuf>,
    saves: usize,
    only: Option<String>,
}

// ---------------------------------------------------------------------------
// Statement capture
// ---------------------------------------------------------------------------

tokio::task_local! {
    /// The statements run on this task, while a case is being captured.
    static CAPTURED: Arc<Mutex<Vec<String>>>;
}

/// Records the text of every `sqlx::query` event on a capturing task.
struct Capture;

#[derive(Default)]
struct StatementText {
    summary: Option<String>,
    full: Option<String>,
}

impl Visit for StatementText {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "summary" => self.summary = Some(value.to_owned()),
            "db.statement" if !value.trim().is_empty() => self.full = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let _ = CAPTURED.try_with(|captured| {
            let mut text = StatementText::default();
            event.record(&mut text);
            if let Ok(mut v) = captured.lock() {
                v.push(text.full.or(text.summary).unwrap_or_default());
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Latency {
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl Latency {
    fn of(samples: &mut [f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f64| {
            let idx = ((q * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1;
            samples[idx]
        };
        Self {
            n: samples.len(),
            p50_ms: at(0.50),
            p95_ms: at(0.95),
            p99_ms: at(0.99),
            max_ms: samples[samples.len() - 1],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Plan {
    /// The slowest item statement's plan nodes, outermost first, e.g.
    /// `Limit > Sort > Seq Scan on item`.
    pub shape: String,
    /// Whether any step of any item statement is a sequential scan on `item`.
    pub seq_scan_on_item: bool,
    pub shared_hit: i64,
    pub shared_read: i64,
    pub execution_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryResult {
    pub id: String,
    pub what: String,
    pub rows: usize,
    pub total: Option<u64>,
    pub statements: usize,
    pub warm: Latency,
    pub cold: Option<Latency>,
    pub plan: Plan,
    pub error: Option<String>,
    /// Timed runs that failed (a statement timeout), not in the latencies.
    #[serde(default)]
    pub warm_failures: usize,
    #[serde(default)]
    pub cold_failures: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RelationSize {
    pub name: String,
    pub table_bytes: i64,
    pub toast_bytes: i64,
    pub index_bytes: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Results {
    pub label: String,
    pub fixture: Value,
    pub postgres_version: String,
    pub settings: Vec<(String, String)>,
    pub item_counts: Vec<(String, i64, i64)>,
    pub revisions: i64,
    pub avg_fields_bytes: f64,
    pub sizes: Vec<RelationSize>,
    pub item_indexes: Vec<(String, i64)>,
    pub hot_set: Vec<(String, i64)>,
    pub hot_set_note: String,
    pub save: Latency,
    pub cold_note: String,
    pub queries: Vec<QueryResult>,
    pub seconds: f64,
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

async fn run_matrix(opts: RunOptions) -> Result<()> {
    let started = Instant::now();
    tracing_subscriber::registry()
        .with(
            Capture.with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("sqlx::query", tracing::Level::DEBUG),
            ),
        )
        .init();

    let state = app_state().await?;
    // The anonymous viewer a real request gets: the anonymous user row with
    // its role's permissions.
    let viewer = trovato_kernel::routes::helpers::anonymous_user_context(&state).await;
    let pool = state.db().clone();
    let fixture: Value = sqlx::query_scalar("SELECT value FROM site_config WHERE key = $1")
        .bind(seed::MARKER)
        .fetch_optional(&pool)
        .await?
        .context("no fixture in this database: run `seed` first")?;
    let seed_value = fixture.get("seed").and_then(Value::as_u64).unwrap_or(207);

    let mut results = Results {
        label: opts.label.clone(),
        fixture: fixture.clone(),
        ..Results::default()
    };
    describe_database(&pool, &mut results).await?;

    let only: Option<Vec<String>> = opts
        .only
        .as_ref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    let cases: Vec<Case> = matrix::cases(&pool, seed_value)
        .await?
        .into_iter()
        .filter(|c| only.as_ref().is_none_or(|o| o.iter().any(|x| x == c.id)))
        .collect();

    if let Some(dir) = &opts.explain_dir {
        std::fs::create_dir_all(dir)?;
    }

    // The hot set: start cold, run every query once, and see what stayed in
    // shared buffers.
    if let Some(cmd) = &opts.cold_command {
        go_cold(cmd, &pool).await?;
        let mut failed = Vec::new();
        for case in &cases {
            if run_case(&state, case, &viewer).await.is_err() {
                failed.push(case.id);
            }
        }
        results.hot_set = hot_set(&pool).await.unwrap_or_default();
        let shared_buffers: i64 =
            sqlx::query_scalar("SELECT pg_size_bytes(current_setting('shared_buffers'))")
                .fetch_one(&pool)
                .await?;
        // Every buffer in use, not only this database's top relations: any
        // page in a buffer is one the pass could not keep.
        let held: i64 = sqlx::query_scalar(
            "SELECT (count(*) * current_setting('block_size')::bigint)::bigint \
             FROM pg_buffercache WHERE relfilenode IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
        results.hot_set_note = hot_set_note(held, shared_buffers, &failed);
    } else {
        results.hot_set_note = "not measured: no --cold-command".into();
    }

    for case in &cases {
        eprintln!("{} {}", case.id, case.what);
        let mut result = QueryResult {
            id: case.id.to_string(),
            what: case.what.to_string(),
            ..QueryResult::default()
        };

        // Cold runs.
        if let Some(cmd) = &opts.cold_command {
            let mut samples = Vec::new();
            for _ in 0..opts.cold_iterations {
                go_cold(cmd, &pool).await?;
                match run_case(&state, case, &viewer).await {
                    Ok(outcome) => samples.push(outcome.elapsed.as_secs_f64() * 1000.0),
                    Err(e) => {
                        let timeout = is_timeout(&e);
                        result.error.get_or_insert_with(|| format!("{e:#}"));
                        result.cold_failures += 1;
                        if !timeout {
                            break;
                        }
                    }
                }
            }
            result.cold = Some(Latency::of(&mut samples));
        }

        // Warm runs: one untimed, then the timed ones. The timed runs happen
        // whether or not the untimed one succeeded, so a query that times out
        // is counted as timing out, not left with an empty sample.
        let warm_up = run_case(&state, case, &viewer).await;
        let warm_up_error = warm_up.as_ref().err().map(|e| format!("{e:#}"));
        let timed = timed_runs(opts.iterations, || run_case(&state, case, &viewer)).await;
        let mut samples = timed.samples;
        result.warm_failures = timed.failures;
        result.error = result.error.take().or(warm_up_error).or(timed.error);
        if let Some(outcome) = warm_up.ok().or(timed.first) {
            result.rows = outcome.rows;
            result.total = outcome.total;
            result.statements = outcome.statements.len();
            result.plan = explain(
                &pool,
                &outcome.statements,
                &case.bind_values(),
                opts.explain_dir
                    .as_ref()
                    .map(|d| d.join(format!("{}.txt", case.id))),
            )
            .await
            .unwrap_or_else(|e| Plan {
                shape: format!("EXPLAIN failed: {e:#}"),
                ..Plan::default()
            });
        }
        result.warm = Latency::of(&mut samples);
        eprintln!(
            "    rows {} total {:?} statements {} warm p50 {:.1} p95 {:.1} p99 {:.1} ms{} | {}{}",
            result.rows,
            result.total,
            result.statements,
            result.warm.p50_ms,
            result.warm.p95_ms,
            result.warm.p99_ms,
            result
                .cold
                .as_ref()
                .map(|c| format!(" cold p50 {:.1} p95 {:.1}", c.p50_ms, c.p95_ms))
                .unwrap_or_default(),
            result.plan.shape,
            result
                .error
                .as_ref()
                .map(|e| format!(" ERROR {e}"))
                .unwrap_or_default()
        );
        results.queries.push(result);
    }
    results.cold_note = match &opts.cold_command {
        Some(cmd) => format!(
            "cold: each run after `{cmd}` (Postgres restarted, so shared buffers are empty, and \
             the Linux page cache of the Docker VM dropped)"
        ),
        None => "warm only: no --cold-command was given".into(),
    };

    if opts.saves > 0 {
        results.save = measure_saves(&state, seed_value, opts.saves).await?;
        eprintln!(
            "save p50 {:.1} p99 {:.1} ms",
            results.save.p50_ms, results.save.p99_ms
        );
    }
    results.seconds = started.elapsed().as_secs_f64();
    std::fs::write(&opts.out, serde_json::to_string_pretty(&results)?)?;
    eprintln!("wrote {}", opts.out.display());
    Ok(())
}

struct Outcome {
    elapsed: Duration,
    rows: usize,
    total: Option<u64>,
    statements: Vec<String>,
}

/// What `iterations` timed calls of one query gave.
struct Timed {
    /// Milliseconds of each call that succeeded.
    samples: Vec<f64>,
    /// Calls that failed. A failure is slower than any success (a timeout)
    /// or no answer at all, so the report counts it above every sample.
    failures: usize,
    /// The first failure's message.
    error: Option<String>,
    /// The first call that succeeded, for its rows, statements and plan.
    first: Option<Outcome>,
}

/// Call `run` `iterations` times and keep every result. A timeout is retried
/// (the next call may finish); any other error is the same on every call, so
/// it is counted once and the loop stops.
async fn timed_runs<F, Fut>(iterations: usize, mut run: F) -> Timed
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Outcome>>,
{
    let mut timed = Timed {
        samples: Vec::with_capacity(iterations),
        failures: 0,
        error: None,
        first: None,
    };
    for _ in 0..iterations {
        match run().await {
            Ok(outcome) => {
                timed.samples.push(outcome.elapsed.as_secs_f64() * 1000.0);
                timed.first.get_or_insert(outcome);
            }
            Err(e) => {
                let timeout = is_timeout(&e);
                timed.error.get_or_insert_with(|| format!("{e:#}"));
                timed.failures += 1;
                if !timeout {
                    break;
                }
            }
        }
    }
    timed
}

async fn run_case(state: &AppState, case: &Case, viewer: &UserContext) -> Result<Outcome> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let gather = state.gather().clone();
    let started = Instant::now();
    let (rows, total) = CAPTURED
        .scope(captured.clone(), async {
            match &case.run {
                Run::Gather {
                    definition,
                    page,
                    per_page,
                    exposed,
                    url_args,
                } => {
                    let display = trovato_kernel::gather::QueryDisplay {
                        items_per_page: Some(*per_page),
                        ..Default::default()
                    };
                    let context = QueryContext {
                        viewer: Some(viewer.clone()),
                        url_args: url_args.clone(),
                        ..QueryContext::default()
                    };
                    let result = gather
                        .execute_definition(
                            definition,
                            &display,
                            *page,
                            exposed.clone(),
                            LIVE_STAGE_ID,
                            &context,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>((result.items.len(), Some(result.total)))
                }
                Run::Facet {
                    source_field,
                    all_exposed,
                    exclude_field,
                    active,
                } => {
                    let values = gather
                        .fetch_faceted_distinct_values(
                            source_field,
                            MAIN_TYPE,
                            all_exposed,
                            exclude_field,
                            active,
                        )
                        .await?;
                    Ok((values.len(), None))
                }
            }
        })
        .await?;
    let elapsed = started.elapsed();
    let statements = captured.lock().map(|v| v.clone()).unwrap_or_default();
    Ok(Outcome {
        elapsed,
        rows,
        total,
        statements,
    })
}

/// Run the cold command, then wait until Postgres answers again.
async fn go_cold(cmd: &str, pool: &PgPool) -> Result<()> {
    let status = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .status()
        .await?;
    if !status.success() {
        bail!("cold command failed: {status}");
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if sqlx::query("SELECT 1").execute(pool).await.is_ok() {
            // A second check: the first may have raced the shutdown.
            tokio::time::sleep(Duration::from_millis(200)).await;
            if sqlx::query("SELECT 1").execute(pool).await.is_ok() {
                return Ok(());
            }
        }
        if Instant::now() > deadline {
            bail!("Postgres did not come back within two minutes");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// `EXPLAIN (ANALYZE, BUFFERS)` every SELECT statement the case ran that
/// reads `item`; the plan reported is the slowest one's.
async fn explain(
    pool: &PgPool,
    statements: &[String],
    binds: &[String],
    out: Option<PathBuf>,
) -> Result<Plan> {
    let mut text_out = String::new();
    let mut best: Option<Plan> = None;
    let mut seq_any = false;
    for sql in statements {
        let trimmed = sql.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("select") || lower.starts_with("with"))
            || !(lower.contains("from \"item\"") || lower.contains("from item"))
        {
            continue;
        }
        // A statement with placeholders is explained as a prepared statement
        // executed with the case's values, on one connection.
        let mut conn = pool.acquire().await?;
        let target = if trimmed.contains("$1") {
            sqlx::query(AssertSqlSafe(format!(
                "PREPARE field_bench_stmt AS {trimmed}"
            )))
            .execute(&mut *conn)
            .await?;
            let args: Vec<String> = binds
                .iter()
                .map(|b| format!("'{}'", b.replace('\'', "''")))
                .collect();
            format!("EXECUTE field_bench_stmt({})", args.join(", "))
        } else {
            trimmed.to_string()
        };
        let json_plan: Value = sqlx::query_scalar(AssertSqlSafe(format!(
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {target}"
        )))
        .fetch_one(&mut *conn)
        .await?;
        let text: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
            "EXPLAIN (ANALYZE, BUFFERS) {target}"
        )))
        .fetch_all(&mut *conn)
        .await?;
        if target != trimmed {
            sqlx::query("DEALLOCATE field_bench_stmt")
                .execute(&mut *conn)
                .await?;
        }
        text_out.push_str(&format!(
            "-- statement\n{trimmed}\n\n{}\n\n",
            text.join("\n")
        ));
        let root = &json_plan[0];
        let plan = &root["Plan"];
        let mut shape = Vec::new();
        let mut seq = false;
        walk(plan, &mut shape, &mut seq);
        seq_any |= seq;
        let candidate = Plan {
            shape: shape.join(" > "),
            seq_scan_on_item: seq,
            shared_hit: plan["Shared Hit Blocks"].as_i64().unwrap_or(0),
            shared_read: plan["Shared Read Blocks"].as_i64().unwrap_or(0),
            execution_ms: root["Execution Time"].as_f64().unwrap_or(0.0),
        };
        if best
            .as_ref()
            .is_none_or(|b| candidate.execution_ms > b.execution_ms)
        {
            best = Some(candidate);
        }
    }
    if let Some(path) = out {
        std::fs::write(path, text_out)?;
    }
    let mut plan = best.unwrap_or_default();
    plan.seq_scan_on_item = seq_any;
    Ok(plan)
}

/// Collect the plan's step names, depth first, keeping only the steps that
/// read something or decide the order.
fn walk(step: &Value, shape: &mut Vec<String>, seq: &mut bool) {
    let kind = step["Node Type"].as_str().unwrap_or("?");
    let relation = step["Relation Name"].as_str();
    let index = step["Index Name"].as_str();
    if kind == "Seq Scan" && relation == Some("item") {
        *seq = true;
    }
    let interesting = matches!(
        kind,
        "Seq Scan"
            | "Index Scan"
            | "Index Only Scan"
            | "Bitmap Index Scan"
            | "Bitmap Heap Scan"
            | "Sort"
            | "Incremental Sort"
            | "BitmapOr"
            | "BitmapAnd"
            | "Gather"
            | "Gather Merge"
            | "Hash Join"
            | "Nested Loop"
            | "Function Scan"
    );
    if interesting {
        let mut label = kind.to_string();
        if let Some(i) = index {
            label.push_str(&format!(" {i}"));
        } else if let Some(r) = relation {
            label.push_str(&format!(" on {r}"));
        }
        shape.push(label);
    }
    if let Some(children) = step["Plans"].as_array() {
        for child in children {
            walk(child, shape, seq);
        }
    }
}

async fn describe_database(pool: &PgPool, results: &mut Results) -> Result<()> {
    results.postgres_version = sqlx::query_scalar("SELECT version()")
        .fetch_one(pool)
        .await?;
    results.settings = sqlx::query_as(
        "SELECT name, setting || COALESCE(' ' || unit, '') FROM pg_settings WHERE name IN \
         ('shared_buffers','effective_cache_size','work_mem','maintenance_work_mem', \
          'random_page_cost','effective_io_concurrency','max_parallel_workers_per_gather', \
          'jit','max_connections','default_statistics_target') ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    results.item_counts = sqlx::query_as(
        "SELECT type, count(*), count(*) FILTER (WHERE status = 1) FROM item GROUP BY type ORDER BY type",
    )
    .fetch_all(pool)
    .await?;
    results.revisions = sqlx::query_scalar("SELECT count(*) FROM item_revision")
        .fetch_one(pool)
        .await?;
    results.avg_fields_bytes = sqlx::query_scalar(
        "SELECT avg(pg_column_size(fields))::float8 FROM (SELECT fields FROM item WHERE type = $1 LIMIT 20000) s",
    )
    .bind(MAIN_TYPE)
    .fetch_one(pool)
    .await?;
    let sizes: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT c.relname::text, pg_relation_size(c.oid), \
                COALESCE(pg_total_relation_size(NULLIF(c.reltoastrelid, 0)), 0), \
                pg_indexes_size(c.oid), pg_total_relation_size(c.oid) \
         FROM pg_class c WHERE c.relname IN ('item', 'item_revision') ORDER BY 1",
    )
    .fetch_all(pool)
    .await?;
    results.sizes = sizes
        .into_iter()
        .map(|(name, t, toast, i, total)| RelationSize {
            name,
            table_bytes: t,
            toast_bytes: toast,
            index_bytes: i,
            total_bytes: total,
        })
        .collect();
    results.item_indexes = sqlx::query_as(
        "SELECT i.indexrelid::regclass::text, pg_relation_size(i.indexrelid) \
         FROM pg_index i WHERE i.indrelid = 'item'::regclass ORDER BY 2 DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(())
}

/// What the hot set snapshot can and cannot say.
///
/// It is what one pass of the matrix from cold left in shared buffers. When
/// the buffers came out (nearly) full, pages read early were evicted by
/// pages read later and are missing from it, so the working set is at least
/// `shared_buffers`, by an amount the snapshot cannot show. Queries that
/// failed during the pass stopped early and read less than they would have.
fn hot_set_note(held: i64, shared_buffers: i64, failed: &[&str]) -> String {
    let mut note =
        "shared buffers holding each relation after one pass of the matrix from cold".to_string();
    if shared_buffers > 0 && held * 100 >= shared_buffers * 95 {
        note.push_str(&format!(
            "; the {} MB of shared buffers were full, so this is a lower bound: pages read \
             earlier in the pass were evicted, and the working set is larger by an amount this \
             cannot measure",
            shared_buffers / 1_048_576
        ));
    }
    if !failed.is_empty() {
        note.push_str(&format!(
            "; {} failed during the pass and read less than a full run would",
            failed.join(", ")
        ));
    }
    note
}

async fn hot_set(pool: &PgPool) -> Result<Vec<(String, i64)>> {
    sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_buffercache")
        .execute(pool)
        .await?;
    Ok(sqlx::query_as(
        "SELECT c.relname::text, (count(*) * 8192)::bigint FROM pg_buffercache b \
         JOIN pg_class c ON b.relfilenode = pg_relation_filenode(c.oid) \
         WHERE b.reldatabase = (SELECT oid FROM pg_database WHERE datname = current_database()) \
         GROUP BY c.relname ORDER BY 2 DESC LIMIT 20",
    )
    .fetch_all(pool)
    .await?)
}

/// Time `n` saves of items of the 200 field type through `ItemService::update`.
async fn measure_saves(state: &AppState, seed: u64, n: usize) -> Result<Latency> {
    let pool = state.db();
    let admin_sql = format!("SELECT {}", seed::id_sql(seed, "user", "0"));
    let admin: uuid::Uuid = sqlx::query_scalar(AssertSqlSafe(admin_sql))
        .fetch_one(pool)
        .await?;
    let user = UserContext::administrator(admin, Vec::new());
    let ids: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM item WHERE type = $1 ORDER BY id LIMIT $2")
            .bind(MAIN_TYPE)
            .bind(i64::try_from(n)?)
            .fetch_all(pool)
            .await?;
    // The saves change the fixture: three fields of each item, and a revision
    // each. Put it back afterwards, whether or not every save succeeded, so
    // the next run on this database queries the data the seed wrote.
    let snapshot = ItemSnapshot::take(pool, &ids).await?;
    let timed = time_saves(state, &user, &ids).await;
    snapshot.restore(state).await?;
    timed
}

/// The item rows and revision ids of some items, to restore after saving
/// them.
struct ItemSnapshot {
    ids: Vec<uuid::Uuid>,
    /// The rows as JSON text, not a parsed value: parsing would turn the
    /// seed's `773.00` into `773.0`, and the restore must write back exactly
    /// what was there.
    rows: String,
    revisions: Vec<uuid::Uuid>,
}

impl ItemSnapshot {
    async fn take(pool: &PgPool, ids: &[uuid::Uuid]) -> Result<Self> {
        let rows: String = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(to_jsonb(i)), '[]')::text FROM item i WHERE i.id = ANY($1)",
        )
        .bind(ids)
        .fetch_one(pool)
        .await?;
        let revisions: Vec<uuid::Uuid> =
            sqlx::query_scalar("SELECT id FROM item_revision WHERE item_id = ANY($1)")
                .bind(ids)
                .fetch_all(pool)
                .await?;
        Ok(Self {
            ids: ids.to_vec(),
            rows,
            revisions,
        })
    }

    /// Write every column of the item rows back as they were, delete the
    /// revisions made since, and forget the cached copies.
    async fn restore(&self, state: &AppState) -> Result<()> {
        let pool = state.db();
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT quote_ident(attname) FROM pg_attribute \
             WHERE attrelid = 'item'::regclass AND attnum > 0 AND NOT attisdropped \
               AND attgenerated = '' AND attname <> 'id' ORDER BY attnum",
        )
        .fetch_all(pool)
        .await?;
        let targets = columns.join(", ");
        let sources = columns
            .iter()
            .map(|c| format!("s.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut tx = pool.begin().await?;
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE item i SET ({targets}) = ROW({sources}) \
             FROM jsonb_populate_recordset(NULL::item, $1::jsonb) s WHERE i.id = s.id"
        )))
        .bind(&self.rows)
        .execute(&mut *tx)
        .await
        .context("restoring the saved items")?;
        sqlx::query("DELETE FROM item_revision WHERE item_id = ANY($1) AND NOT (id = ANY($2))")
            .bind(&self.ids)
            .bind(&self.revisions)
            .execute(&mut *tx)
            .await
            .context("deleting the revisions the saves made")?;
        tx.commit().await?;
        for id in &self.ids {
            state.items().invalidate(*id);
        }
        Ok(())
    }
}

/// Save each of `ids` once with three fields changed, and time each save.
async fn time_saves(state: &AppState, user: &UserContext, ids: &[uuid::Uuid]) -> Result<Latency> {
    let mut samples = Vec::with_capacity(ids.len());
    for (i, id) in ids.iter().enumerate() {
        let Some(item) = state.items().load(*id).await? else {
            continue;
        };
        let mut fields = item.fields.clone();
        if let Some(obj) = fields.as_object_mut() {
            obj.insert(
                "ft_color".into(),
                Value::String(format!("col{}", 1 + i % 12)),
            );
            obj.insert("fi_price".into(), Value::from(1000 + i as i64));
            obj.insert("fdt_published".into(), Value::String("2025-06-01".into()));
        }
        let input = trovato_kernel::models::UpdateItem {
            title: None,
            status: None,
            promote: None,
            sticky: None,
            fields: Some(fields),
            log: Some("field-bench save".into()),
            stage_id: None,
            author_id: None,
            created: None,
        };
        let started = Instant::now();
        state
            .items()
            .update(*id, input, user)
            .await
            .with_context(|| format!("saving {id}"))?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(Latency::of(&mut samples))
}

/// Whether `e` is Postgres cancelling a statement at the gather's timeout.
fn is_timeout(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("statement timeout")
}

// Tests are allowed to use unwrap/expect freely.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_database_checked_is_the_one_connected_to() {
        let (admin, name) =
            bench_database("postgres://u:p@127.0.0.1:5432/field_bench?sslmode=disable").unwrap();
        assert_eq!(name, "field_bench");
        assert_eq!(
            admin,
            "postgres://u:p@127.0.0.1:5432/postgres?sslmode=disable"
        );

        // A slash in the query string is not the database name.
        let err = bench_database("postgres://u:p@127.0.0.1/trovato?application_name=/field_bench")
            .unwrap_err();
        assert!(format!("{err:#}").contains("`trovato`"), "{err:#}");

        for url in [
            "postgres://u:p@127.0.0.1/trovato",
            "postgres://u:p@127.0.0.1",
            "postgres://u:p@127.0.0.1/",
        ] {
            assert!(bench_database(url).is_err(), "{url} was accepted");
        }
    }

    #[test]
    fn a_batch_of_no_rows_is_refused() {
        for batch in ["0", "-5"] {
            assert!(
                Cli::try_parse_from(["field-bench", "seed", "--batch", batch]).is_err(),
                "--batch {batch} was accepted"
            );
        }
        assert!(Cli::try_parse_from(["field-bench", "seed", "--batch", "1"]).is_ok());

        let opts = SeedOptions {
            main_items: 10,
            other_items: 10,
            seed: 207,
            batch: 0,
            parallel: 1,
        };
        assert!(opts.check().is_err());
        assert!(SeedOptions { batch: -1, ..opts }.check().is_err());
        assert!(
            SeedOptions {
                main_items: -1,
                batch: 1,
                ..opts
            }
            .check()
            .is_err()
        );
        assert!(SeedOptions { batch: 1, ..opts }.check().is_ok());
    }

    fn outcome(ms: u64) -> Outcome {
        Outcome {
            elapsed: Duration::from_millis(ms),
            rows: 10,
            total: Some(10),
            statements: vec!["SELECT 1".into()],
        }
    }

    #[tokio::test]
    async fn every_timed_run_is_attempted_and_each_timeout_counted() {
        // A query that always times out: all 30 runs are attempted and
        // counted, none is a sample.
        let timed = timed_runs(30, || async {
            Err(anyhow::anyhow!(
                "canceling statement due to statement timeout"
            ))
        })
        .await;
        assert_eq!(timed.failures, 30);
        assert!(timed.samples.is_empty());
        assert!(timed.first.is_none());
        assert!(timed.error.unwrap().contains("statement timeout"));

        // Two timeouts among 30: 28 samples and 2 failures, and the first
        // success kept for its statements.
        let mut call = 0;
        let timed = timed_runs(30, || {
            call += 1;
            let n = call;
            async move {
                if n == 3 || n == 17 {
                    Err(anyhow::anyhow!(
                        "canceling statement due to statement timeout"
                    ))
                } else {
                    Ok(outcome(40))
                }
            }
        })
        .await;
        assert_eq!((timed.samples.len(), timed.failures), (28, 2));
        assert_eq!(timed.first.unwrap().statements.len(), 1);

        // Any other error is the same every time: counted once, then stop.
        let mut calls = 0;
        let timed = timed_runs(30, || {
            calls += 1;
            async { Err(anyhow::anyhow!("operator does not exist: text >= integer")) }
        })
        .await;
        assert_eq!((calls, timed.failures), (1, 1));
    }

    #[test]
    fn a_full_buffer_cache_is_reported_as_a_lower_bound() {
        let gb = 1_073_741_824;
        let note = hot_set_note(4 * gb - 100 * 1_048_576, 4 * gb, &[]);
        assert!(note.contains("lower bound"), "{note}");
        let note = hot_set_note(gb, 4 * gb, &["Q03", "Q17"]);
        assert!(!note.contains("lower bound"), "{note}");
        assert!(note.contains("Q03, Q17 failed"), "{note}");
    }

    /// Every item row and revision id, as one string to compare.
    async fn fingerprint(pool: &PgPool) -> (String, String) {
        sqlx::query_as(
            "SELECT (SELECT md5(string_agg(to_jsonb(i)::text, '|' ORDER BY i.id)) FROM item i), \
                    (SELECT md5(string_agg(r.id::text, '|' ORDER BY r.id)) FROM item_revision r)",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn timing_saves_leaves_the_fixture_as_seeded() {
        let db = trovato_test_utils::ScratchDb::create("field_bench_saves").await;
        trovato_kernel::db::run_migrations(db.pool())
            .await
            .expect("migrate");
        let opts = SeedOptions {
            main_items: 40,
            other_items: 8,
            seed: 207,
            batch: 10,
            parallel: 2,
        };
        seed::seed(db.pool(), opts).await.expect("seed");

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let plugins = std::env::temp_dir().join(format!("field-bench-saves-{}", db.name()));
        std::fs::create_dir_all(&plugins).unwrap();
        let mut config = Config::from_env().expect("config");
        config.database_url = db.url();
        config.plugins_dirs = vec![plugins.clone()];
        config.templates_dirs = vec![root.join("templates")];
        config.themes_dirs = vec![root.join("themes")];
        let state = AppState::new(&config).await.expect("app state");

        let before = fingerprint(db.pool()).await;
        let saves = measure_saves(&state, opts.seed, 5).await.expect("saves");
        assert_eq!(saves.n, 5, "every save is timed");
        assert_eq!(
            fingerprint(db.pool()).await,
            before,
            "the saves changed the fixture the next run queries"
        );
        std::fs::remove_dir_all(&plugins).ok();
    }
}
