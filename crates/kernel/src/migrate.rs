//! Schema migrations as their own step (`docs/design/upgrades-and-probes.md`).
//!
//! Three things live here:
//!
//! - [`MigrationLock`]: one Postgres advisory lock that every migrator takes,
//!   on a connection of its own, so kernel and plugin migrations from any
//!   number of processes run one process at a time, and a failed run cannot
//!   leave the lock behind in a pool;
//! - [`Report`]: what is applied, pending, and unknown (applied by a newer
//!   release), read without changing anything;
//! - [`run`]: apply what is pending, under the lock.
//!
//! An applied migration this binary does not know is accepted unless its name
//! marks it incompatible ([`crate::migration_rule::is_incompatible_name`]). That
//! is what lets the previous release run on the schema the next one migrated.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sqlx::migrate::Migrator;
use sqlx::{Connection, PgConnection, PgPool, Row};
use tracing::info;

use crate::migration_rule::{is_incompatible_description, is_incompatible_name};
use crate::plugin::PluginInfo;

/// The advisory lock key every Trovato migrator takes ("trovatoM").
///
/// Separate from the key sqlx's migrator derives from the database name: that
/// one covers only the kernel half, and is taken on a pooled connection.
pub const MIGRATE_LOCK_KEY: i64 = 0x7472_6f76_6174_6f4d;

/// Plugins by name, with their manifest and directory.
pub type PluginSet = HashMap<String, (PluginInfo, PathBuf)>;

/// The migration lock, held on a dedicated connection.
///
/// Dropping it without [`release`](Self::release) closes the connection, and
/// Postgres releases a session lock when its session ends, so an error on any
/// path still frees the lock for the next migrator.
pub struct MigrationLock {
    conn: PgConnection,
}

impl MigrationLock {
    /// Take the lock, waiting for any other migrator to finish.
    ///
    /// The connection is detached from `pool`, so it is closed rather than
    /// returned when this value goes away.
    pub async fn acquire(pool: &PgPool) -> Result<Self> {
        let conn = pool
            .acquire()
            .await
            .context("failed to get a connection for the migration lock")?
            .detach();
        Self::lock(conn).await
    }

    /// [`acquire`](Self::acquire) on a connection to `database_url`.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let conn = PgConnection::connect(database_url)
            .await
            .context("failed to connect to PostgreSQL")?;
        Self::lock(conn).await
    }

    async fn lock(mut conn: PgConnection) -> Result<Self> {
        // A role or URL default `lock_timeout` or `statement_timeout` would turn
        // the wait for another migrator into a failure: off for the wait, back
        // to the session default once the lock is held.
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "SET lock_timeout = 0; SET statement_timeout = 0",
        ))
        .execute(&mut conn)
        .await
        .context("failed to clear timeouts for the migration lock")?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&mut conn)
            .await
            .context("failed to take the migration lock")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "RESET lock_timeout; RESET statement_timeout",
        ))
        .execute(&mut conn)
        .await
        .context("failed to restore timeouts after taking the migration lock")?;
        Ok(Self { conn })
    }

    /// Limit how long a migration's statement may wait for a table lock.
    ///
    /// Set after the advisory lock is held, never before: Postgres applies
    /// `lock_timeout` to advisory lock waits too, and a second migrator must
    /// wait for the first, not fail. Zero leaves the server default.
    pub async fn set_lock_timeout(&mut self, timeout: Duration) -> Result<()> {
        if timeout.is_zero() {
            return Ok(());
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SET lock_timeout = '{}ms'",
            timeout.as_millis()
        )))
        .execute(&mut self.conn)
        .await
        .context("failed to set lock_timeout")?;
        Ok(())
    }

    /// The connection holding the lock; migrations run on it.
    pub fn conn(&mut self) -> &mut PgConnection {
        &mut self.conn
    }

    /// Release the lock and close the connection.
    pub async fn release(mut self) -> Result<()> {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&mut self.conn)
            .await
            .context("failed to release the migration lock")?;
        let _ = self.conn.close().await;
        Ok(())
    }
}

