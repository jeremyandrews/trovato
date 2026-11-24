#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Upgrades with no downtime (`docs/design/upgrades-and-probes.md`).
//!
//! Every test here drives the real `trovato` binary as a child process, against
//! a scratch database of its own, because what is under test is process
//! behaviour: two migrators racing, a server that waits for its schema instead
//! of exiting, an old binary on a newer schema, and what a pod does between
//! SIGTERM and exit.
//!
//! The file is named to sort after every other kernel test target, so adding it
//! does not move any existing target to a different CI shard
//! (`scripts/ci-test-shard.sh` partitions by sorted index).
//!
//! `previous_release_serves_on_this_schema` is the CI `rollback` job's driver
//! and skips itself unless `TROVATO_PREVIOUS_BIN` names the previous binary.
//!
//! Requires Postgres and Redis; runs in CI.

use std::net::TcpListener as StdListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use sqlx::{Connection, PgConnection, PgPool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use trovato_test_utils::ScratchDb;

/// The binary this branch builds.
fn this_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trovato"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// How many kernel migrations this binary embeds.
fn kernel_migration_count() -> i64 {
    std::fs::read_dir(repo_root().join("crates/kernel/migrations"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "sql")
        })
        .count() as i64
}

fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A directory under the system temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("trovato-{label}-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A command for `bin` with a clean environment: only what is set here, so the
/// developer's `.env` and the suite's shared `DATABASE_URL` cannot leak in.
fn command(bin: &Path, database_url: &str, work: &TempDir) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .current_dir(&work.0)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("DATABASE_URL", database_url)
        .env("REDIS_URL", trovato_test_utils::env::redis_url())
        .env("DATABASE_MAX_CONNECTIONS", "5")
        .env("RUST_LOG", "warn")
        .env("TEMPLATES_DIR", repo_root().join("templates"))
        .env("STATIC_DIR", repo_root().join("static"))
        .env("UPLOADS_DIR", work.0.join("uploads"))
        .env("THEMES_DIR", work.0.join("themes"))
        // No plugins by default: these tests are about the kernel's process
        // behaviour, and compiling plugin modules would only slow them down.
        .env("PLUGINS_DIR", work.0.join("plugins"))
        .env("COOKIE_SECURE", "never")
        .kill_on_drop(true);
    cmd
}

/// A running `trovato serve`.
struct Server {
    child: Child,
    base: String,
    /// Everything the server has logged so far (its stdout).
    log: std::sync::Arc<parking_lot::Mutex<String>>,
    _work: TempDir,
}

