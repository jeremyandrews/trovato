//! Trovato Kernel
//!
//! HTTP server, plugin runtime, and core services.

// Many models, services, and types expose APIs that are used by plugins, tests,
// or routes not yet wired into the binary. Suppress dead_code and unused re-exports
// for the binary target; the lib target (lib.rs) maintains stricter checking.
#![allow(dead_code, unused_imports)]

mod assistant;
mod audit;
mod batch;
mod cache;
mod circuit_breaker;
mod client_addr;
mod config;
mod config_storage;
mod content;
mod cron;
mod db;
mod edge_cache;
mod error;
mod file;
mod form;
mod gather;
mod generate;
mod host;
mod i18n;
mod import;
mod lockout;
mod menu;
mod metrics;
mod middleware;
mod migrate;
mod migration_rule;
mod models;
mod net_policy;
mod pagination;
mod password_prompt;
mod permissions;
mod plugin;
mod recovery;
mod redis_conn;
mod routes;
mod search;
mod services;
mod session;
mod stage;
mod state;
mod system_log;
mod tap;
mod telemetry;
mod theme;
mod timer;
mod update_status;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use clap::{Parser, Subcommand};
use tower_http::cors::CorsLayer;
use tower_sessions::Session;
use tower_sessions::cookie::SameSite;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, Layer as _, layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::cache::generation::CacheKind;
use crate::config::Config;
use crate::state::AppState;

#[derive(Parser)]
// `version` reads CARGO_PKG_VERSION, which is the one project version from the
// workspace Cargo.toml, so `trovato --version` cannot drift from the release.
#[command(name = "trovato", about = "Trovato", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP server (default).
    Serve,
    /// Plugin management commands.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Configuration export/import commands.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// User management commands.
    User {
        #[command(subcommand)]
        action: UserAction,
    },
    /// Cache commands.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Static search index commands.
    Search {
        #[command(subcommand)]
        action: SearchAction,
    },
    /// Item and revision management commands.
    Item {
        #[command(subcommand)]
        action: ItemAction,
    },
    /// Import content from another site, keeping each record's id and dates.
    ///
    /// Reads newline delimited JSON, one record per line
    /// (`docs/design/import.md`), and writes the whole file in one
    /// transaction, each record checked as the forms check it. A record
    /// identical to what is stored changes nothing, so a rerun reports 0
    /// changes. Any record that fails is reported with its line number, and
    /// nothing is written. Exits nonzero on any failure.
    Import {
        /// The file to import (`-` reads stdin).
        file: std::path::PathBuf,
        /// Check every record and report what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Apply pending kernel and plugin migrations, under one lock.
    ///
    /// Safe to run from several places at once: a second run waits for the
    /// first, then finds nothing to do. Exits nonzero on any failure.
    Migrate {
        /// List applied, pending and unknown (newer) migrations; change nothing.
        #[arg(long, conflicts_with = "dry_run")]
        status: bool,
        /// List what would be applied; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Seconds a migration statement may wait for a table lock before it
        /// fails (and the migration rolls back); 0 waits forever.
        #[arg(long, default_value_t = 10)]
        lock_timeout: u64,
    },
    /// Write a deterministic generated site of the given size into
    /// DATABASE_URL, for performance tests and benchmarks (BG14). The same
    /// options always write the same rows.
    Generate {
        /// Blog posts to write.
        #[arg(long)]
        items: u64,
        /// Comments in total, spread over the newest posts first
        /// (default: one per post).
        #[arg(long)]
        comments: Option<u64>,
        /// Terms in the tags vocabulary.
        #[arg(long, default_value_t = 500)]
        tags: u64,
        /// Members (an administrator, gen_admin, is extra).
        #[arg(long, default_value_t = 1000)]
        users: u64,
        /// The seed every value is derived from.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Rows per statement.
        #[arg(long, default_value_t = 20_000)]
        batch: u64,
        /// Statements in flight at once.
        #[arg(long, default_value_t = 16)]
        parallel: usize,
        /// The password of every generated account, `gen_admin` included.
        /// Required: a known default would be a working administrator login
        /// on any generated site that can be reached.
        #[arg(long)]
        admin_password: String,
    },
    /// Uploaded file commands.
    Files {
        #[command(subcommand)]
        action: FilesAction,
    },
}

#[derive(Subcommand)]
enum FilesAction {
    /// Copy every file under the uploads directory into the S3 bucket the
    /// `FILE_STORAGE=s3` and `S3_*` settings name, keeping each file's key.
    ///
    /// Each copy is read back and checked by SHA-256. A file the bucket
    /// already holds with the same SHA-256 is skipped, so an interrupted or
    /// partly failed run is finished by running it again. Nothing is deleted.
    /// Exits nonzero if any file was not copied.
    MigrateToS3 {
        /// The directory to copy from (default: `UPLOADS_DIR`).
        #[arg(long)]
        from: Option<std::path::PathBuf>,
        /// Files copied at once.
        #[arg(long, default_value_t = 16)]
        parallel: usize,
    },
}