/// The kernel's migrations against the database.
#[derive(Debug, Default)]
pub struct KernelReport {
    /// Applied migrations this binary knows.
    pub applied: usize,
    /// Migrations this binary has that the database does not: (version, description).
    pub pending: Vec<(i64, String)>,
    /// Applied migrations this binary does not know: (version, description).
    pub unknown: Vec<(i64, String)>,
    /// What stops this binary from using the schema at all.
    pub problems: Vec<String>,
}

/// One plugin's migrations against the database.
#[derive(Debug, Default)]
pub struct PluginReport {
    /// The plugin.
    pub name: String,
    /// Applied migrations its manifest lists.
    pub applied: usize,
    /// Migrations its manifest lists that are not applied.
    pub pending: Vec<String>,
    /// Applied migrations its manifest does not list.
    pub unknown: Vec<String>,
    /// What stops this binary from using the schema.
    pub problems: Vec<String>,
}

/// Everything this binary would migrate, and where the database stands.
#[derive(Debug, Default)]
pub struct Report {
    /// The kernel.
    pub kernel: KernelReport,
    /// Each plugin that would be migrated, by name.
    pub plugins: Vec<PluginReport>,
}

impl Report {
    /// Read the state of the schema. Changes nothing and takes no lock.
    pub async fn read(
        conn: &mut PgConnection,
        migrator: &Migrator,
        plugins: &PluginSet,
    ) -> Result<Self> {
        let kernel = kernel_report(conn, migrator).await?;
        let mut names: Vec<&String> = plugins.keys().collect();
        names.sort();
        let applied = applied_plugin_migrations(conn).await?;
        let plugins = names
            .into_iter()
            .map(|name| plugin_report(name, &plugins[name].0, applied.get(name)))
            .collect();
        Ok(Self { kernel, plugins })
    }

    /// Migrations not yet applied, kernel and plugin.
    pub fn pending(&self) -> usize {
        self.kernel.pending.len() + self.plugins.iter().map(|p| p.pending.len()).sum::<usize>()
    }

    /// What stops this binary from using the schema at all.
    pub fn problems(&self) -> Vec<String> {
        let mut all = self.kernel.problems.clone();
        for p in &self.plugins {
            all.extend(p.problems.iter().map(|m| format!("plugin {}: {m}", p.name)));
        }
        all
    }

    /// Whether this binary can serve on the schema as it is.
    pub fn ready(&self) -> bool {
        self.pending() == 0 && self.problems().is_empty()
    }

    /// The `trovato migrate --status` text.
    pub fn render_status(&self) -> String {
        let mut out = String::new();
        let k = &self.kernel;
        out.push_str(&format!(
            "Kernel: {} applied, {} pending, {} unknown (applied by a newer release)\n",
            k.applied,
            k.pending.len(),
            k.unknown.len()
        ));
        for (v, d) in &k.pending {
            out.push_str(&format!("  pending  {v} {d}\n"));
        }
        for (v, d) in &k.unknown {
            out.push_str(&format!("  unknown  {v} {d}\n"));
        }
        for p in &self.plugins {
            out.push_str(&format!(
                "Plugin {}: {} applied, {} pending, {} unknown\n",
                p.name,
                p.applied,
                p.pending.len(),
                p.unknown.len()
            ));
            for m in &p.pending {
                out.push_str(&format!("  pending  {m}\n"));
            }
            for m in &p.unknown {
                out.push_str(&format!("  unknown  {m}\n"));
            }
        }
        for problem in self.problems() {
            out.push_str(&format!("Problem: {problem}\n"));
        }
        out.push_str(&format!("Pending: {}\n", self.pending()));
        out
    }

    /// The `trovato migrate --dry-run` text.
    pub fn render_dry_run(&self) -> String {
        let mut out = format!("Would apply {} migration(s):\n", self.pending());
        for (v, d) in &self.kernel.pending {
            out.push_str(&format!("  kernel {v} {d}\n"));
        }
        for p in &self.plugins {
            for m in &p.pending {
                out.push_str(&format!("  plugin {} {m}\n", p.name));
            }
        }
        for problem in self.problems() {
            out.push_str(&format!("Problem: {problem}\n"));
        }
        out
    }
}