impl Server {
    async fn start(bin: &Path, database_url: &str, env: &[(&str, &str)]) -> Self {
        let work = TempDir::new("serve");
        let port = free_port();
        let mut cmd = command(bin, database_url, &work);
        cmd.arg("serve")
            .env("PORT", port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn trovato serve");
        let log = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
        let stdout = child.stdout.take().unwrap();
        let sink = log.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut log = sink.lock();
                log.push_str(&line);
                log.push('\n');
            }
        });
        Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
            log,
            _work: work,
        }
    }

    fn logged(&self, needle: &str) -> bool {
        self.log.lock().contains(needle)
    }

    /// Wait until the server has logged `needle`, or panic after `within`.
    async fn wait_for_log(&self, needle: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.logged(needle) {
            assert!(
                Instant::now() < deadline,
                "no {needle:?} logged within {within:?}:\n{}",
                self.log.lock()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Status of `GET path`, or `None` when nothing answers.
    async fn status(&self, path: &str) -> Option<u16> {
        self.get(path, &[]).await.map(|(s, _)| s)
    }

    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> Option<(u16, String)> {
        let mut request = http().get(format!("{}{path}", self.base));
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let response = request.send().await.ok()?;
        let status = response.status().as_u16();
        Some((status, response.text().await.unwrap_or_default()))
    }

    /// Wait until `path` answers `status`, or panic after `within`.
    async fn wait_for(&mut self, path: &str, status: u16, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            // Exit first: a port another process took over must not answer for
            // a server that has already died.
            if let Ok(Some(exit)) = self.child.try_wait() {
                panic!("trovato serve exited ({exit}) before {path} answered {status}");
            }
            if self.status(path).await == Some(status) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{path} did not answer {status} within {within:?} (last: {:?})",
                self.status(path).await
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn sigterm(&self) {
        self.signal("-TERM");
    }

    fn signal(&self, signal: &str) {
        let pid = self.child.id().expect("server still running");
        let ok = std::process::Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill {signal} {pid}");
    }

    /// Wait for the process to exit, or panic after `within`.
    async fn exit_within(&mut self, within: Duration) -> std::process::ExitStatus {
        tokio::time::timeout(within, self.child.wait())
            .await
            .unwrap_or_else(|_| panic!("trovato serve still running after {within:?}"))
            .unwrap()
    }

    async fn stop(mut self) {
        self.sigterm();
        let _ = tokio::time::timeout(Duration::from_secs(60), self.child.wait()).await;
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Run `trovato migrate <args>` to completion: (success, stdout, stderr).
async fn migrate(
    bin: &Path,
    database_url: &str,
    args: &[&str],
    plugins: Option<&Path>,
) -> (bool, String, String) {
    let work = TempDir::new("migrate");
    let mut cmd = command(bin, database_url, &work);
    cmd.arg("migrate").args(args);
    if let Some(dir) = plugins {
        cmd.env("PLUGINS_DIR", dir);
    }
    let out = cmd.output().await.expect("run trovato migrate");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        eprintln!(
            "trovato migrate {args:?} failed ({}):\n{stdout}\n{stderr}",
            out.status
        );
    }
    (out.status.success(), stdout, stderr)
}

/// Advisory locks in `database_url`'s database, seen from a connection of
/// the test's own: (held, waited for). A migrator waits by polling
/// `pg_try_advisory_lock`, so a waiter is a backend whose last statement was
/// that try and which holds no advisory lock (or one blocked on a lock).
async fn advisory_locks(database_url: &str) -> (i64, i64) {
    let mut conn = PgConnection::connect(database_url).await.unwrap();
    let row: (i64, i64) = sqlx::query_as(
        "WITH here AS (SELECT oid FROM pg_database WHERE datname = current_database()), \
              holders AS (SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND granted \
                          AND database = (SELECT oid FROM here)) \
         SELECT (SELECT count(*) FROM holders), \
                (SELECT count(*) FROM pg_stat_activity a \
                  WHERE a.datname = current_database() AND a.pid <> pg_backend_pid() \
                    AND a.query LIKE '%pg_try_advisory_lock%' \
                    AND a.pid NOT IN (SELECT pid FROM holders)) \
              + (SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND NOT granted \
                  AND database = (SELECT oid FROM here))",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let _ = conn.close().await;
    row
}

/// Poll `advisory_locks` until `want` holds, or panic after `within`.
async fn wait_for_locks(database_url: &str, want: impl Fn((i64, i64)) -> bool, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let seen = advisory_locks(database_url).await;
        if want(seen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "advisory locks (held, waiting): {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn applied_versions(pool: &PgPool) -> Option<i64> {
    let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
        .fetch_one(pool)
        .await
        .unwrap();
    exists?;
    Some(
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await
            .unwrap(),
    )
}

/// The full kernel schema, migrated through the library, so a test about the
/// server does not depend on `trovato migrate`.
async fn migrated(db: &ScratchDb) {
    trovato_kernel::db::run_migrations(db.pool())
        .await
        .expect("migrate the scratch database");
}

/// Record a kernel migration this binary does not know, as a newer release's
/// `trovato migrate` would.
async fn add_unknown_migration(pool: &PgPool, description: &str) {
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (29991231000001, $1, true, '\\x00'::bytea, 0)",
    )
    .bind(description)
    .execute(pool)
    .await
    .unwrap();
}

/// The numbers in `Applied <k> kernel and <p> plugin migration(s).`.
fn applied_counts(stdout: &str) -> (i64, i64) {
    let line = stdout
        .lines()
        .find(|l| l.starts_with("Applied "))
        .unwrap_or_else(|| panic!("no summary line in:\n{stdout}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    (words[1].parse().unwrap(), words[4].parse().unwrap())
}

// =============================================================================
// trovato migrate
// =============================================================================

/// Two migrators started together, as two Jobs or a Job and a pod with
/// `MIGRATE_ON_START` left on would be: both succeed, and between them every
/// kernel and plugin migration is applied exactly once.
#[tokio::test(flavor = "multi_thread")]
async fn two_migrators_at_once_both_succeed_and_apply_each_migration_once() {
    let db = ScratchDb::create("bg10_race").await;
    let url = db.url();
    let plugins = repo_root().join("plugins");

    // Hold the lock first, so both are certainly started and waiting on it
    // together rather than one finishing before the other begins.
    let holder = trovato_kernel::migrate::MigrationLock::acquire(db.pool())
        .await
        .unwrap();
    let bin = this_bin();
    let work_a = TempDir::new("race-a");
    let work_b = TempDir::new("race-b");
    let spawn = |work: &TempDir| {
        let mut cmd = command(&bin, &url, work);
        // A lock timeout shorter than the wait: it must apply to the
        // migrations only, never to the wait for the other migrator.
        cmd.args(["migrate", "--lock-timeout", "1"])
            .env("PLUGINS_DIR", &plugins)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn().expect("spawn trovato migrate")
    };
    let mut child_a = spawn(&work_a);
    let mut child_b = spawn(&work_b);
    // Both are waiting on the lock this test holds ...
    wait_for_locks(
        &url,
        |(held, waiting)| held == 1 && waiting == 2,
        Duration::from_secs(60),
    )
    .await;
    // ... and keep waiting past their one-second lock timeout.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        child_a.try_wait().unwrap().is_none(),
        "a waits for the lock"
    );
    assert!(
        child_b.try_wait().unwrap().is_none(),
        "b waits for the lock"
    );
    assert_eq!(
        applied_versions(db.pool()).await,
        None,
        "nothing ran while the lock was held"
    );
    holder.release().await.unwrap();

    let finish = |child: Child| async move {
        let out = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
            .await
            .expect("migrate finishes once the lock is free")
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if !out.status.success() {
            eprintln!("{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        }
        (out.status.success(), stdout)
    };
    let (a, b) = tokio::join!(finish(child_a), finish(child_b));
    assert!(a.0 && b.0, "both migrators must succeed");

    let (ka, pa) = applied_counts(&a.1);
    let (kb, pb) = applied_counts(&b.1);
    assert_eq!(
        ka + kb,
        kernel_migration_count(),
        "each kernel migration applied by exactly one of the two"
    );
    let plugin_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_migration")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(plugin_rows > 0, "the in-tree plugins have migrations");
    assert_eq!(pa + pb, plugin_rows, "each plugin migration applied once");
    assert!(
        (ka == 0 && pa == 0) || (kb == 0 && pb == 0),
        "the lock serializes the two runs: one finds nothing to do ({a:?} / {b:?})"
    );

    // Idempotent: a third run applies nothing and succeeds.
    let c = migrate(&this_bin(), &url, &[], Some(&plugins)).await;
    assert!(c.0);
    assert_eq!(applied_counts(&c.1), (0, 0));

    // And `--status` reports nothing pending.
    let status = migrate(&this_bin(), &url, &["--status"], Some(&plugins)).await;
    assert!(status.0);
    assert!(
        status.1.contains("Pending: 0"),
        "status after a full run:\n{}",
        status.1
    );
}

/// A migrator waiting for the lock holds no snapshot, so the holder's
/// `CREATE INDEX CONCURRENTLY` (which waits for every older snapshot) does
/// not deadlock against it.
#[tokio::test(flavor = "multi_thread")]
async fn a_waiting_migrator_does_not_block_a_concurrent_index_build() {
    let db = ScratchDb::create("bg10_cic").await;
    let url = db.url();
    sqlx::query("CREATE TABLE bg10_cic (x INT)")
        .execute(db.pool())
        .await
        .unwrap();
    let mut holder = trovato_kernel::migrate::MigrationLock::connect(&url)
        .await
        .unwrap();
    let work = TempDir::new("cic");
    let waiter = command(&this_bin(), &url, &work)
        .arg("migrate")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_locks(
        &url,
        |(held, waiting)| held == 1 && waiting == 1,
        Duration::from_secs(60),
    )
    .await;

    tokio::time::timeout(
        Duration::from_secs(30),
        sqlx::query("CREATE INDEX CONCURRENTLY bg10_cic_x ON bg10_cic (x)").execute(holder.conn()),
    )
    .await
    .expect("the index build must not wait on the waiting migrator")
    .expect("the index build must not deadlock");
    holder.release().await.unwrap();

    let out = tokio::time::timeout(Duration::from_secs(300), waiter.wait_with_output())
        .await
        .expect("the waiter runs once the lock is free")
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--dry-run` and `--status` change nothing.
#[tokio::test(flavor = "multi_thread")]
async fn dry_run_and_status_change_nothing() {
    let db = ScratchDb::create("bg10_dry").await;
    let url = db.url();

    let dry = migrate(&this_bin(), &url, &["--dry-run"], None).await;
    assert!(dry.0);
    assert!(dry.1.contains("Would apply"), "dry run output:\n{}", dry.1);
    let status = migrate(&this_bin(), &url, &["--status"], None).await;
    assert!(status.0);
    assert!(
        status
            .1
            .contains(&format!("Pending: {}", kernel_migration_count())),
        "status output:\n{}",
        status.1
    );
    assert_eq!(applied_versions(db.pool()).await.unwrap_or(0), 0);
}

/// A run that fails part way through sqlx's own run still releases the lock,
/// both ours and sqlx's: no advisory lock is left in the database, and the
/// next migrator takes ours at once on a connection of its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_run_releases_the_lock() {
    let db = ScratchDb::create("bg10_lockfail").await;
    // The first kernel migration creates `users`; a table of that name makes
    // it fail inside the migrator, after the lock is taken.
    sqlx::query("CREATE TABLE users (x INT)")
        .execute(db.pool())
        .await
        .unwrap();
    let no_locks = |(held, waiting): (i64, i64)| held == 0 && waiting == 0;

    let err = trovato_kernel::db::run_migrations(db.pool())
        .await
        .expect_err("the first migration must fail");
    assert!(format!("{err:#}").contains("users"), "{err:#}");
    wait_for_locks(&db.url(), no_locks, Duration::from_secs(5)).await;

    let (ok, _, stderr) = migrate(&this_bin(), &db.url(), &[], None).await;
    assert!(!ok, "trovato migrate must fail too");
    assert!(stderr.contains("users"), "{stderr}");
    wait_for_locks(&db.url(), no_locks, Duration::from_secs(5)).await;

    let lock = tokio::time::timeout(
        Duration::from_secs(5),
        trovato_kernel::migrate::MigrationLock::connect(&db.url()),
    )
    .await
    .expect("the lock is free after a failed run")
    .unwrap();
    lock.release().await.unwrap();
}

/// `--lock-timeout` bounds how long a migration statement waits for a table
/// lock, on the connection the migrations run on: a long query holding the
/// table makes the run fail quickly and change nothing, while `0` waits.
#[tokio::test(flavor = "multi_thread")]
async fn lock_timeout_applies_to_the_migrations() {
    let db = ScratchDb::create("bg10_lock_timeout").await;
    let url = db.url();
    migrated(&db).await;
    let before = applied_versions(db.pool()).await;

    // A long transaction holding the table every kernel run reads first.
    let mut blocker = PgConnection::connect(&url).await.unwrap();
    let mut tx = blocker.begin().await.unwrap();
    sqlx::query("LOCK TABLE _sqlx_migrations IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .unwrap();

    let started = Instant::now();
    let (ok, _, stderr) = tokio::time::timeout(
        Duration::from_secs(30),
        migrate(&this_bin(), &url, &["--lock-timeout", "1"], None),
    )
    .await
    .expect("--lock-timeout 1 must not wait for the blocker");
    assert!(!ok, "the run must fail on the lock timeout");
    assert!(stderr.contains("lock timeout"), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(15));

    // `0` waits for the blocker instead, then succeeds.
    let work = TempDir::new("lock-timeout-0");
    let mut waiting = command(&this_bin(), &url, &work)
        .args(["migrate", "--lock-timeout", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        waiting.try_wait().unwrap().is_none(),
        "--lock-timeout 0 waits"
    );
    tx.rollback().await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(60), waiting.wait())
        .await
        .expect("finishes once the blocker is gone")
        .unwrap();
    assert!(exit.success());
    assert_eq!(applied_versions(db.pool()).await, before);
}

/// `--status` and `--dry-run` take no lock, so they never wait for, or block,
/// a migration in progress.
#[tokio::test(flavor = "multi_thread")]
async fn status_and_dry_run_take_no_lock() {
    let db = ScratchDb::create("bg10_status_nolock").await;
    let url = db.url();
    let holder = trovato_kernel::migrate::MigrationLock::acquire(db.pool())
        .await
        .unwrap();
    for args in [["--status"], ["--dry-run"]] {
        let (ok, _, _) = tokio::time::timeout(
            Duration::from_secs(10),
            migrate(&this_bin(), &url, &args, None),
        )
        .await
        .unwrap_or_else(|_| panic!("{args:?} waited for the migration lock"));
        assert!(ok, "{args:?}");
    }
    assert_eq!(advisory_locks(&url).await, (1, 0), "only the test's lock");
    holder.release().await.unwrap();
}

/// `trovato migrate` needs Postgres only: it runs with Redis unreachable.
#[tokio::test(flavor = "multi_thread")]
async fn migrate_needs_no_redis() {
    let db = ScratchDb::create("bg10_no_redis").await;
    let work = TempDir::new("no-redis");
    let out = command(&this_bin(), &db.url(), &work)
        .arg("migrate")
        .env("REDIS_URL", format!("redis://127.0.0.1:{}", free_port()))
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        applied_versions(db.pool()).await,
        Some(kernel_migration_count())
    );
}

/// The plugins `trovato migrate` runs are the ones `serve` would load: those
/// enabled in `plugin_status`, plus unrecorded ones it would auto-enable, not
/// those in `DISABLED_PLUGINS` or recorded as disabled. It records no status.
#[tokio::test(flavor = "multi_thread")]
async fn migrate_runs_only_the_plugins_serve_would_load() {
    let db = ScratchDb::create("bg10_which_plugins").await;
    let url = db.url();
    migrated(&db).await;
    let plugins = repo_root().join("plugins");
    let discovered = trovato_kernel::plugin::PluginRuntime::discover_plugins_in(&plugins).await;
    // Two auto-enabled plugins with migrations that nothing depends on, so
    // leaving them out leaves every other plugin's dependencies whole.
    let mut leaves: Vec<&String> = discovered
        .iter()
        .filter(|(name, (info, _))| {
            info.default_enabled
                && !info.migrations.files.is_empty()
                && !discovered
                    .values()
                    .any(|(other, _)| other.dependencies.contains(name))
        })
        .map(|(name, _)| name)
        .collect();
    leaves.sort();
    assert!(
        leaves.len() >= 2,
        "two leaf plugins with migrations: {leaves:?}"
    );
    let (by_env, by_status) = (leaves[0].as_str(), leaves[1].as_str());
    sqlx::query(
        "INSERT INTO plugin_status (name, status, version, installed_at, updated_at) \
         VALUES ($1, 0, '1.0.0', 0, 0)",
    )
    .bind(by_status)
    .execute(db.pool())
    .await
    .unwrap();

    let work = TempDir::new("which-plugins");
    let out = command(&this_bin(), &url, &work)
        .arg("migrate")
        .env("PLUGINS_DIR", &plugins)
        .env("DISABLED_PLUGINS", by_env)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rows_for = |name: &str| {
        let name = name.to_string();
        let pool = db.pool().clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_migration WHERE plugin = $1")
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(rows_for(by_env).await, 0, "{by_env} is in DISABLED_PLUGINS");
    assert_eq!(
        rows_for(by_status).await,
        0,
        "{by_status} is recorded disabled"
    );
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_migration")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(total > 0, "the other plugins were migrated");
    let statuses: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_status")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(statuses, 1, "trovato migrate records no plugin status");
}

/// The server's own boot path (`db::run_migrations`, which `serve` runs with
/// `MIGRATE_ON_START` on) takes the same lock, so it waits for a running
/// `trovato migrate` instead of racing it.
#[tokio::test(flavor = "multi_thread")]
async fn the_boot_path_waits_for_the_migration_lock() {
    let db = ScratchDb::create("bg10_boot_lock").await;
    let holder = trovato_kernel::migrate::MigrationLock::acquire(db.pool())
        .await
        .unwrap();
    let pool = db.pool().clone();
    let boot = tokio::spawn(async move { trovato_kernel::db::run_migrations(&pool).await });
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!boot.is_finished(), "the boot path must wait for the lock");
    assert_eq!(applied_versions(db.pool()).await, None);
    holder.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(120), boot)
        .await
        .expect("finishes once the lock is free")
        .unwrap()
        .unwrap();
    assert_eq!(
        applied_versions(db.pool()).await,
        Some(kernel_migration_count())
    );
}

/// A newer plugin migration named incompatible stops `trovato migrate`, shows
/// as a problem in `--status`, and stops `plugin migrate <name>` too.
#[tokio::test(flavor = "multi_thread")]
async fn a_newer_incompatible_plugin_migration_is_refused_everywhere() {
    let db = ScratchDb::create("bg10_plugin_incompat_cli").await;
    let url = db.url();
    let plugins = repo_root().join("plugins");
    let (ok, _, _) = migrate(&this_bin(), &url, &[], Some(&plugins)).await;
    assert!(ok, "the first full migration");
    let plugin: String =
        sqlx::query_scalar("SELECT plugin FROM plugin_migration ORDER BY plugin LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO plugin_migration (plugin, migration, applied_at) \
         VALUES ($1, 'migrations/999_incompatible_drop_legacy.sql', 0)",
    )
    .bind(&plugin)
    .execute(db.pool())
    .await
    .unwrap();

    let (ok, _, stderr) = migrate(&this_bin(), &url, &[], Some(&plugins)).await;
    assert!(!ok, "trovato migrate must refuse");
    assert!(stderr.contains("marked incompatible"), "{stderr}");

    let (ok, status, _) = migrate(&this_bin(), &url, &["--status"], Some(&plugins)).await;
    assert!(ok, "--status reads the state whatever it says");
    assert!(
        status.contains(&format!("Problem: plugin {plugin}:")),
        "{status}"
    );

    let work = TempDir::new("plugin-migrate");
    let out = command(&this_bin(), &url, &work)
        .args(["plugin", "migrate", &plugin])
        .env("PLUGINS_DIR", &plugins)
        .output()
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "plugin migrate {plugin} must refuse");
    assert!(stderr.contains("marked incompatible"), "{stderr}");
}

/// Every path that applies one plugin's migrations (`plugin install <name>`,
/// `plugin migrate <name>`) refuses a newer migration of that plugin named
/// incompatible, as `trovato migrate` does, and applies nothing.
#[tokio::test(flavor = "multi_thread")]
async fn single_plugin_migration_refuses_a_newer_incompatible_migration() {
    let db = ScratchDb::create("bg10_plugin_incompat").await;
    migrated(&db).await;
    let discovered =
        trovato_kernel::plugin::PluginRuntime::discover_plugins_in(&repo_root().join("plugins"))
            .await;
    let (name, (info, dir)) = discovered
        .iter()
        .find(|(_, (info, _))| !info.migrations.files.is_empty())
        .expect("an in-tree plugin with migrations");
    sqlx::query(
        "INSERT INTO plugin_migration (plugin, migration, applied_at) \
         VALUES ($1, '999_incompatible_drop_legacy.sql', 0)",
    )
    .bind(name)
    .execute(db.pool())
    .await
    .unwrap();

    let err = trovato_kernel::plugin::migration::run_plugin_migrations(db.pool(), name, info, dir)
        .await
        .expect_err("an incompatible newer plugin migration must be refused");
    assert!(format!("{err:#}").contains("incompatible"), "{err:#}");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_migration WHERE plugin = $1")
        .bind(name)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "nothing applied for {name}");
}

// =============================================================================
// MIGRATE_ON_START=false
// =============================================================================

/// The other subcommands do not migrate either with `MIGRATE_ON_START=false`:
/// on a schema that is behind they refuse, say why, and change nothing.
#[tokio::test(flavor = "multi_thread")]
async fn cli_commands_refuse_a_schema_that_is_behind() {
    let db = ScratchDb::create("bg10_cli_off").await;
    let work = TempDir::new("cli-off");
    let out = command(&this_bin(), &db.url(), &work)
        .args(["user", "roles", "nobody"])
        .env("MIGRATE_ON_START", "false")
        .output()
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "must refuse: {stderr}");
    assert!(
        stderr.contains("run `trovato migrate` first"),
        "says why: {stderr}"
    );
    assert_eq!(applied_versions(db.pool()).await, None, "schema untouched");
}

/// With `MIGRATE_ON_START=false`, a server on a schema that is behind waits:
/// alive, not ready, schema untouched. It does not exit (no crash loop), and it
/// becomes ready by itself once `trovato migrate` has run.
#[tokio::test(flavor = "multi_thread")]
async fn serve_waits_for_pending_migrations_without_changing_the_schema() {
    let db = ScratchDb::create("bg10_wait").await;
    let url = db.url();

    let mut server = Server::start(&this_bin(), &url, &[("MIGRATE_ON_START", "false")]).await;
    server
        .wait_for("/health/live", 200, Duration::from_secs(60))
        .await;
    assert_eq!(server.status("/health/ready").await, Some(503));
    assert_eq!(server.status("/health").await, Some(503));

    // Long enough for several of the waiting server's checks.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "serve must wait, not exit"
    );
    assert_eq!(server.status("/health/live").await, Some(200));
    assert_eq!(server.status("/health/ready").await, Some(503));
    assert_eq!(
        applied_versions(db.pool()).await.unwrap_or(0),
        0,
        "serve with MIGRATE_ON_START=false must not migrate"
    );
    let tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        tables, 0,
        "serve with MIGRATE_ON_START=false created tables"
    );

    // The migration Job runs; the waiting pod becomes ready on its own, and
    // liveness never drops while it hands its socket to the real server.
    assert!(migrate(&this_bin(), &url, &[], None).await.0);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert_eq!(
            server.status("/health/live").await,
            Some(200),
            "liveness during the hand-over"
        );
        if server.status("/health/ready").await == Some(200) {
            break;
        }
        assert!(Instant::now() < deadline, "not ready within 60s of migrate");
        assert!(server.child.try_wait().unwrap().is_none(), "serve exited");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(server.status("/health/live").await, Some(200));
    assert_eq!(server.status("/health").await, Some(200));
    server.stop().await;
}

/// Pending plugin migrations keep a `MIGRATE_ON_START=false` server not
/// ready, as kernel ones do, and it applies none of them itself.
#[tokio::test(flavor = "multi_thread")]
async fn serve_waits_for_pending_plugin_migrations() {
    let db = ScratchDb::create("bg10_wait_plugins").await;
    let url = db.url();
    migrated(&db).await;
    let plugins = repo_root().join("plugins");
    let plugins_dir = plugins.to_str().unwrap();

    let (ok, status, _) = migrate(&this_bin(), &url, &["--status"], Some(&plugins)).await;
    assert!(ok);
    assert!(
        !status.contains("Pending: 0"),
        "the in-tree plugins have pending migrations:\n{status}"
    );

    let mut server = Server::start(
        &this_bin(),
        &url,
        &[
            ("MIGRATE_ON_START", "false"),
            ("PLUGINS_DIR", plugins_dir),
            ("RUST_LOG", "info"),
        ],
    )
    .await;
    server
        .wait_for("/health/live", 200, Duration::from_secs(60))
        .await;
    // Not ready because of the plugin migrations, not because it is still
    // building: it says so, and never gets as far as the real server.
    server
        .wait_for_log("migration(s) pending", Duration::from_secs(30))
        .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(server.status("/health/ready").await, Some(503));
    assert!(
        !server.logged("Server listening"),
        "built the application with plugin migrations pending"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_migration")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "serve applied plugin migrations");

    assert!(migrate(&this_bin(), &url, &[], Some(&plugins)).await.0);
    server
        .wait_for("/health/ready", 200, Duration::from_secs(180))
        .await;
    server.stop().await;
}

/// SIGTERM while waiting for the schema is a clean exit, not the default
/// action of a signal nobody handles.
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_while_waiting_for_the_schema_exits_cleanly() {
    let db = ScratchDb::create("bg10_wait_sigterm").await;
    let mut server = Server::start(&this_bin(), &db.url(), &[("MIGRATE_ON_START", "false")]).await;
    server
        .wait_for("/health/live", 200, Duration::from_secs(60))
        .await;
    server.sigterm();
    let exit = server.exit_within(Duration::from_secs(10)).await;
    assert!(exit.success(), "exit after SIGTERM while waiting: {exit}");
}

/// A database that cannot be reached yet does not make a
/// `MIGRATE_ON_START=false` server exit: it waits, alive and not ready.
#[tokio::test(flavor = "multi_thread")]
async fn serve_waits_while_postgres_is_unreachable() {
    let nowhere = format!(
        "postgres://trovato:trovato@127.0.0.1:{}/nowhere",
        free_port()
    );
    let mut server = Server::start(&this_bin(), &nowhere, &[("MIGRATE_ON_START", "false")]).await;
    server
        .wait_for("/health/live", 200, Duration::from_secs(60))
        .await;
    assert_eq!(server.status("/health/ready").await, Some(503));
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "serve must wait for Postgres, not exit"
    );
    assert_eq!(server.status("/health/live").await, Some(200));
    assert_eq!(server.status("/health/ready").await, Some(503));
    server.stop().await;
}

// =============================================================================
// The previous binary on a newer schema
// =============================================================================

/// A binary built without the newest migration starts and serves on a schema
/// that has it, both with `MIGRATE_ON_START` on (a single server rolled back)
/// and off (the previous image redeployed on Kubernetes).
#[tokio::test(flavor = "multi_thread")]
async fn binary_without_newest_migration_serves_on_newer_schema() {
    let db = ScratchDb::create("bg10_newer").await;
    let url = db.url();
    migrated(&db).await;
    add_unknown_migration(db.pool(), "add item summary column").await;

    for mode in ["true", "false"] {
        let mut server = Server::start(&this_bin(), &url, &[("MIGRATE_ON_START", mode)]).await;
        server
            .wait_for("/health/ready", 200, Duration::from_secs(60))
            .await;
        // The site itself answers (the installer, on this fresh database).
        assert!(server.status("/").await.is_some_and(|s| s < 500));
        server.stop().await;
    }

    let still_there: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version = 29991231000001")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(still_there, 1, "the newer migration's record is untouched");
}

/// A newer migration whose name marks it incompatible keeps an older binary
/// not ready: a rollback across it fails loudly instead of serving on a schema
/// the old code cannot use.
#[tokio::test(flavor = "multi_thread")]
async fn binary_refuses_a_newer_incompatible_migration() {
    let db = ScratchDb::create("bg10_incompat").await;
    let url = db.url();
    migrated(&db).await;
    add_unknown_migration(db.pool(), "incompatible drop item legacy").await;

    // With `MIGRATE_ON_START` on, as on a single server, it refuses to start,
    // and says why.
    let work = TempDir::new("incompat-serve");
    let out = tokio::time::timeout(
        Duration::from_secs(60),
        command(&this_bin(), &url, &work)
            .arg("serve")
            .env("PORT", free_port().to_string())
            .env("MIGRATE_ON_START", "true")
            .output(),
    )
    .await
    .expect("serve must exit on a newer incompatible migration")
    .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "serve exit: {}", out.status);
    assert!(stderr.contains("marked incompatible"), "{stderr}");

    // `trovato migrate` refuses it too, with a nonzero exit.
    let (ok, _, stderr) = migrate(&this_bin(), &url, &[], None).await;
    assert!(!ok);
    assert!(stderr.contains("marked incompatible"), "{stderr}");

    // With it off, the server stays not ready, and says why ...
    let mut server = Server::start(
        &this_bin(),
        &url,
        &[("MIGRATE_ON_START", "false"), ("RUST_LOG", "info")],
    )
    .await;
    server
        .wait_for("/health/live", 200, Duration::from_secs(60))
        .await;
    server
        .wait_for_log("marked incompatible", Duration::from_secs(30))
        .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(server.status("/health/ready").await, Some(503));
    assert!(!server.logged("Server listening"));

    // ... and the incompatible record is what holds it: without it, the
    // same server becomes ready by itself.
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 29991231000001")
        .execute(db.pool())
        .await
        .unwrap();
    server
        .wait_for("/health/ready", 200, Duration::from_secs(60))
        .await;
    server.stop().await;
}

// =============================================================================
// Probes
// =============================================================================

/// A TCP proxy in front of Postgres that the test can cut, which is how a test
/// "stops Postgres" without touching the server every other test is using.
struct PgProxy {
    port: u16,
    accept: tokio::task::JoinHandle<()>,
    conns: std::sync::Arc<parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl PgProxy {
    async fn start(upstream: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let conns = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let tracked = conns.clone();
        let accept = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let upstream = upstream.clone();
                tracked.lock().push(tokio::spawn(async move {
                    if let Ok(mut outbound) = TcpStream::connect(upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                }));
            }
        });
        Self {
            port,
            accept,
            conns,
        }
    }

    /// Postgres "goes away": no new connections, existing ones cut.
    fn cut(&self) {
        self.accept.abort();
        for conn in self.conns.lock().drain(..) {
            conn.abort();
        }
    }
}

/// `postgres://u:p@host:port/db` → (`host:port`, the URL through `proxy_port`).
fn through_proxy(url: &str, proxy_port: u16) -> (String, String) {
    let (scheme, rest) = url.split_once("://").unwrap();
    let (auth, tail) = rest.split_once('@').unwrap();
    let (host_port, path) = tail.split_once('/').unwrap();
    (
        host_port.to_string(),
        format!("{scheme}://{auth}@127.0.0.1:{proxy_port}/{path}"),
    )
}

/// Liveness never looks at a dependency, so a database blip takes a pod out of
/// rotation (readiness) without Kubernetes restarting it (liveness).
#[tokio::test(flavor = "multi_thread")]
async fn liveness_survives_postgres_going_away_and_readiness_does_not() {
    let db = ScratchDb::create("bg10_pgdown").await;
    migrated(&db).await;

    let (upstream, _) = through_proxy(&db.url(), 0);
    let proxy = PgProxy::start(upstream).await;
    let (_, proxied) = through_proxy(&db.url(), proxy.port);

    let mut server = Server::start(&this_bin(), &proxied, &[]).await;
    server
        .wait_for("/health/ready", 200, Duration::from_secs(60))
        .await;
    assert_eq!(server.status("/health/live").await, Some(200));

    proxy.cut();

    server
        .wait_for("/health/ready", 503, Duration::from_secs(15))
        .await;
    assert_eq!(server.status("/health").await, Some(503));
    for _ in 0..10 {
        assert_eq!(server.status("/health/live").await, Some(200));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    server.stop().await;
}

/// The anonymous answer is the status word. Pool statistics and the services
/// report go only to a loopback caller with no forwarding header (an operator
/// on the box) or to a caller with the configured token.
#[tokio::test(flavor = "multi_thread")]
async fn anonymous_health_reveals_no_pool_statistics() {
    let db = ScratchDb::create("bg10_leak").await;
    migrated(&db).await;
    let token = "health-token-for-the-zero-downtime-test";
    let mut server = Server::start(&this_bin(), &db.url(), &[("HEALTH_TOKEN", token)]).await;
    server
        .wait_for("/health/ready", 200, Duration::from_secs(60))
        .await;

    // A visitor through a reverse proxy on the same host: loopback peer, but a
    // forwarding header, so not an operator.
    for header in [
        ("x-forwarded-for", "203.0.113.7"),
        ("forwarded", "for=203.0.113.7"),
        ("x-real-ip", "203.0.113.7"),
    ] {
        for path in ["/health", "/health/ready", "/health/live"] {
            let (status, body) = server.get(path, &[header]).await.unwrap();
            assert_eq!(status, 200, "{path} with {header:?}");
            let json: Value = serde_json::from_str(&body).unwrap();
            let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
            assert_eq!(
                keys,
                vec!["status"],
                "{path} with {header:?} leaked: {body}"
            );
        }
    }
    // A wrong token is anonymous too.
    let (_, body) = server
        .get(
            "/health",
            &[
                ("x-forwarded-for", "203.0.113.7"),
                ("authorization", "Bearer not-the-token"),
            ],
        )
        .await
        .unwrap();
    let json: Value = serde_json::from_str(&body).unwrap();
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["status"], "wrong token got details: {body}");

    // The token holder sees the details from anywhere.
    let bearer = format!("Bearer {token}");
    let (_, body) = server
        .get(
            "/health",
            &[
                ("x-forwarded-for", "203.0.113.7"),
                ("authorization", &bearer),
            ],
        )
        .await
        .unwrap();
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(json["pool"]["size"].is_number(), "token holder: {body}");
    assert!(json["services"].is_object(), "token holder: {body}");

    // So does an operator on the box.
    let (_, body) = server.get("/health", &[]).await.unwrap();
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["postgres"], true, "loopback operator: {body}");
    assert!(json["pool"].is_object(), "loopback operator: {body}");

    // No probe sets a cookie.
    for path in ["/health", "/health/ready", "/health/live"] {
        let response = http()
            .get(format!("{}{path}", server.base))
            .send()
            .await
            .unwrap();
        assert!(
            response.headers().get("set-cookie").is_none(),
            "{path} set a cookie"
        );
    }
    server.stop().await;
}

/// The probes have no rate-limit bucket: far more probes than the visitor
/// `page` bucket allows in a minute all answer.
#[tokio::test(flavor = "multi_thread")]
async fn probes_are_not_rate_limited() {
    let db = ScratchDb::create("bg10_probe_rl").await;
    migrated(&db).await;
    let mut server = Server::start(
        &this_bin(),
        &db.url(),
        &[
            ("TROVATO_RATE_LIMIT_PAGE", "5"),
            ("TROVATO_RATE_LIMIT_FORMS", "2"),
        ],
    )
    .await;
    server
        .wait_for("/health/ready", 200, Duration::from_secs(60))
        .await;
    for path in ["/health", "/health/ready", "/health/live"] {
        for _ in 0..20 {
            assert_eq!(server.status(path).await, Some(200), "{path}");
        }
    }

    // The control: the same caller's page views do hit the limit, so the
    // probes above passed because they are exempt, not because nothing limits.
    let mut limited = false;
    for _ in 0..10 {
        if server.status("/").await == Some(429) {
            limited = true;
            break;
        }
    }
    assert!(limited, "page views past the limit of 5 must get 429");

    // And the probes still answer once the caller is limited.
    for path in ["/health", "/health/ready", "/health/live"] {
        assert_eq!(server.status(path).await, Some(200), "{path} after 429");
    }

    // Only reads are exempt: a POST to a probe path is limited like any other.
    let mut post_limited = false;
    for _ in 0..10 {
        let status = http()
            .post(format!("{}/health", server.base))
            .send()
            .await
            .ok()
            .map(|r| r.status().as_u16());
        assert_ne!(status, Some(200), "POST /health is not a probe");
        if status == Some(429) {
            post_limited = true;
            break;
        }
    }
    assert!(
        post_limited,
        "POST /health past the forms limit of 2 must get 429"
    );
    server.stop().await;
}

// =============================================================================
// Shutdown
// =============================================================================

/// After SIGTERM the pod reports not ready at once, keeps serving for the drain
/// period, and a request already in flight still gets its answer.
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_turns_readiness_off_and_finishes_in_flight_requests() {
    let db = ScratchDb::create("bg10_drain").await;
    migrated(&db).await;
    let mut server = Server::start(
        &this_bin(),
        &db.url(),
        &[
            ("SHUTDOWN_DRAIN_SECS", "8"),
            ("SHUTDOWN_TIMEOUT_SECS", "20"),
        ],
    )
    .await;
    server
        .wait_for("/health/ready", 200, Duration::from_secs(60))
        .await;

    // A slow request: headers sent, body still to come, so its handler is
    // waiting on the client when the signal arrives.
    let addr = server.base.trim_start_matches("http://").to_string();
    let mut slow = TcpStream::connect(&addr).await.unwrap();
    let body = "username=x";
    slow.write_all(
        format!(
            "POST /install/admin HTTP/1.1\r\nHost: {addr}\r\n\
             Content-Type: application/x-www-form-urlencoded\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let signalled = Instant::now();
    server.sigterm();

    // Not ready within the drain, while still answering.
    server
        .wait_for("/health/ready", 503, Duration::from_secs(2))
        .await;
    assert!(signalled.elapsed() < Duration::from_secs(3));
    assert_eq!(server.status("/health/live").await, Some(200));

    // Still serving new requests late in the drain (it lasts 8 seconds).
    tokio::time::sleep(Duration::from_secs(5).saturating_sub(signalled.elapsed())).await;
    assert_eq!(
        server.status("/health/live").await,
        Some(200),
        "during drain"
    );
    assert!(signalled.elapsed() < Duration::from_secs(8));

    // Finish the slow request after the drain has ended.
    tokio::time::sleep(Duration::from_secs(10).saturating_sub(signalled.elapsed())).await;
    slow.write_all(body.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(10), slow.read_to_string(&mut response))
        .await
        .expect("the in-flight request must be answered")
        .unwrap();
    let code: u16 = response
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("in-flight request got: {response:?}"));
    assert!(
        code < 500,
        "in-flight request answered {code}: {response:?}"
    );

    let exit = tokio::time::timeout(Duration::from_secs(30), server.child.wait())
        .await
        .expect("the server exits after the drain")
        .unwrap();
    assert!(exit.success(), "clean exit after SIGTERM, got {exit}");
}

/// SIGINT (Ctrl-C at a terminal) never drains, whatever the drain setting:
/// a developer's server stops at once.
#[tokio::test(flavor = "multi_thread")]
async fn sigint_does_not_drain() {
    let db = ScratchDb::create("bg10_sigint").await;
    migrated(&db).await;
    let mut server = Server::start(&this_bin(), &db.url(), &[("SHUTDOWN_DRAIN_SECS", "10")]).await;
    server
        .wait_for("/health/ready", 200, Duration::from_secs(60))
        .await;
    server.signal("-INT");
    let exit = server.exit_within(Duration::from_secs(5)).await;
    assert!(exit.success(), "exit after SIGINT: {exit}");
}

/// With a drain configured, an in-flight request that never finishes is cut
/// off at `SHUTDOWN_TIMEOUT_SECS`, so the pod exits inside its grace period
/// instead of waiting for SIGKILL.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_never_finishes_is_cut_off_after_the_drain() {
    let db = ScratchDb::create("bg10_cutoff").await;
    migrated(&db).await;
    let mut server = Server::start(
        &this_bin(),
        &db.url(),
        &[("SHUTDOWN_DRAIN_SECS", "1"), ("SHUTDOWN_TIMEOUT_SECS", "3")],
    )
    .await;
    server
        .wait_for("/health/ready", 200, Duration::from_secs(60))
        .await;

    // Headers only; the body never comes.
    let addr = server.base.trim_start_matches("http://").to_string();
    let mut stuck = TcpStream::connect(&addr).await.unwrap();
    stuck
        .write_all(
            format!(
                "POST /install/admin HTTP/1.1\r\nHost: {addr}\r\n\
                 Content-Type: application/x-www-form-urlencoded\r\n\
                 Content-Length: 100\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let signalled = Instant::now();
    server.sigterm();
    // 1 (drain) + 3 (timeout) + 0.5 + up to 1 (pool close), with slack.
    let exit = server.exit_within(Duration::from_secs(15)).await;
    assert!(exit.success(), "exit after the cut-off: {exit}");
    assert!(
        signalled.elapsed() >= Duration::from_secs(4),
        "the drain and the timeout both ran: {:?}",
        signalled.elapsed()
    );
    drop(stuck);
}

/// SIGTERM while a `MIGRATE_ON_START=false` server builds its application
/// (after the schema is ready, before it serves) is not lost: the signal is
/// listened for once, from the start, and the server drains and exits.
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_while_building_after_the_wait_is_not_lost() {
    let db = ScratchDb::create("bg10_build_sigterm").await;
    let url = db.url();
    let plugins = repo_root().join("plugins");
    assert!(migrate(&this_bin(), &url, &[], Some(&plugins)).await.0);

    let mut server = Server::start(
        &this_bin(),
        &url,
        &[
            ("MIGRATE_ON_START", "false"),
            ("PLUGINS_DIR", plugins.to_str().unwrap()),
            ("SHUTDOWN_DRAIN_SECS", "2"),
            ("RUST_LOG", "info"),
        ],
    )
    .await;
    server
        .wait_for("/health/live", 200, Duration::from_secs(60))
        .await;
    // Loading the in-tree plugins takes a while, so the signal normally lands
    // during the build; whether it did is printed, and the outcome must be the
    // same either way.
    let during_build = !server.logged("Server listening");
    println!("SIGTERM during the application build: {during_build}");
    server.sigterm();
    let exit = server.exit_within(Duration::from_secs(120)).await;
    assert!(
        exit.success(),
        "exit after SIGTERM during the build: {exit}"
    );
    if during_build && server.logged("Server listening") {
        assert!(
            server.logged("Draining"),
            "served after the signal without draining:\n{}",
            server.log.lock()
        );
    }
}

// =============================================================================
// The CI rollback job
// =============================================================================

/// The previous binary (`TROVATO_PREVIOUS_BIN`) serves on the schema this
/// branch's `trovato migrate` produced: ready, an anonymous page, a login.
///
/// Skips unless `TROVATO_PREVIOUS_BIN` is set; the CI `rollback` job sets it.
/// `TROVATO_PREVIOUS_ROOT` points at the previous checkout's templates and
/// static files (default: this tree's).
#[tokio::test(flavor = "multi_thread")]
async fn previous_release_serves_on_this_schema() {
    let Some(previous) = std::env::var_os("TROVATO_PREVIOUS_BIN").map(PathBuf::from) else {
        eprintln!("TROVATO_PREVIOUS_BIN not set; skipping the rollback smoke test");
        return;
    };
    let previous_root = std::env::var_os("TROVATO_PREVIOUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(repo_root);

    let db = ScratchDb::create("bg10_rollback").await;
    let url = db.url();

    // This branch migrates and installs the site.
    let (ok, out, _) = migrate(&this_bin(), &url, &[], None).await;
    assert!(ok, "this branch's migrate");
    println!("this branch's trovato migrate: {}", out.trim());
    let mut current = Server::start(&this_bin(), &url, &[("MIGRATE_ON_START", "false")]).await;
    current
        .wait_for("/health/ready", 200, Duration::from_secs(120))
        .await;
    let admin = "rollbackadmin";
    let password = "rollback-admin-password";
    install(&current.base, admin, password).await;
    current.stop().await;

    // The previous binary on that schema.
    let templates = previous_root.join("templates");
    let statics = previous_root.join("static");
    let mut old = Server::start(
        &previous,
        &url,
        &[
            ("TEMPLATES_DIR", templates.to_str().unwrap()),
            ("STATIC_DIR", statics.to_str().unwrap()),
        ],
    )
    .await;
    // A binary from before the probes existed has only `/health`.
    old.wait_for("/health", 200, Duration::from_secs(120)).await;
    let ready = old.status("/health/ready").await;
    assert!(
        ready == Some(200) || ready == Some(404),
        "previous binary readiness: {ready:?}"
    );
    println!("previous binary: /health 200, /health/ready {ready:?}");

    let front = old.get("/", &[]).await.unwrap();
    assert_eq!(front.0, 200, "anonymous front page on the previous binary");
    println!("previous binary: anonymous GET / -> {}", front.0);

    let login = http()
        .post(format!("{}/user/login/json", old.base))
        .json(&serde_json::json!({"username": admin, "password": password}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status().as_u16(), 200, "login on the previous binary");
    println!("previous binary: POST /user/login/json -> 200");
    old.stop().await;
}

/// Run the web installer: an administrator, then the site name.
async fn install(base: &str, admin: &str, password: &str) {
    let client = http();
    let response = client
        .post(format!("{base}/install/admin"))
        .form(&[
            ("username", admin),
            ("email", "admin@rollback.test"),
            ("password", password),
            ("password_confirm", password),
        ])
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_redirection(),
        "install admin: {}",
        response.status()
    );
    let response = client
        .post(format!("{base}/install/site"))
        .form(&[("site_name", "Rollback")])
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_redirection(),
        "install site: {}",
        response.status()
    );
}