/// Run a `trovato files` command. Needs no database: only the settings.
async fn run_files_command(action: FilesAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;
    match action {
        FilesAction::MigrateToS3 { from, parallel } => {
            if !matches!(config.file_storage, crate::file::StorageConfig::S3(_)) {
                anyhow::bail!(
                    "the destination is the S3 bucket: set FILE_STORAGE=s3 and S3_BUCKET \
                     (and S3_REGION, or S3_ENDPOINT for a non-AWS store) first"
                );
            }
            let from = from.unwrap_or_else(|| config.uploads_dir.clone());
            let dest = crate::file::build_storage(
                &config.file_storage,
                &config.uploads_dir,
                &config.files_url,
            )?;
            dest.check_usable().await?;
            let report =
                crate::file::migrate::copy_dir_to_storage(&from, dest.clone(), parallel).await?;
            for (key, reason) in &report.failed {
                eprintln!("failed: {key}: {reason}");
            }
            println!(
                "{} files under {}: {} copied, {} already in {}, {} failed",
                report.total,
                from.display(),
                report.copied,
                report.already_there,
                dest.describe(),
                report.failed.len()
            );
            if !report.failed.is_empty() {
                anyhow::bail!(
                    "{} files were not copied; fix the cause and run this again",
                    report.failed.len()
                );
            }
            Ok(())
        }
    }
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Export all config to YAML files.
    Export {
        /// Output directory.
        #[arg(default_value = "config")]
        dir: String,
        /// Remove stale .yml config files after exporting.
        #[arg(long)]
        clean: bool,
    },
    /// Import config from YAML files.
    Import {
        /// Input directory.
        #[arg(default_value = "config")]
        dir: String,
        /// Validate files without writing to the database.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum SearchAction {
    /// Ask for a full rebuild of the static search index at the next cron
    /// run, instead of the incremental updates that follow each change.
    Rebuild,
}

#[derive(Subcommand)]
enum CacheAction {
    /// Tell every running server to drop its in-memory caches, so a change
    /// written straight to the database is served on the next request.
    Clear {
        /// Clear only this kind (repeatable); every kind when omitted.
        #[arg(long = "kind", value_name = "KIND", value_parser = parse_cache_kind)]
        kinds: Vec<CacheKind>,
    },
}

/// A `--kind` value, by the name `cache:gen` uses for it.
fn parse_cache_kind(name: &str) -> std::result::Result<CacheKind, String> {
    CacheKind::from_name(name).ok_or_else(|| {
        let names: Vec<&str> = CacheKind::ALL.iter().map(|k| k.as_str()).collect();
        format!("unknown cache kind; one of: {}", names.join(", "))
    })
}

#[derive(Subcommand)]
enum UserAction {
    /// Create an account, with the checks the admin "Add user" form makes.
    ///
    /// The password is read from stdin, never from the command line. Exits 1,
    /// creating nothing, when the name or email is taken or missing.
    Create {
        /// Username.
        username: String,
        /// Email address (required, as on the admin form).
        // An `Option` rather than a required argument so that a missing
        // address fails as an empty one does, with the form's message and
        // exit code 1, not clap's usage error and exit code 2.
        #[arg(long, value_name = "ADDRESS")]
        mail: Option<String>,
        /// Give the account this role (repeatable).
        #[arg(long = "role", value_name = "ROLE")]
        roles: Vec<String>,
        /// Create the account blocked.
        #[arg(long)]
        blocked: bool,
        /// Create the account with no password: it cannot sign in until
        /// `trovato user reset-password` (or a password reset) sets one.
        #[arg(long)]
        no_password: bool,
    },
    /// Reset a user's password.
    ResetPassword {
        /// Username.
        username: String,
        /// New password (min 12 characters). If omitted, reads from stdin.
        password: Option<String>,
    },
    /// Give a user a role.
    RoleAdd {
        /// Username.
        username: String,
        /// Role name.
        role: String,
    },
    /// Take a role away from a user.
    RoleRemove {
        /// Username.
        username: String,
        /// Role name.
        role: String,
    },
    /// List the roles a user holds.
    Roles {
        /// Username.
        username: String,
    },
}

#[derive(Subcommand)]
enum ItemAction {
    /// Delete a past revision of an item: what an operator reaches for a
    /// revision nothing in the UI should offer deleting through a browser (a
    /// test edit, a private draft caught in history). Refused the same way
    /// the route is: the current revision (exit nonzero), one that does not
    /// belong to the item or does not exist (exit nonzero). Runs as an
    /// administrator, the same bypass `user role-add` and the rest of this
    /// CLI run as: whoever can reach the database already has it.
    RevisionDelete {
        /// The item's id.
        item: Uuid,
        /// The revision's id.
        revision: Uuid,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    /// Scaffold a new plugin in the plugins/ directory.
    New {
        /// Plugin machine name (snake_case, e.g. my_plugin).
        name: String,
    },
    /// List discovered plugins and their status.
    List,
    /// Install a plugin (run migrations, set enabled).
    Install {
        /// Plugin machine name.
        name: String,
    },
    /// Run pending migrations for a plugin (or all plugins).
    Migrate {
        /// Plugin name. If omitted, runs migrations for all plugins.
        name: Option<String>,
    },
    /// Enable a plugin.
    Enable {
        /// Plugin machine name.
        name: String,
    },
    /// Disable a plugin.
    Disable {
        /// Plugin machine name.
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing. The site log's rows wait in a channel until the
    // server has a database pool to write them to.
    let log_receiver = init_tracing()?;

    let cli = Cli::parse();

    let result = match cli.command {
        None | Some(Commands::Serve) => run_server(log_receiver).await,
        Some(Commands::Plugin { action }) => run_plugin_command(action).await,
        Some(Commands::Config { action }) => run_config_command(action).await,
        Some(Commands::User { action }) => run_user_command(action).await,
        Some(Commands::Cache { action }) => run_cache_command(action).await,
        Some(Commands::Search { action }) => run_search_command(action).await,
        Some(Commands::Item { action }) => run_item_command(action).await,
        Some(Commands::Import { file, dry_run }) => run_import_command(&file, dry_run).await,
        Some(Commands::Migrate {
            status,
            dry_run,
            lock_timeout,
        }) => run_migrate_command(status, dry_run, lock_timeout).await,
        Some(Commands::Generate {
            items,
            comments,
            tags,
            users,
            seed,
            batch,
            parallel,
            admin_password,
        }) => {
            let options = generate::GenerateOptions {
                items,
                comments: comments.unwrap_or(items),
                tags,
                users,
                seed,
                batch,
                parallel,
            };
            run_generate_command(options, &admin_password).await
        }
        Some(Commands::Files { action }) => run_files_command(action).await,
    };
    // The spans still waiting in the export batch are sent before the
    // process ends, for at most a few seconds: a collector that is down must
    // not hold the shutdown past its budget.
    if telemetry::export::active() {
        // The SDK enforces the limit itself: the runtime waits for a
        // blocking task to finish before the process exits, so a timeout
        // around this one would not shorten anything.
        let _ =
            tokio::task::spawn_blocking(|| telemetry::export::shutdown(EXPORT_FLUSH_LIMIT)).await;
    }
    result
}

/// `trovato generate`: write a deterministic site (`generate.rs`).
async fn run_generate_command(
    options: generate::GenerateOptions,
    admin_password: &str,
) -> Result<()> {
    let mut config = Config::from_env().context("failed to load configuration")?;
    config.database_max_connections = config.database_max_connections.max(
        u32::try_from(options.parallel)
            .unwrap_or(u32::MAX)
            .saturating_add(4),
    );
    // The full server state: migrations, plugin discovery (which installs and
    // enables trovato_blog on a fresh database) and its content types.
    let state = AppState::new(&config)
        .await
        .context("failed to initialize application state")?;
    if generate::repair_after_crash(state.db()).await? {
        warn!("re-enabled the comment triggers a crashed generate run left off");
    }
    println!("Generating {options:?}");
    let report = generate::generate(state.db(), &options, admin_password, |line| {
        println!("  {line}");
    })
    .await?;
    if report.already_present {
        println!("This database already holds exactly this generated site; nothing written.");
        return Ok(());
    }
    for phase in &report.phases {
        println!(
            "{:<34} {:>12} rows {:>10.1} s",
            phase.name, phase.rows, phase.seconds
        );
    }
    println!(
        "{:<34} {:>12}      {:>10.1} s",
        "total",
        "",
        report.total_seconds()
    );
    // Bulk rows written straight to the database: the static search index is
    // rebuilt in full by the next cron run rather than item by item.
    crate::cron::request_search_index_rebuild(state.db()).await;
    println!("{}", bump_from_cli(&config, &CacheKind::ALL).await);
    Ok(())
}

/// `trovato migrate`: the migration step of an upgrade, run before new
/// servers start (`docs/design/upgrades-and-probes.md` §1).
async fn run_migrate_command(status: bool, dry_run: bool, lock_timeout: u64) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;

    if status || dry_run {
        // No lock and no writes, so a report never waits on, or blocks, a run.
        let report = migrate::read_report(
            &config.database_url,
            db::migrator(),
            &config.plugins_dirs,
            &config.disabled_plugins,
        )
        .await?;
        if status {
            print!("{}", report.render_status());
        } else {
            print!("{}", report.render_dry_run());
        }
        return Ok(());
    }

    let mut lock = migrate::MigrationLock::connect(&config.database_url).await?;
    lock.set_lock_timeout(std::time::Duration::from_secs(lock_timeout))
        .await?;
    let applied = migrate::run(
        &mut lock,
        db::migrator(),
        &config.plugins_dirs,
        &config.disabled_plugins,
    )
    .await?;
    lock.release().await?;
    println!(
        "Applied {} kernel and {} plugin migration(s).",
        applied.kernel, applied.plugins
    );
    Ok(())
}

/// What a subcommand other than `serve` and `migrate` does about the schema
/// when it starts: migrate it (`MIGRATE_ON_START`, the default), or refuse to
/// run on a schema that is behind this binary.
async fn prepare_schema_for_cli(pool: &sqlx::PgPool, config: &Config) -> Result<()> {
    if config.migrate_on_start {
        return db::run_migrations(pool)
            .await
            .context("failed to run migrations");
    }
    let report = migrate::read_report(
        &config.database_url,
        db::migrator(),
        &config.plugins_dirs,
        &config.disabled_plugins,
    )
    .await?;
    let kernel = &report.kernel;
    if !kernel.problems.is_empty() {
        anyhow::bail!(
            "the database schema cannot be used by this binary:\n  {}",
            kernel.problems.join("\n  ")
        );
    }
    if !kernel.pending.is_empty() {
        let pending: Vec<String> = kernel
            .pending
            .iter()
            .map(|(v, d)| format!("{v} {d}"))
            .collect();
        anyhow::bail!(
            "the database schema is behind this binary (MIGRATE_ON_START=false): \
             run `trovato migrate` first. Pending kernel migrations:\n  {}",
            pending.join("\n  ")
        );
    }
    Ok(())
}

// path_alias_fallback lives in middleware::path_alias and is reused by tests.

/// Run the HTTP server (original startup path).
async fn run_server(log_receiver: system_log::SystemLogReceiver) -> Result<()> {
    info!("Starting Trovato kernel");

    // Load configuration from environment
    let config = Config::from_env().context("failed to load configuration")?;
    info!(port = config.port, "Configuration loaded");

    // Refuse to start on a file store this server cannot use (see
    // `file::startup_check`: a directory must pass in full; a remote store
    // that cannot be reached is logged and the server starts, so a pod
    // restarted during its outage does not crash loop).
    let file_store =
        crate::file::build_storage(&config.file_storage, &config.uploads_dir, &config.files_url)?;
    crate::file::startup_check(file_store.as_ref()).await?;

    // With migrations as their own step, wait (alive, not ready) until the
    // schema this binary needs is there, rather than exit and crash loop.
    //
    // The signal is listened for from here on in that mode, once, so a SIGTERM
    // that arrives while the application is still being built is kept and
    // starts the drain as soon as the server runs. (Tokio replaces the default
    // action for good once a handler exists; a second listener created later
    // would not see it.) The default mode listens from the server start, as it
    // always has.
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let mut stop_signal = None;
    let early_listener = if config.migrate_on_start {
        None
    } else {
        let signal = listen_for_signal();
        stop_signal = Some(signal.clone());
        match wait_for_schema(&config, addr, signal).await? {
            Some(listener) => Some(listener),
            // Asked to stop while waiting.
            None => return Ok(()),
        }
    };

    // Initialize application state (database connections, etc.)
    let state = AppState::new(&config)
        .await
        .context("failed to initialize application state")?;

    info!("Database and Redis connections established");

    // The site log (`/admin/reports/log`): warnings and errors from here on
    // are written to `system_log`.
    system_log::spawn_writer(state.db().clone(), log_receiver);

    // Create a cancellation token for coordinated shutdown of background tasks.
    let shutdown_token = tokio_util::sync::CancellationToken::new();

    // Spawn background task to monitor database pool utilization
    {
        let db = state.db().clone();
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let size = db.size();
                        let idle = db.num_idle() as u32;
                        let active = size.saturating_sub(idle);
                        let pct = (active * 100).checked_div(size).unwrap_or(0);
                        if pct >= 80 {
                            warn!(
                                active = active,
                                total = size,
                                pct = pct,
                                "Database pool utilization at {pct}% ({active}/{size} connections active)"
                            );
                        }
                    }
                    _ = token.cancelled() => {
                        info!("DB pool monitor shutting down");
                        break;
                    }
                }
            }
        });
    }

    // Flush search analytics from memory to the database every 30 seconds, and
    // once more on shutdown, so a clean stop loses nothing and a crash at most
    // one window. `flush` logs its own failures and drops that batch.
    {
        let search = state.search().clone();
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(crate::search::analytics::FLUSH_INTERVAL);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let _ = search.analytics().flush().await;
                    }
                    _ = token.cancelled() => {
                        let _ = search.analytics().flush().await;
                        break;
                    }
                }
            }
        });
    }

    // Spawn the resident plugin-queue runner (P11d / D-46), but only when it is
    // enabled in site_config at boot. The default deployment runs no queue
    // runner and drains the queue on the cron cadence (the built-in scheduler's
    // or an external caller's); enabling the
    // runner is an explicit, config-gated opt-in for sub-cron latency.
    //
    // Its token is cancelled when shutdown begins, not when the server has
    // stopped: it takes no new batch from SIGTERM on, and a batch it has claimed
    // finishes or, if the process is gone first, is reclaimed when its lease
    // expires.
    let workers_token = tokio_util::sync::CancellationToken::new();
    let mut queue_runner = None;
    {
        let cron = state.cron().clone();
        let runner_config = cron.queue_runner_config().await;
        if runner_config.enabled {
            let token = workers_token.clone();
            queue_runner = Some(tokio::spawn(
                async move { cron.run_queue_runner(token).await },
            ));
            info!(
                poll_interval_secs = runner_config.poll_interval_secs,
                "Resident plugin-queue runner enabled (cron drain steps aside)"
            );
        }
    }

    // The built-in cron scheduler (B178): calls `CronService::run` on its own
    // cadence, so a site runs cron (scheduled publishing, search rebuilds,
    // session and temp-file cleanup) with no external caller configured.
    // Always spawned, unlike the queue runner above: it rereads
    // `cron_interval_secs` every time it wakes, so turning it off or on, or
    // changing the interval, applies without a restart. `/cron/{key}` and the
    // admin "Run now" control are unaffected: all three call the same `run`
    // and share its Redis lock, so whichever arrives second is skipped.
    //
    // On `workers_token`, like the queue runner: it starts no new run from the
    // signal on, and where a drain is configured the run in flight is waited
    // for below, within the shutdown deadline.
    let cron_scheduler = {
        let cron = state.cron().clone();
        let scheduler_config = cron.scheduler_config().await;
        let token = workers_token.clone();
        info!(
            enabled = scheduler_config.enabled(),
            interval_secs = scheduler_config.interval_secs,
            source = ?scheduler_config.source,
            "Built-in cron scheduler starting"
        );
        tokio::spawn(async move { cron.run_scheduler(token).await })
    };

    // Precise timers (B278): one loop per process that sleeps until the next
    // timer is due, wakes for a write that moves one earlier (in this process,
    // and on every server through Postgres NOTIFY), and rechecks at least once
    // a minute. Always spawned: an idle site runs one indexed query a minute.
    // On `workers_token`, like the cron scheduler: no timer starts from the
    // signal on, and the ones firing are waited for below, within the
    // shutdown deadline.
    let timer_scheduler = {
        let timers = state.timers().clone();
        let token = workers_token.clone();
        tokio::spawn(async move { timers.run_scheduler(token).await })
    };

    // The mail workers (BG18): whenever a transport is configured, so a
    // queued message is sent within seconds with nothing else to enable. On the
    // same token as the queue runner: they take no new message from SIGTERM on,
    // put back what they claimed and did not start, and a killed process's
    // claims return when their leases expire.
    let mail_workers = state
        .email()
        .map(|email| services::mail_worker::spawn_workers(email, &workers_token))
        .unwrap_or_default();
    if !mail_workers.is_empty() {
        info!(workers = mail_workers.len(), "Mail workers started");
    }

    // The edge cache purge drain (B303): turns pending tags into one
    // `tap_cache_purge` dispatch a second, for as long as the process runs.
    crate::edge_cache::spawn_drain(state.clone());

    // Create session layer
    let same_site = match config.cookie_same_site.as_str() {
        "lax" => SameSite::Lax,
        "none" => SameSite::None,
        _ => SameSite::Strict,
    };
    // COOKIE_SECURE=never is legitimate only on a plain HTTP LAN or development
    // site; say so on every start, so it is never on by accident.
    if !config.runtime.cookie_secure.is_secure() {
        warn!(
            setting = "COOKIE_SECURE",
            value = config.runtime.cookie_secure.as_str(),
            "COOKIE_SECURE=never: the session cookie is not Secure and is sent over plain HTTP. \
             Use this only for a plain HTTP LAN or development site, never on the internet."
        );
    }
    let session_layer =
        session::create_session_layer(&config.redis_url, same_site, config.runtime.cookie_secure)
            .await
            .context("failed to create session layer")?;

    // Log plugin and content type info
    info!(
        plugins = state.plugin_runtime().plugin_count(),
        content_types = state.content_types().len(),
        "Plugins and content types loaded"
    );

    // Build CORS layer from config
    let cors = build_cors_layer(&config);

    // Build the inner router with all routes (no path alias — that's handled
    // by the fallback below so it runs BEFORE Axum route matching).
    // Say so, once, when a plugin declares a callback the kernel cannot reach.
    let plugin_menus = state.menu_registry().all().cloned().collect::<Vec<_>>();
    routes::plugin_api::warn_unreachable_callbacks(&plugin_menus);

    // Refuse, with a message naming the plugin, when one claims a path the
    // kernel serves and cannot yield. `Router::merge` panics on an overlapping
    // route, so before this check a plugin could take the process down at boot
    // with axum's message instead of an explanation of what to do about it.
    routes::sitemap::ensure_reserved_paths_unclaimed(&plugin_menus)?;

    // Every absolute URL a feed or the sitemap emits is built from SITE_URL,
    // never from the request's Host header; say so once when it cannot be a
    // public address.
    routes::feed::warn_if_site_url_unusable(&state.runtime().site_url);
    warn_missing_field_indexes(&state).await;
    state.reconcile_field_indexes();

    let inner_router: Router<AppState> = Router::new()
        .merge(routes::front::router())
        .merge(routes::install::router())
        .merge(routes::auth::router())
        .merge(routes::user_delete::router())
        .merge(routes::admin::router())
        .merge(routes::password_reset::router())
        .merge(routes::webauthn::router())
        .merge(routes::sessions::router())
        .merge(routes::recovery::router())
        .merge(routes::health::router())
        .merge(routes::item::router())
        .merge(routes::gather::router())
        .merge(routes::gather_admin::router())
        .merge(routes::plugin_admin::router())
        .merge(routes::search::router())
        .merge(routes::cron::router())
        .merge(routes::file::router())
        .merge(routes::metrics::router())
        .merge(routes::api_token::router())
        // The AI Assistant's themed page (its API routes are in api_v1).
        .merge(routes::assistant::router())
        // Every kernel JSON route, under /api/v1, with the deprecated aliases.
        .merge(routes::api_v1::router(&state))
        .merge(routes::tile_admin::router())
        .merge(routes::static_files::router())
        .merge(routes::sitemap::router())
        // Plugin-gated routes — runtime middleware returns 404 when disabled.
        .merge(routes::gated_plugin_routes(&state))
        // Dynamic gather route aliases from query display configs.
        .merge(routes::gather_routes::build_gather_route_router(
            &state.gather().list_queries(),
        ))
        // RSS and Atom feeds declared by gather query display configs.
        .merge(routes::feed::build_feed_router(&state.gather().list_queries()))
        // Plugin-served API routes from `tap_menu` entries whose handler_type
        // is `api` (G-NO-PLUGIN-HTTP). Entries that name a callback without
        // asking for `api` are logged: that combination registers nothing.
        .merge(routes::plugin_api::build_plugin_api_router(
            &state.menu_registry().all().cloned().collect::<Vec<_>>(),
        ))
        // Owner routes for the vocabulary owners plugins declare (B108).
        .merge(routes::owned_vocabulary::router(&state));

    // Wrap the inner router with state so we can clone it for the fallback.
    let inner_with_state: Router = inner_router.clone().with_state(state.clone());
    let shared_router = Arc::new(inner_with_state);

    // Build the outer app with a fallback that handles path alias resolution.
    // In Axum 0.8, Router::layer() middleware runs AFTER route matching, so
    // URI rewriting in middleware cannot affect which route is matched.
    // The fallback receives all unmatched requests and forwards them to the
    // inner router after resolving any URL alias.
    let app = inner_router
        .fallback({
            let router = shared_router.clone();
            let app_state = state.clone();
            move |session: Session, request: axum::extract::Request| {
                let router = router.clone();
                let app_state = app_state.clone();
                async move {
                    crate::middleware::path_alias_fallback(app_state, session, router, request).await
                }
            }
        })
        // Middleware layers (last added = first executed in request flow):
        // TraceLayer → track_request_timing → security_headers → CORS → session → rate_limit(per-IP) →
        // bearer_auth → api_token → rate_limit(per-user) → install_check →
        // negotiate_language → redirect → routes
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_redirect,
        ))
        // Who is acting and from where, for the audit rows the write services
        // record. Inside the session and API token layers, which it reads.
        .layer(axum::middleware::from_fn(
            crate::middleware::scope_audit_actor,
        ))
        // FR-7b: maintain the per-user session index. Inside the session layer
        // so it can read the session the handler just wrote (a login, or a
        // cycle_id), which is exactly what it needs to observe.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::track_session,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::negotiate_language,
        ))
        // Maintenance mode: a 503 for everyone but a holder of the bypass
        // permission. After the installation check, inside the session layer.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_maintenance,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_installation,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_authenticated_rate_limit,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::authenticate_api_token,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::authenticate_bearer_token,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_rate_limit,
        ))
        // Resolve the trusted-proxy-gated client IP once (RATE-1), outer to the
        // rate-limit checks and all handlers so they share one vetted value.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::resolve_client_ip,
        ))
        // One error page (B84): every marked error, and every bare plain-text
        // one, rendered as the themed error page for a browser and as the
        // AppError body for anyone else. Inside the session layer, which the
        // page's navigation needs; outside everything else, so a rate-limit or
        // authentication refusal is rendered the same way.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::render_error_pages,
        ))
        // The theme each render uses: the admin theme for `/admin`, the
        // default otherwise (B108). Outside the error-page layer, so an error
        // page is drawn in the request's theme too.
        .layer(axum::middleware::from_fn(crate::middleware::scope_theme))
        .layer(session_layer)
        // Edge caching headers (B303): outside the session layer, so it sees
        // the session layer's `Set-Cookie`, and inside the security headers
        // layer below.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::set_cache_headers,
        ))
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::inject_security_headers,
        ))
        // Request timing, outside every other layer so `Server-Timing` measures
        // the whole request rather than a part of it.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::track_request_timing,
        ))
        .layer(crate::telemetry::trace_layer())
        .with_state(state.clone());

    // The front page: `/` served as the configured path, rewritten before
    // routing, outside every layer (`routes::front`).
    let app = routes::front::with_front_page(state.clone(), app);

    // Keep a reference to the DB pool for graceful shutdown cleanup.
    let db_pool = state.db().clone();

    // Start the server: on the socket the waiting server holds, if there is
    // one, handed over only now so the probes answer while the app is built.
    let listener = match early_listener {
        Some(waiting) => waiting.hand_over().await?,
        None => tokio::net::TcpListener::bind(addr)
            .await
            .context("failed to bind to address")?,
    };

    info!(%addr, "Server listening");

    // Shutdown (`docs/design/upgrades-and-probes.md` §6). At the signal,
    // readiness turns false and the queue runner stops taking work; on SIGTERM
    // the server keeps serving for the drain; then it stops accepting. With a
    // drain, in-flight requests, the queue runner and the pool close share one
    // SHUTDOWN_TIMEOUT_SECS from there. Without one, as before, in-flight requests
    // are not bounded, the queue runner is not waited for, and closing the
    // pool gets its own SHUTDOWN_TIMEOUT_SECS.
    let stop_signal = stop_signal.unwrap_or_else(listen_for_signal);
    let deadline = Arc::new(std::sync::OnceLock::<tokio::time::Instant>::new());
    let (stopping_tx, stopping_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = {
        let probes = state.probes().clone();
        let workers = workers_token.clone();
        let drain = config.shutdown_drain;
        let deadline = deadline.clone();
        let timeout = config.shutdown_timeout;
        async move {
            let signal = signalled(stop_signal).await;
            probes.start_draining();
            workers.cancel();
            if signal == Signal::Terminate && !drain.is_zero() {
                info!(
                    drain_secs = drain.as_secs(),
                    "Draining: reporting not ready, still serving"
                );
                tokio::time::sleep(drain).await;
            }
            info!("No longer accepting connections; finishing in-flight requests");
            let _ = deadline.set(tokio::time::Instant::now() + timeout);
            let _ = stopping_tx.send(());
        }
    };
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown);
    // In-flight requests are cut off at the deadline only where a drain is
    // configured (Kubernetes, which kills the pod at its grace period anyway).
    // Without one, a single server waits for them as it always has.
    let bound_in_flight = !config.shutdown_drain.is_zero();
    let in_flight_deadline = async {
        match (stopping_rx.await, deadline.get()) {
            (Ok(()), Some(at)) if bound_in_flight => tokio::time::sleep_until(*at).await,
            _ => std::future::pending().await,
        }
    };
    tokio::select! {
        result = std::future::IntoFuture::into_future(server) => result.context("server error")?,
        () = in_flight_deadline => warn!(
            timeout_secs = config.shutdown_timeout.as_secs(),
            "In-flight requests did not finish within SHUTDOWN_TIMEOUT_SECS; closing them"
        ),
    }

    // Server has stopped accepting new connections.
    // Signal all background tasks to stop.
    info!("Signaling background tasks to shut down...");
    shutdown_token.cancel();
    let deadline = deadline
        .get()
        .copied()
        .unwrap_or_else(|| tokio::time::Instant::now() + config.shutdown_timeout);
    // Waited for only where a drain is configured, like in-flight requests: a
    // single server stops as fast as it did, its claims expiring as before.
    if let Some(runner) = queue_runner.filter(|_| bound_in_flight)
        && tokio::time::timeout_at(deadline, runner).await.is_err()
    {
        warn!(
            "The queue runner did not finish its batch; its claims are reclaimed when their leases expire"
        );
    }
    // Likewise the cron scheduler: it has stopped starting runs, and a run in
    // flight that is cut off here leaves the lock to expire on its TTL.
    if bound_in_flight
        && tokio::time::timeout_at(deadline, cron_scheduler)
            .await
            .is_err()
    {
        warn!("The scheduled cron run did not finish; its lock expires on its own TTL");
    }
    // Always waited for, within the deadline: a timer firing finishes, and
    // one cut off here is claimed again by any server when its lease ends.
    if tokio::time::timeout_at(deadline, timer_scheduler)
        .await
        .is_err()
    {
        warn!("A timer did not finish before the shutdown deadline; its lease returns it");
    }
    // Always waited for, within the deadline: a mail worker stops after at most
    // the one send in flight, and putting its other claims back now saves them
    // the lease (MAIL_CLAIM_LEASE_SECS) before another process may send them.
    for worker in mail_workers {
        if tokio::time::timeout_at(deadline, worker).await.is_err() {
            warn!("A mail worker did not stop in time; its claims return when their leases expire");
            break;
        }
    }

    // Give background tasks a moment to finish their current iteration.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Close external connections with a timeout.
    info!("Closing external connections...");
    // With a drain, what is left of the one budget, but at least a second.
    // Without one, a fresh SHUTDOWN_TIMEOUT_SECS, as a single server always had.
    let close_by = if bound_in_flight {
        deadline.max(tokio::time::Instant::now() + std::time::Duration::from_secs(1))
    } else {
        tokio::time::Instant::now() + config.shutdown_timeout
    };
    let drain_result = tokio::time::timeout_at(close_by, async {
        db_pool.close().await;
        info!("Database pool closed");
    })
    .await;

    if drain_result.is_err() {
        warn!(
            timeout_secs = config.shutdown_timeout.as_secs(),
            "Shutdown timeout reached, forcing exit"
        );
    }

    info!("Shutdown complete");
    Ok(())
}