async fn table_exists(conn: &mut PgConnection, table: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(conn)
            .await?,
    )
}

async fn kernel_report(conn: &mut PgConnection, migrator: &Migrator) -> Result<KernelReport> {
    let rows = if table_exists(conn, "_sqlx_migrations").await? {
        sqlx::query(
            "SELECT version, description, success, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&mut *conn)
        .await
        .context("failed to read _sqlx_migrations")?
    } else {
        Vec::new()
    };

    let mut report = KernelReport::default();
    let mut applied: HashMap<i64, Vec<u8>> = HashMap::new();
    for row in &rows {
        let version: i64 = row.get("version");
        let description: String = row.get("description");
        if !row.get::<bool, _>("success") {
            report.problems.push(format!(
                "migration {version} ({description}) is marked failed (dirty); fix it by hand"
            ));
        }
        applied.insert(version, row.get("checksum"));
        if !migrator.iter().any(|m| m.version == version) {
            if is_incompatible_description(&description) {
                report.problems.push(format!(
                    "migration {version} ({description}) was applied by a newer release \
                     and is marked incompatible with this one"
                ));
            }
            report.unknown.push((version, description));
        }
    }
    for m in migrator
        .iter()
        .filter(|m| m.migration_type.is_up_migration())
    {
        match applied.get(&m.version) {
            Some(checksum) => {
                report.applied += 1;
                if checksum.as_slice() != m.checksum.as_ref() {
                    report.problems.push(format!(
                        "migration {} ({}) was edited after it was applied",
                        m.version, m.description
                    ));
                }
            }
            None => report.pending.push((m.version, m.description.to_string())),
        }
    }
    Ok(report)
}

async fn applied_plugin_migrations(
    conn: &mut PgConnection,
) -> Result<HashMap<String, HashSet<String>>> {
    let mut by_plugin: HashMap<String, HashSet<String>> = HashMap::new();
    if !table_exists(conn, "plugin_migration").await? {
        return Ok(by_plugin);
    }
    let rows = sqlx::query("SELECT plugin, migration FROM plugin_migration")
        .fetch_all(conn)
        .await
        .context("failed to read plugin_migration")?;
    for row in rows {
        by_plugin
            .entry(row.get("plugin"))
            .or_default()
            .insert(row.get("migration"));
    }
    Ok(by_plugin)
}

fn plugin_report(name: &str, info: &PluginInfo, applied: Option<&HashSet<String>>) -> PluginReport {
    let empty = HashSet::new();
    let applied = applied.unwrap_or(&empty);
    let listed: HashSet<&str> = info.migrations.files.iter().map(String::as_str).collect();
    let mut report = PluginReport {
        name: name.to_string(),
        ..Default::default()
    };
    for file in &info.migrations.files {
        if applied.contains(file) {
            report.applied += 1;
        } else {
            report.pending.push(file.clone());
        }
    }
    let mut unknown: Vec<&String> = applied
        .iter()
        .filter(|m| !listed.contains(m.as_str()))
        .collect();
    unknown.sort();
    for m in unknown {
        if is_incompatible_name(m) {
            report.problems.push(format!(
                "migration {m} was applied by a newer release and is marked incompatible with this one"
            ));
        }
        report.unknown.push(m.clone());
    }
    report
}

/// The plugins `serve` would run, and so the plugins whose migrations it needs:
/// those enabled in `plugin_status`, plus discovered plugins not yet recorded
/// there that `serve` will auto-enable. Writes nothing.
pub async fn plugins_to_migrate(
    conn: &mut PgConnection,
    plugins_dirs: &[PathBuf],
    disabled_plugins: &[String],
) -> Result<PluginSet> {
    let discovered = crate::plugin::PluginRuntime::discover_plugins(plugins_dirs).await;
    let recorded: HashMap<String, i16> = if table_exists(conn, "plugin_status").await? {
        sqlx::query("SELECT name, status FROM plugin_status")
            .fetch_all(conn)
            .await
            .context("failed to read plugin_status")?
            .into_iter()
            .map(|r| (r.get("name"), r.get("status")))
            .collect()
    } else {
        HashMap::new()
    };
    Ok(discovered
        .into_iter()
        .filter(|(name, (info, _))| match recorded.get(name) {
            Some(status) => *status == crate::plugin::status::STATUS_ENABLED,
            None => crate::plugin::gate::should_auto_enable(
                info.default_enabled,
                disabled_plugins,
                name,
            ),
        })
        .collect())
}

/// What a [`run`] applied.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Kernel migrations applied.
    pub kernel: usize,
    /// Plugin migrations applied.
    pub plugins: usize,
}