/// The socket `serve` binds while it waits for its schema
/// (`MIGRATE_ON_START=false`), and the probe-only server answering on it.
struct WaitingServer {
    listener: std::net::TcpListener,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl WaitingServer {
    /// Stop answering probes and give the socket to the full server.
    /// Connections that arrive in between wait in the socket's backlog.
    ///
    /// Bounded: a client holding a half-sent request would otherwise keep the
    /// probe server's graceful shutdown, and so the hand-over, waiting forever.
    async fn hand_over(self) -> Result<tokio::net::TcpListener> {
        let _ = self.stop.send(());
        let mut task = self.task;
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
        tokio::net::TcpListener::from_std(self.listener)
            .context("failed to take over the listening socket")
    }
}

/// `serve` with `MIGRATE_ON_START=false`: bind the port and answer the probes
/// (alive, not ready) until the schema has everything this binary needs,
/// checking every two seconds, so a pod started before its migration Job has
/// finished waits instead of crash looping. Changes nothing in the database.
///
/// Returns the waiting server, still answering, for the caller to
/// [`hand_over`](WaitingServer::hand_over) once the application is built; or
/// `None` when a shutdown signal came first.
async fn wait_for_schema(
    config: &Config,
    addr: SocketAddr,
    stop_signal: tokio::sync::watch::Receiver<Option<Signal>>,
) -> Result<Option<WaitingServer>> {
    let listener = std::net::TcpListener::bind(addr).context("failed to bind to address")?;
    listener
        .set_nonblocking(true)
        .context("failed to configure the listening socket")?;
    let probe_listener = tokio::net::TcpListener::from_std(
        listener
            .try_clone()
            .context("failed to share the listening socket")?,
    )?;
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        axum::serve(probe_listener, routes::health::waiting_router())
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
    });
    let waiting = WaitingServer {
        listener,
        stop,
        task,
    };
    info!(%addr, "MIGRATE_ON_START=false: checking the schema; answering probes only until it is ready");

    let signal = signalled(stop_signal);
    tokio::pin!(signal);
    let mut last_reason = String::new();
    loop {
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            migrate::read_report(
                &config.database_url,
                db::migrator(),
                &config.plugins_dirs,
                &config.disabled_plugins,
            ),
        )
        .await;
        let reason = match report {
            Ok(Ok(report)) if report.ready() => return Ok(Some(waiting)),
            Ok(Ok(report)) => {
                let mut reason = format!("{} migration(s) pending", report.pending());
                for problem in report.problems() {
                    reason.push_str("; ");
                    reason.push_str(&problem);
                }
                reason
            }
            Ok(Err(e)) => format!("{e:#}"),
            Err(_) => "PostgreSQL did not answer within 5 seconds".to_string(),
        };
        if reason != last_reason {
            warn!(%reason, "Not ready: waiting for `trovato migrate`");
            last_reason = reason;
        }
        tokio::select! {
            _ = &mut signal => {
                let _ = waiting.hand_over().await;
                return Ok(None);
            }
            () = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    }
}

/// Listen for SIGINT and SIGTERM from now on; the receiver holds the first.
fn listen_for_signal() -> tokio::sync::watch::Receiver<Option<Signal>> {
    let (tx, rx) = tokio::sync::watch::channel(None);
    tokio::spawn(async move {
        let signal = shutdown_signal().await;
        let _ = tx.send(Some(signal));
    });
    rx
}

/// Resolve once a signal has been received, now or before.
async fn signalled(mut rx: tokio::sync::watch::Receiver<Option<Signal>>) -> Signal {
    let received = rx.wait_for(Option::is_some).await.ok().and_then(|s| *s);
    match received {
        Some(signal) => signal,
        // The listener is gone without a signal: never stop on that.
        None => std::future::pending().await,
    }
}

/// Which signal asked the process to stop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// SIGINT (Ctrl-C at a terminal): stop without a drain.
    Interrupt,
    /// SIGTERM: drain for `SHUTDOWN_DRAIN_SECS` first (0 unless under Kubernetes or set).
    Terminate,
}

/// Wait for a shutdown signal (SIGINT or SIGTERM).
async fn shutdown_signal() -> Signal {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => {
                        info!("Received SIGINT, starting graceful shutdown");
                        Signal::Interrupt
                    }
                    _ = sigterm.recv() => {
                        info!("Received SIGTERM, starting graceful shutdown");
                        Signal::Terminate
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to install SIGTERM handler, using SIGINT only");
                ctrl_c.await.ok();
                info!("Received SIGINT, starting graceful shutdown");
                Signal::Interrupt
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        info!("Received SIGINT, starting graceful shutdown");
        Signal::Interrupt
    }
}