/// Apply the kernel's pending migrations on the lock's connection.
///
/// Refuses, changing nothing, when the schema has a problem this binary cannot
/// fix: a dirty or edited migration, or a newer one marked incompatible.
/// Unknown newer migrations are otherwise accepted.
pub async fn run_kernel(lock: &mut MigrationLock, migrator: &Migrator) -> Result<usize> {
    let report = kernel_report(lock.conn(), migrator).await?;
    if !report.problems.is_empty() {
        bail!(
            "the database schema cannot be migrated by this binary:\n  {}",
            report.problems.join("\n  ")
        );
    }
    if report.pending.is_empty() {
        return Ok(0);
    }
    // A `CREATE INDEX CONCURRENTLY` waits, by design, for every older
    // transaction, and `lock_timeout` would cancel it and leave an INVALID
    // index. A run that includes a no-transaction migration waits instead.
    let pending: HashSet<i64> = report.pending.iter().map(|(v, _)| *v).collect();
    if migrator
        .iter()
        .any(|m| m.no_tx && pending.contains(&m.version))
    {
        sqlx::query("SET lock_timeout = 0")
            .execute(lock.conn())
            .await
            .context("failed to clear lock_timeout")?;
        info!("a pending migration runs outside a transaction; lock_timeout cleared for this run");
    }
    // Our own check above has vetted every unknown migration, so sqlx's blanket
    // refusal of them is switched off.
    let mut tolerant = Migrator::with_migrations(migrator.iter().cloned().collect());
    tolerant.set_ignore_missing(true);
    // `run_direct` rather than `run`: `run` is generic over `Acquire`, which on
    // a borrowed connection makes every caller's future fail the `Send`
    // check (sqlx documents `run_direct` as the way around exactly that).
    tolerant
        .run_direct(None, lock.conn(), false)
        .await
        .context("failed to run kernel migrations")?;
    for (version, description) in &report.pending {
        info!(version, description = %description, "applied kernel migration");
    }
    Ok(report.pending.len())
}

/// Apply the pending plugin migrations of `plugins` on the lock's connection.
/// Refuses, changing nothing, when an applied plugin migration this binary
/// does not know is named incompatible. Returns the files applied, by plugin.
pub async fn run_plugins(
    lock: &mut MigrationLock,
    plugins: &PluginSet,
) -> Result<HashMap<String, Vec<String>>> {
    refuse_plugin_problems(
        lock.conn(),
        plugins
            .iter()
            .map(|(name, (info, _))| (name.as_str(), info)),
    )
    .await?;
    let results = crate::plugin::migration::run_all_pending_migrations(lock.conn(), plugins)
        .await
        .context("failed to run plugin migrations")?;
    for (plugin, files) in &results {
        info!(plugin = %plugin, count = files.len(), "applied plugin migrations");
    }
    Ok(results)
}