/// Run a plugin CLI command with a minimal context (pool only).
async fn run_plugin_command(action: PluginAction) -> Result<()> {
    // `New` needs no DB connection — handle it before pool setup.
    if let PluginAction::New { name } = action {
        let workspace_root = std::env::current_dir().context("failed to get current directory")?;
        return plugin::cli::cmd_plugin_new(&workspace_root, &name);
    }

    let config = Config::from_env().context("failed to load configuration")?;

    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;

    // The kernel schema has to be current: plugin_status is a kernel table.
    prepare_schema_for_cli(&pool, &config).await?;

    match action {
        PluginAction::New { .. } => unreachable!("handled above"),
        PluginAction::List => {
            plugin::cli::cmd_plugin_list(&pool, &config.plugins_dirs).await?;
            return Ok(());
        }
        PluginAction::Install { name } => {
            plugin::cli::cmd_plugin_install(&pool, &config.plugins_dirs, &name).await?;
        }
        PluginAction::Migrate { name } => {
            plugin::cli::cmd_plugin_migrate(&pool, &config.plugins_dirs, name.as_deref()).await?;
        }
        PluginAction::Enable { name } => {
            plugin::cli::cmd_plugin_enable(&pool, &config.plugins_dirs, &name).await?;
        }
        PluginAction::Disable { name } => {
            plugin::cli::cmd_plugin_disable(&pool, &name).await?;
        }
    }

    // Install and migrate may write any table a plugin's migrations touch.
    // Which plugins run still waits for a restart.
    println!("{}", bump_from_cli(&config, &CacheKind::ALL).await);

    Ok(())
}

/// Run a config CLI command with a minimal context (pool only).
async fn run_config_command(action: ConfigAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;

    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;

    prepare_schema_for_cli(&pool, &config).await?;

    // `file` config entities carry bytes, which go to the site's file storage.
    let files = std::sync::Arc::new(crate::file::FileService::new(
        pool.clone(),
        crate::file::build_storage(&config.file_storage, &config.uploads_dir, &config.files_url)?,
    ));
    // Plugin config tables are named in the manifests on disk.
    let plugins = config_storage::PluginConfigSet::discover(&config.plugins_dirs).await;
    let storage = config_storage::DirectConfigStorage::new(pool.clone())
        .with_files(files)
        .with_plugins(plugins);

    match action {
        ConfigAction::Export { dir, clean } => {
            let dir = std::path::PathBuf::from(dir);
            let result = config_storage::yaml::export_config(&storage, &pool, &dir, clean).await?;
            print_config_summary("Exported", &dir, &result.counts, &result.warnings);
        }
        ConfigAction::Import { dir, dry_run } => {
            let dir = std::path::PathBuf::from(dir);
            let result =
                config_storage::yaml::import_config(&storage, &pool, &dir, dry_run).await?;
            let verb = if dry_run { "Would import" } else { "Imported" };
            print_config_summary(verb, &dir, &result.counts, &result.warnings);
            if !dry_run {
                // Any cached kind may have changed, so every kind is bumped.
                println!("{}", bump_from_cli(&config, &CacheKind::ALL).await);
                // Imported content, aliases and search settings bypass the item
                // service, so the static search index is rebuilt in full.
                crate::cron::request_search_index_rebuild(&pool).await;
                if result.counts.contains_key("plugin_status") {
                    println!("{CLI_PLUGIN_RESTART_NOTE}");
                }
            }
        }
    }

    Ok(())
}

/// End every session of `user_id` through the session index in Redis, and say
/// what happened in a line fit for the terminal.
///
/// Used by the command line password reset, which runs in its own process
/// without an `AppState`. The site's session tracking refuses a session whose
/// index entry is gone, so removing the entries here ends the sessions in the
/// running site too.
async fn end_sessions_from_cli(config: &Config, user_id: uuid::Uuid) -> String {
    let registry = match redis::Client::open(config.redis_url.as_str()) {
        Ok(client) => services::session_registry::SessionRegistry::new(
            client,
            session::DEFAULT_SESSION_EXPIRY_HOURS * 3600,
        ),
        Err(e) => {
            return format!(
                "Existing sessions were NOT ended: the Redis URL is not usable ({e}). \
                 They end when they idle out, or from the user's sessions page."
            );
        }
    };
    match registry.revoke_all_except(user_id, None).await {
        Ok(revoked) => format!("Ended {} existing session(s).", revoked.len()),
        Err(e) => format!(
            "Existing sessions were NOT ended: Redis could not be reached ({e:#}). \
             They end when they idle out, or from the user's sessions page."
        ),
    }
}