/// Fail, before anything is applied, when any of `plugins` has an applied
/// migration this binary does not know that is named incompatible. Every path
/// that applies plugin migrations calls this first.
pub(crate) async fn refuse_plugin_problems<'a>(
    conn: &mut PgConnection,
    plugins: impl IntoIterator<Item = (&'a str, &'a PluginInfo)>,
) -> Result<()> {
    let applied = applied_plugin_migrations(conn).await?;
    let problems: Vec<String> = plugins
        .into_iter()
        .flat_map(|(name, info)| {
            plugin_report(name, info, applied.get(name))
                .problems
                .into_iter()
                .map(move |p| format!("plugin {name}: {p}"))
        })
        .collect();
    if !problems.is_empty() {
        bail!(
            "the database schema cannot be migrated by this binary:\n  {}",
            problems.join("\n  ")
        );
    }
    Ok(())
}

/// Kernel then plugin migrations, all under one hold of the lock.
pub async fn run(
    lock: &mut MigrationLock,
    migrator: &Migrator,
    plugins_dirs: &[PathBuf],
    disabled_plugins: &[String],
) -> Result<Applied> {
    // Plugin problems first, so a refusal changes nothing, kernel half
    // included. (On a fresh database there is nothing applied to refuse.)
    let before = plugins_to_migrate(lock.conn(), plugins_dirs, disabled_plugins).await?;
    refuse_plugin_problems(
        lock.conn(),
        before.iter().map(|(name, (info, _))| (name.as_str(), info)),
    )
    .await?;
    let kernel = run_kernel(lock, migrator).await?;
    // Read after the kernel half: on a fresh database `plugin_status` exists
    // only now.
    let plugins = plugins_to_migrate(lock.conn(), plugins_dirs, disabled_plugins).await?;
    let plugins = run_plugins(lock, &plugins)
        .await?
        .values()
        .map(Vec::len)
        .sum();
    Ok(Applied { kernel, plugins })
}

/// Read the schema state for `serve` with `MIGRATE_ON_START=false`.
pub async fn read_report(
    database_url: &str,
    migrator: &Migrator,
    plugins_dirs: &[PathBuf],
    disabled_plugins: &[String],
) -> Result<Report> {
    let mut conn = PgConnection::connect(database_url)
        .await
        .context("failed to connect to PostgreSQL")?;
    let plugins = plugins_to_migrate(&mut conn, plugins_dirs, disabled_plugins).await?;
    let report = Report::read(&mut conn, migrator, &plugins).await;
    let _ = conn.close().await;
    report
}

#[cfg(test)]
mod tests {
    /// `AppState::new` awaits these, and every test spawns `AppState::new` on a
    /// multi-threaded runtime, so they must stay `Send`. sqlx's generic
    /// `Acquire` breaks that silently for every caller; this fails to compile
    /// instead.
    #[allow(dead_code)]
    fn migration_futures_are_send(
        pool: &'static sqlx::PgPool,
        lock: &'static mut super::MigrationLock,
    ) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(crate::db::run_migrations(pool));
        assert_send(super::MigrationLock::acquire(pool));
        assert_send(super::run_kernel(lock, crate::db::migrator()));
    }

    #[allow(dead_code)]
    fn plugin_migration_futures_are_send(
        lock: &'static mut super::MigrationLock,
        plugins: &'static super::PluginSet,
    ) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(super::run_plugins(lock, plugins));
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn an_unknown_plugin_migration_is_a_problem_only_when_named_incompatible() {
        let info: crate::plugin::PluginInfo = toml::from_str(
            r#"
name = "p"
description = "p"
version = "1.0.0"
[migrations]
files = ["migrations/001_a.sql"]
"#,
        )
        .unwrap();
        let applied: std::collections::HashSet<String> = [
            "migrations/001_a.sql",
            "migrations/002_newer.sql",
            "migrations/003_incompatible_drop_x.sql",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let report = super::plugin_report("p", &info, Some(&applied));
        assert_eq!(report.applied, 1);
        assert!(report.pending.is_empty());
        assert_eq!(report.unknown.len(), 2, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(report.problems[0].contains("003_incompatible_drop_x"));

        let nothing_applied = super::plugin_report("p", &info, None);
        assert_eq!(nothing_applied.pending, vec!["migrations/001_a.sql"]);
        assert!(nothing_applied.problems.is_empty());
    }
}