/// Run a user CLI command with a minimal context (pool only).
async fn run_user_command(action: UserAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;

    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;

    prepare_schema_for_cli(&pool, &config).await?;

    match action {
        UserAction::Create {
            username,
            mail,
            roles,
            blocked,
            no_password,
        } => {
            let account = NewAccount {
                username,
                mail: mail.unwrap_or_default(),
                roles,
                blocked,
            };
            let password = if no_password {
                None
            } else {
                Some(password_prompt::read_password_from_stdin("Password: ")?)
            };
            create_user_from_cli(&config, account, password).await?;
        }
        UserAction::ResetPassword { username, password } => {
            let password = match password {
                Some(p) => p,
                None => password_prompt::read_password_from_stdin("New password: ")?,
            };

            if password.len() < 12 {
                anyhow::bail!("Password must be at least 12 characters.");
            }

            let user = models::User::find_by_name(&pool, &username)
                .await?
                .context(format!("User '{username}' not found."))?;

            let updated = services::user::reset_password_offline(&pool, user.id, &password).await?;
            if updated {
                println!("Password updated for user '{username}'.");
            } else {
                anyhow::bail!("Failed to update password for user '{username}'.");
            }

            // A reset ends the account's existing sessions, as a reset through
            // the site does. This process has no running site to ask, so it
            // goes to the same Redis-backed session index directly. The
            // password is already changed either way, so a Redis that cannot
            // be reached is reported rather than turned into a failure.
            println!("{}", end_sessions_from_cli(&config, user.id).await);
            println!("{}", bump_from_cli(&config, &[CacheKind::Users]).await);
        }
        UserAction::RoleAdd { username, role } => {
            let (user, role) = find_user_and_role(&pool, &username, &role).await?;
            // Idempotent: `assign_to_user` upserts, so re-running is not an
            // error, and saying which it was is more use than a bare "done".
            //
            // The grant and its `user.roles_changed` commit together.
            let audit = services::audit::AuditTrail::offline(pool.clone()).await;
            // Whether this assigned the role is the insert's own answer, so a
            // grant is recorded exactly when this run made it.
            let mut tx = pool.begin().await?;
            let already = !models::Role::assign_to_user(&mut *tx, user.id, role.id).await?;
            let name = if already {
                None
            } else {
                Some(
                    services::role::membership_event_in(&audit, &mut tx, user.id, role.id, "added")
                        .await?,
                )
            };
            tx.commit().await?;
            if let Some(name) = name {
                services::role::record_membership(&audit, user.id, role.id, &name, "added").await;
            }
            if already {
                println!("User '{username}' already had role '{}'.", role.name);
            } else {
                println!("User '{username}' now has role '{}'.", role.name);
                println!(
                    "{}",
                    bump_from_cli(&config, &[CacheKind::Permissions]).await
                );
            }
        }
        UserAction::RoleRemove { username, role } => {
            let (user, role) = find_user_and_role(&pool, &username, &role).await?;
            // The removal and its `user.roles_changed` commit together.
            let audit = services::audit::AuditTrail::offline(pool.clone()).await;
            let mut tx = pool.begin().await?;
            models::Role::remove_from_user(&mut *tx, user.id, role.id).await?;
            let name =
                services::role::membership_event_in(&audit, &mut tx, user.id, role.id, "removed")
                    .await?;
            tx.commit().await?;
            services::role::record_membership(&audit, user.id, role.id, &name, "removed").await;
            println!("User '{username}' no longer has role '{}'.", role.name);
            println!(
                "{}",
                bump_from_cli(&config, &[CacheKind::Permissions]).await
            );
        }
        UserAction::Roles { username } => {
            let user = models::User::find_by_name(&pool, &username)
                .await?
                .context(format!("User '{username}' not found."))?;
            let roles = models::Role::get_user_roles(&pool, user.id).await?;
            if roles.is_empty() {
                println!("User '{username}' holds no roles.");
            } else {
                println!("User '{username}' holds {} role(s):", roles.len());
                for role in roles {
                    println!("  {}", role.name);
                }
            }
        }
    }

    Ok(())
}

/// What `trovato user create` was asked for, short of the password.
struct NewAccount {
    username: String,
    mail: String,
    roles: Vec<String>,
    blocked: bool,
}

/// `trovato user create`: the admin "Add user" form's checks and write, from
/// the command line.
///
/// The account is written by the site's own `UserService`, as the form writes
/// it, so `tap_user_register` reaches the enabled plugins and the audit trail
/// records it; roles go through `RoleService`, which audits each grant as the
/// form's role checkboxes do. That takes the full server state, built here the
/// way `trovato generate` builds it. `password` is `None` for `--no-password`.
async fn create_user_from_cli(
    config: &Config,
    account: NewAccount,
    password: Option<String>,
) -> Result<()> {
    let state = AppState::new(config)
        .await
        .context("failed to initialize application state")?;

    let mut problems = routes::admin_user::new_account_problems(
        &state,
        &account.username,
        &account.mail,
        password.as_deref(),
    )
    .await;
    let mut roles = Vec::new();
    for name in &account.roles {
        match models::Role::find_by_name(state.db(), name).await? {
            Some(role) => roles.push(role),
            None => problems.push(routes::admin_form::FieldError::form(
                i18n::UiMessage::new("Role '{name}' not found.").arg("name", name),
            )),
        }
    }
    if !problems.is_empty() {
        let lines: Vec<String> = problems
            .iter()
            .map(|p| p.message.text(None, "en"))
            .collect();
        anyhow::bail!("{}", lines.join("\n"));
    }

    let input = models::CreateUser {
        name: account.username.clone(),
        password: password.unwrap_or_default(),
        mail: account.mail.clone(),
        // Never a superuser: the form allows it only to a superuser, and
        // nobody is signed in here.
        is_admin: false,
    };
    let nobody = tap::UserContext::anonymous();
    let user = if account.blocked {
        state
            .users()
            .create_with_status(input, 0, &nobody, "cli")
            .await?
    } else {
        state.users().create(input, &nobody, "cli").await?
    };
    // The account is committed before its roles, one grant at a time, as on
    // the form. A grant that fails (a role deleted since it was looked up, a
    // database error) cannot be undone by running the command again, which
    // would now refuse the taken name; so every grant is still tried, and the
    // failure names what is missing and the commands that add it.
    let mut missing = Vec::new();
    for role in &roles {
        if let Err(e) = state.roles().assign_to_user(user.id, role.id).await {
            missing.push((role.name.as_str(), e));
        }
    }
    if !missing.is_empty() {
        println!(
            "{}",
            bump_from_cli(config, &[CacheKind::Users, CacheKind::Permissions]).await
        );
        let mut message = format!(
            "User '{}' ({}) was created, but {} of its roles could not be granted:",
            user.name,
            user.id,
            missing.len()
        );
        for (name, e) in &missing {
            message.push_str(&format!("\n  {name}: {e:#}"));
        }
        message.push_str("\nTo finish, run:");
        for (name, _) in &missing {
            message.push_str(&format!("\n  trovato user role-add {} {name}", user.name));
        }
        anyhow::bail!(message);
    }

    let status = if account.blocked { "blocked" } else { "active" };
    println!("Created {status} user '{}' ({}).", user.name, user.id);
    if !roles.is_empty() {
        let names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
        println!("Roles: {}.", names.join(", "));
    }
    if user.pass.is_empty() {
        println!(
            "It has no password and cannot sign in until one is set: \
             trovato user reset-password {}",
            user.name
        );
    }
    if let Some(warning) =
        routes::admin_user::access_rule_warning(&state, &user.name, &account.mail).await
    {
        println!("Warning: {warning}");
    }
    println!(
        "{}",
        bump_from_cli(config, &[CacheKind::Users, CacheKind::Permissions]).await
    );
    Ok(())
}

/// `trovato import`: content from another site, through `import::import`
/// (`docs/design/import.md`). Runs as the operator, the bypass every command
/// here runs with; the records are still checked as the forms check them.
async fn run_import_command(file: &std::path::Path, dry_run: bool) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;
    let state = AppState::new(&config)
        .await
        .context("failed to initialize application state")?;

    let name = file.display().to_string();
    let report = if name == "-" {
        import::import(&state, std::io::stdin().lock(), dry_run).await?
    } else {
        let opened = std::fs::File::open(file).with_context(|| format!("cannot open {name}"))?;
        import::import(&state, std::io::BufReader::new(opened), dry_run).await?
    };

    let verb = if dry_run { "Would import" } else { "Imported" };
    println!("{verb} {} records from {name}:", report.records);
    for line in report.lines() {
        println!("{line}");
    }
    println!(
        "{} change(s){}.",
        report.changes(),
        if dry_run { ", none written" } else { "" }
    );
    if let Some(note) = &report.cache_note {
        println!("{note}");
    }
    Ok(())
}

/// `trovato item`: an operator acting directly on an item, the same database
/// access every other command here already has.
async fn run_item_command(action: ItemAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;
    let state = AppState::new(&config)
        .await
        .context("failed to initialize application state")?;

    match action {
        ItemAction::RevisionDelete { item, revision } => {
            // The operator bypass every CLI command runs with (`user
            // role-add`'s own doc, `docs/admin-permissions.md`): whoever can
            // reach the database already has it, so `delete revisions` and
            // delete access on the item are not asked again here.
            let operator = tap::UserContext::administrator(Uuid::nil(), Vec::new());
            match state
                .items()
                .delete_revision(item, revision, &operator)
                .await
            {
                Ok(()) => {
                    println!("Revision '{revision}' of item '{item}' deleted.");
                }
                Err(content::RevisionDeleted::NotFound) => {
                    anyhow::bail!(
                        "No such revision '{revision}' of item '{item}' (or no such item)."
                    );
                }
                Err(content::RevisionDeleted::Forbidden) => {
                    // The administrator bypass makes this unreachable; kept
                    // so a future change to `may_delete_revisions` that adds
                    // a real refusal here fails loudly instead of silently.
                    anyhow::bail!("Refused to delete revision '{revision}' of item '{item}'.");
                }
                Err(content::RevisionDeleted::Current) => {
                    anyhow::bail!(
                        "Revision '{revision}' is item '{item}''s current revision and cannot be deleted."
                    );
                }
                Err(content::RevisionDeleted::Failed(e)) => {
                    return Err(e.context("failed to delete revision"));
                }
            }
        }
    }

    Ok(())
}

/// Resolve a username and a role name, or say which one was not found.
///
/// Both lookups are by name rather than id, because a name is what an operator
/// has in front of them. Naming the missing one matters: "not found" on its own
/// leaves them checking both.
async fn find_user_and_role(
    pool: &sqlx::PgPool,
    username: &str,
    role: &str,
) -> Result<(models::User, models::Role)> {
    let user = models::User::find_by_name(pool, username)
        .await?
        .context(format!("User '{username}' not found."))?;
    let role = models::Role::find_by_name(pool, role)
        .await?
        .context(format!("Role '{role}' not found."))?;
    Ok((user, role))
}

/// Which plugins run is read at boot, on every server, so an imported plugin
/// status waits for a restart; everything else an import writes does not.
const CLI_PLUGIN_RESTART_NOTE: &str = "Plugin enable and disable take effect when the servers restart (all of them, if more than one).";

/// `trovato cache clear`.
async fn run_search_command(action: SearchAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;
    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;
    match action {
        SearchAction::Rebuild => {
            crate::cron::request_search_index_rebuild(&pool).await;
            println!(
                "A full search index rebuild will run at the next cron run \
                 (if the trovato_search plugin is installed)."
            );
        }
    }
    Ok(())
}

async fn run_cache_command(action: CacheAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;
    match action {
        CacheAction::Clear { kinds } => {
            let kinds = if kinds.is_empty() {
                CacheKind::ALL.to_vec()
            } else {
                kinds
            };
            // Clearing is this command's whole job, so a Redis that cannot be
            // reached is a failure here, not the note it is after a write.
            clear_caches(&config, &kinds)
                .await
                .context("running servers were NOT told: Redis could not be reached")?;
            let names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
            println!("Cleared: {}.", names.join(", "));
            println!(
                "Running servers have dropped their cached copies; the next request reads the database."
            );
        }
    }
    Ok(())
}

/// Tell every running server to drop its caches of `kinds`: the one path every
/// command takes, `cache clear` included (`cache::clear_everywhere`).
///
/// A command runs in its own process, with no server cache of its own to
/// clear; the bump is how the servers hear about the write.
async fn clear_caches(config: &Config, kinds: &[CacheKind]) -> Result<()> {
    let client = redis::Client::open(config.redis_url.as_str())?;
    cache::clear_everywhere(&client, kinds).await
}

/// [`clear_caches`] after a command's write, saying what happened in a line
/// fit for the terminal: the write has happened either way, so a Redis that
/// cannot be reached is reported rather than turned into a failure.
async fn bump_from_cli(config: &Config, kinds: &[CacheKind]) -> String {
    match clear_caches(config, kinds).await {
        Ok(()) => {
            "Running servers have dropped their cached copies; the next request reads the database."
                .to_string()
        }
        Err(e) => format!(
            "Running servers were NOT told: Redis could not be reached ({e:#}). \
             They see this change when their caches expire."
        ),
    }
}

fn print_config_summary(
    verb: &str,
    dir: &std::path::Path,
    counts: &std::collections::BTreeMap<String, usize>,
    warnings: &[String],
) {
    let total: usize = counts.values().sum();
    println!("{verb} {total} config entities ({})", dir.display());
    for (entity_type, count) in counts {
        println!("  {entity_type}: {count}");
    }
    if !warnings.is_empty() {
        println!("{} warning(s):", warnings.len());
        for warning in warnings {
            println!("  warning: {warning}");
        }
    }
}

fn build_cors_layer(config: &Config) -> CorsLayer {
    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::OPTIONS,
    ];

    if config.cors_allowed_origins.len() == 1 && config.cors_allowed_origins[0] == "*" {
        CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods(methods)
            .allow_headers(tower_http::cors::Any)
    } else if config.cors_allowed_origins.is_empty() {
        // No CORS origins configured — block cross-origin requests.
        // Use a restrictive layer that allows nothing from other origins.
        CorsLayer::new().allow_methods(methods).allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
    } else {
        let origins: Vec<HeaderValue> = config
            .cors_allowed_origins
            .iter()
            .filter_map(|o| match o.parse::<HeaderValue>() {
                Ok(v) => Some(v),
                Err(_) => {
                    warn!(origin = %o, "ignoring unparseable CORS origin");
                    None
                }
            })
            .collect();

        // Specific origins: enable credentials with explicit headers
        // (tower-http disallows credentials + wildcard headers).
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(methods)
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
            ])
            .allow_credentials(true)
    }
}

/// How long shutdown waits for the last exported spans to be sent.
const EXPORT_FLUSH_LIMIT: std::time::Duration = std::time::Duration::from_secs(3);

fn init_tracing() -> Result<system_log::SystemLogReceiver> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(telemetry::DEFAULT_LOG_FILTER));
    let (log_layer, log_receiver) = system_log::SystemLogLayer::new();

    // Trace export, off by default: then no layer wants a Gander span and
    // none is ever built (`docs/tracing.md`).
    let export = telemetry::export::ExportSettings::from_env()?;
    let provider = if export.otlp {
        let exporter = telemetry::export::otlp_exporter()?;
        Some(telemetry::export::provider(&export, exporter))
    } else {
        None
    };
    telemetry::subscriber(std::io::stdout, filter, log_layer, provider).init();
    if export.otlp {
        info!(
            service = %export.service_name,
            sample_ratio = export.sample_ratio,
            "exporting traces over OTLP"
        );
    }
    Ok(log_receiver)
}

/// Name every field marked "Filterable and sortable" whose index is not
/// built yet (B207). The background reconcile started next queues the
/// missing ones; until each is built, filtering or sorting on that field
/// reads every item of its type.
async fn warn_missing_field_indexes(state: &AppState) {
    match crate::gather::field_index::missing(state.db()).await {
        Ok(missing) => {
            for (item_type, field, index_state) in missing {
                warn!(
                    item_type,
                    field, index_state, "marked field has no ready index yet; it will be built"
                );
            }
        }
        Err(e) => warn!(error = %e, "could not check item field indexes"),
    }
}
