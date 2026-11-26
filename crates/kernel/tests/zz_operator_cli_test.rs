#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `trovato user create` and `trovato cache clear` (B165), through the real
//! binary.
//!
//! An operator's account import and a direct database write both used to need
//! more than the command line gave: the first a SQL `INSERT INTO users`, the
//! second a server restart. Each test here runs `trovato` as a child process
//! against a scratch database of its own, and the cache test runs a real
//! `trovato serve` beside it, because what is under test is one process telling
//! another.
//!
//! Named to sort after every other kernel test target, so adding it does not
//! move any existing kernel target to a different CI shard
//! (`scripts/ci-test-shard.sh` partitions by sorted index).
//!
//! Requires Postgres and Redis; runs in CI.

use std::net::TcpListener as StdListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use trovato_test_utils::ScratchDb;
use uuid::Uuid;

/// Held by every test that clears caches: a clear of every kind drops the
/// shared search settings copy one of them watches.
static CLEARING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A password long enough for the admin form's rule (12 to 128 characters).
const PASSWORD: &str = "operator-cli-password-1";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trovato"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A directory under the system temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("trovato-{label}-{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `trovato` command with a clean environment, so the developer's `.env`
/// and the suite's shared `DATABASE_URL` cannot leak in. No plugins: these
/// tests are about the kernel.
fn command(database_url: &str, work: &TempDir) -> Command {
    let mut cmd = Command::new(bin());
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
        .env("PLUGINS_DIR", work.0.join("plugins"))
        .env("COOKIE_SECURE", "never")
        .kill_on_drop(true);
    cmd
}

/// What a finished command said.
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn all(&self) -> String {
        format!(
            "exit {:?}\n--- stdout\n{}--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

/// Run `trovato <args>` to completion, writing `stdin` to it.
async fn run(database_url: &str, args: &[&str], stdin: &str) -> Ran {
    let work = TempDir::new("cli");
    let mut child = command(database_url, &work)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn trovato");
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.as_bytes()).await.unwrap();
    drop(input);
    let out = child.wait_with_output().await.unwrap();
    Ran {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

async fn migrated(label: &str) -> ScratchDb {
    let db = ScratchDb::create(label).await;
    trovato_kernel::db::run_migrations(db.pool())
        .await
        .expect("migrate the scratch database");
    db
}

#[derive(sqlx::FromRow)]
struct Account {
    id: Uuid,
    mail: Option<String>,
    pass: String,
    status: i16,
    is_admin: bool,
}

async fn account(pool: &PgPool, name: &str) -> Option<Account> {
    sqlx::query_as("SELECT id, mail, pass, status, is_admin FROM users WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn user_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A password verifies against `user`'s stored hash, the check login makes.
async fn password_verifies(pool: &PgPool, name: &str, password: &str) -> bool {
    trovato_kernel::models::User::find_by_name(pool, name)
        .await
        .unwrap()
        .expect("the account exists")
        .verify_password(password)
}

// =============================================================================
// trovato user create
// =============================================================================

#[tokio::test]
async fn user_create_makes_an_account_that_can_log_in() {
    let db = migrated("cli_user_create").await;
    sqlx::query("INSERT INTO roles (id, name) VALUES ($1, 'cli_editor')")
        .bind(Uuid::now_v7())
        .execute(db.pool())
        .await
        .unwrap();

    let ran = run(
        &db.url(),
        &[
            "user",
            "create",
            "cli_alice",
            "--mail",
            "alice@example.com",
            "--role",
            "cli_editor",
        ],
        &format!("{PASSWORD}\n"),
    )
    .await;
    assert_eq!(ran.code, Some(0), "{}", ran.all());
    assert!(
        !ran.all().contains(PASSWORD),
        "the password must never be printed:\n{}",
        ran.all()
    );

    let alice = account(db.pool(), "cli_alice")
        .await
        .expect("the account exists");
    assert_eq!(alice.mail.as_deref(), Some("alice@example.com"));
    assert_eq!(alice.status, 1, "active unless --blocked");
    assert!(!alice.is_admin, "the command never makes a superuser");
    assert!(
        password_verifies(db.pool(), "cli_alice", PASSWORD).await,
        "the password read from stdin is the one that logs in"
    );

    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT r.name FROM roles r JOIN user_roles ur ON ur.role_id = r.id WHERE ur.user_id = $1",
    )
    .bind(alice.id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(roles, vec!["cli_editor".to_string()]);

    // The security audit trail records the role grant, as the admin form's
    // role checkboxes do (`RoleService::assign_to_user`).
    let events: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM security_audit_log WHERE user_id = $1 ORDER BY id")
            .bind(alice.id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(
        events.iter().any(|k| k == "user.roles_changed"),
        "role grant audited: {events:?}"
    );
}

#[tokio::test]
async fn user_create_blocked_and_without_a_password() {
    let db = migrated("cli_user_nopass").await;

    // No stdin at all: --no-password must not wait for one.
    let ran = run(
        &db.url(),
        &[
            "user",
            "create",
            "cli_bob",
            "--mail",
            "bob@example.com",
            "--no-password",
            "--blocked",
        ],
        "",
    )
    .await;
    assert_eq!(ran.code, Some(0), "{}", ran.all());

    let bob = account(db.pool(), "cli_bob").await.expect("account exists");
    assert_eq!(bob.status, 0, "--blocked creates a blocked account");
    assert!(
        bob.pass.is_empty(),
        "--no-password stores no password, so nothing logs in until a reset"
    );
    assert!(!password_verifies(db.pool(), "cli_bob", "").await);

    // The reset the command points at is what lets it log in.
    let reset = run(
        &db.url(),
        &["user", "reset-password", "cli_bob"],
        &format!("{PASSWORD}\n"),
    )
    .await;
    assert_eq!(reset.code, Some(0), "{}", reset.all());
    assert!(password_verifies(db.pool(), "cli_bob", PASSWORD).await);
}

#[tokio::test]
async fn user_create_refuses_what_the_admin_form_refuses() {
    let db = migrated("cli_user_refuse").await;
    let first = run(
        &db.url(),
        &["user", "create", "cli_carol", "--mail", "carol@example.com"],
        &format!("{PASSWORD}\n"),
    )
    .await;
    assert_eq!(first.code, Some(0), "{}", first.all());
    let before = user_count(db.pool()).await;

    // (args, stdin, what the message must say)
    let cases: Vec<(Vec<&str>, String, &str)> = vec![
        (
            vec!["user", "create", "CLI_Carol", "--mail", "other@example.com"],
            format!("{PASSWORD}\n"),
            "Username 'CLI_Carol' is already taken.",
        ),
        (
            vec!["user", "create", "cli_dave", "--mail", "Carol@Example.com"],
            format!("{PASSWORD}\n"),
            "Email 'Carol@Example.com' is already in use.",
        ),
        (
            vec!["user", "create", "cli_dave"],
            format!("{PASSWORD}\n"),
            "Email is required.",
        ),
        (
            vec!["user", "create", "cli_dave", "--mail", ""],
            format!("{PASSWORD}\n"),
            "Email is required.",
        ),
        (
            vec!["user", "create", "cli_dave", "--mail", "   "],
            format!("{PASSWORD}\n"),
            "Email is required.",
        ),
        (
            vec!["user", "create", "cli_dave", "--mail", "dave@example.com"],
            "short\n".to_string(),
            "Password must be at least 12 characters.",
        ),
        (
            vec!["user", "create", "cli_dave", "--mail", "dave@example.com"],
            "\n".to_string(),
            "Password is required.",
        ),
        (
            vec![
                "user",
                "create",
                "cli_dave",
                "--mail",
                "dave@example.com",
                "--role",
                "no_such_role",
            ],
            format!("{PASSWORD}\n"),
            "Role 'no_such_role' not found.",
        ),
    ];
    for (args, stdin, says) in cases {
        let ran = run(&db.url(), &args, &stdin).await;
        assert_eq!(ran.code, Some(1), "{args:?} must exit 1:\n{}", ran.all());
        assert!(
            ran.stderr.contains(says),
            "{args:?} must say {says:?}:\n{}",
            ran.all()
        );
        assert!(!ran.all().contains(PASSWORD), "{}", ran.all());
        assert!(
            says != "Email is required." || !ran.stderr.contains("already in use"),
            "an empty address is missing, not taken:\n{}",
            ran.all()
        );
    }
    assert_eq!(
        user_count(db.pool()).await,
        before,
        "a refused create writes nothing"
    );
}

// =============================================================================
// trovato cache clear, against a running server
// =============================================================================

/// A running `trovato serve`.
struct Server {
    child: Child,
    base: String,
    _work: TempDir,
}

impl Server {
    async fn start(database_url: &str) -> Self {
        let work = TempDir::new("serve");
        let port = StdListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = command(database_url, &work)
            .arg("serve")
            .env("PORT", port.to_string())
            // Long enough that only an invalidation, never the TTL, can
            // explain a fresh read inside this test.
            .env("CACHE_TTL", "3600")
            .env("CACHE_TTL_ITEMS", "3600")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn trovato serve");
        let mut server = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
            _work: work,
        };
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if let Ok(Some(exit)) = server.child.try_wait() {
                panic!("trovato serve exited ({exit}) before it was ready");
            }
            if server.get("/health/ready").await.map(|(s, _)| s) == Some(200) {
                return server;
            }
            assert!(Instant::now() < deadline, "server never became ready");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn get(&self, path: &str) -> Option<(u16, String)> {
        let response = http()
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .ok()?;
        let status = response.status().as_u16();
        Some((status, response.text().await.unwrap_or_default()))
    }

    async fn page(&self, path: &str) -> String {
        let (status, body) = self.get(path).await.expect("server answers");
        assert_eq!(status, 200, "GET {path}:\n{body}");
        body
    }

    async fn stop(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(30), self.child.wait()).await;
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

/// A published page, written straight to the database.
async fn seed_page(pool: &PgPool, title: &str) -> Uuid {
    let id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, promote, \
         sticky, fields, language) \
         VALUES ($1, 'page', $2, '00000000-0000-0000-0000-000000000000', 1, $3, $3, 0, 0, \
         '{}'::jsonb, 'en')",
    )
    .bind(id)
    .bind(title)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO item_revision (id, item_id, title, author_id, status, created, fields, log) \
         VALUES ($1, $2, $3, '00000000-0000-0000-0000-000000000000', 1, $4, '{}'::jsonb, 'seed')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(title)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Retitle an item behind every server's back.
async fn retitle(pool: &PgPool, id: Uuid, title: &str) {
    sqlx::query("UPDATE item SET title = $2 WHERE id = $1")
        .bind(id)
        .bind(title)
        .execute(pool)
        .await
        .unwrap();
}

/// The first request made after a command has returned shows `needle`. Not
/// retried: a command that returned while a server still held a copy one poll
/// from being dropped fails here.
async fn first_request_shows(server: &Server, path: &str, needle: &str) {
    let body = server.page(path).await;
    assert!(
        body.contains(needle),
        "the first request after the command must show {needle:?}"
    );
}

#[tokio::test]
async fn cache_clear_makes_a_running_server_serve_a_direct_write() {
    let _serial = CLEARING.lock().await;
    let db = migrated("cli_cache_clear").await;
    sqlx::query(
        "INSERT INTO site_config (key, value) VALUES ('installed', 'true'::jsonb) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let id = seed_page(db.pool(), "Title before the write").await;
    let path = format!("/item/{id}");

    let server = Server::start(&db.url()).await;
    assert!(server.page(&path).await.contains("Title before the write"));

    // A write the server does not hear about: its cached item still serves.
    retitle(db.pool(), id, "Title after the write").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let stale = server.page(&path).await;
    assert!(
        stale.contains("Title before the write") && !stale.contains("Title after the write"),
        "the server serves its cached item until told (the premise of this test)"
    );

    // A kind other than items leaves the item cached.
    let other = run(&db.url(), &["cache", "clear", "--kind", "menus"], "").await;
    assert_eq!(other.code, Some(0), "{}", other.all());
    assert!(
        server.page(&path).await.contains("Title before the write"),
        "--kind menus must not clear items"
    );

    let ran = run(&db.url(), &["cache", "clear"], "").await;
    assert_eq!(ran.code, Some(0), "{}", ran.all());
    first_request_shows(&server, &path, "Title after the write").await;

    // Naming the kind does it too.
    retitle(db.pool(), id, "Title after the second write").await;
    let named = run(&db.url(), &["cache", "clear", "--kind", "items"], "").await;
    assert_eq!(named.code, Some(0), "{}", named.all());
    first_request_shows(&server, &path, "Title after the second write").await;

    server.stop().await;
}

#[tokio::test]
async fn cache_clear_refuses_an_unknown_kind() {
    let db = migrated("cli_cache_kind").await;
    let ran = run(&db.url(), &["cache", "clear", "--kind", "no_such_kind"], "").await;
    assert_ne!(ran.code, Some(0), "{}", ran.all());
    assert!(
        ran.stderr.contains("items"),
        "lists the kinds:\n{}",
        ran.all()
    );
}

/// The search settings are the one `site_config` value cached in Redis rather
/// than in a server's memory, so no generation reaches them: clearing
/// `site_config` drops that copy too, and a hand edit is read at once.
#[tokio::test]
async fn cache_clear_drops_the_shared_search_settings() {
    use trovato_kernel::cache::CacheLayer;
    use trovato_kernel::search::settings::SearchSettings;
    let _serial = CLEARING.lock().await;

    let db = migrated("cli_cache_search").await;
    let redis = redis::Client::open(trovato_test_utils::env::redis_url()).unwrap();
    let cache = CacheLayer::new(redis, Default::default());
    let write = |per_page: u32| {
        let pool = db.pool().clone();
        async move {
            let settings = SearchSettings {
                results_per_page: per_page,
                ..Default::default()
            };
            sqlx::query(
                "INSERT INTO site_config (key, value) VALUES ('search', $1) \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(serde_json::to_value(&settings).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        }
    };

    write(17).await;
    let ran = run(&db.url(), &["cache", "clear", "--kind", "site_config"], "").await;
    assert_eq!(ran.code, Some(0), "{}", ran.all());
    assert_eq!(
        SearchSettings::load(db.pool(), &cache)
            .await
            .results_per_page,
        17
    );

    // Written behind the cache's back: the cached copy still answers.
    write(23).await;
    assert_eq!(
        SearchSettings::load(db.pool(), &cache)
            .await
            .results_per_page,
        17
    );

    // Another kind leaves it; site_config (or every kind) drops it.
    let other = run(&db.url(), &["cache", "clear", "--kind", "items"], "").await;
    assert_eq!(other.code, Some(0), "{}", other.all());
    assert_eq!(
        SearchSettings::load(db.pool(), &cache)
            .await
            .results_per_page,
        17
    );
    let ran = run(&db.url(), &["cache", "clear"], "").await;
    assert_eq!(ran.code, Some(0), "{}", ran.all());
    assert_eq!(
        SearchSettings::load(db.pool(), &cache)
            .await
            .results_per_page,
        23
    );

    // Leave no copy of this scratch site's settings for another test to read.
    let ran = run(&db.url(), &["cache", "clear", "--kind", "site_config"], "").await;
    assert_eq!(ran.code, Some(0), "{}", ran.all());
}

/// A grant that fails after the account is written (here a trigger refusing one
/// role; in life a role deleted between lookup and grant) cannot be retried by
/// running the command again, which now refuses the taken name. The command
/// grants what it can, exits 1, and prints the commands that finish the job.
#[tokio::test]
async fn user_create_says_how_to_finish_a_failed_role_grant() {
    let db = migrated("cli_user_grant").await;
    for name in ["cli_good", "cli_refused"] {
        sqlx::query("INSERT INTO roles (id, name) VALUES ($1, $2)")
            .bind(Uuid::now_v7())
            .bind(name)
            .execute(db.pool())
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        "CREATE FUNCTION refuse_cli_refused() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN \
           IF (SELECT name FROM roles WHERE id = NEW.role_id) = 'cli_refused' THEN \
             RAISE EXCEPTION 'grant refused for this test'; \
           END IF; \
           RETURN NEW; \
         END $$; \
         CREATE TRIGGER refuse_cli_refused BEFORE INSERT ON user_roles \
           FOR EACH ROW EXECUTE FUNCTION refuse_cli_refused();",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let ran = run(
        &db.url(),
        &[
            "user",
            "create",
            "cli_erin",
            "--mail",
            "erin@example.com",
            "--role",
            "cli_refused",
            "--role",
            "cli_good",
        ],
        &format!("{PASSWORD}\n"),
    )
    .await;
    assert_eq!(ran.code, Some(1), "{}", ran.all());
    assert!(
        ran.stderr.contains("User 'cli_erin'")
            && ran.stderr.contains("was created")
            && ran
                .stderr
                .contains("trovato user role-add cli_erin cli_refused"),
        "says what exists and how to finish:\n{}",
        ran.all()
    );
    assert!(
        !ran.stderr.contains("role-add cli_erin cli_good"),
        "a role that was granted is not listed as missing:\n{}",
        ran.all()
    );

    // The grant after the failed one was still made.
    let erin = account(db.pool(), "cli_erin")
        .await
        .expect("account exists");
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT r.name FROM roles r JOIN user_roles ur ON ur.role_id = r.id WHERE ur.user_id = $1",
    )
    .bind(erin.id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(roles, vec!["cli_good".to_string()]);

    // And the printed command finishes it.
    sqlx::raw_sql("DROP TRIGGER refuse_cli_refused ON user_roles")
        .execute(db.pool())
        .await
        .unwrap();
    let finish = run(
        &db.url(),
        &["user", "role-add", "cli_erin", "cli_refused"],
        "",
    )
    .await;
    assert_eq!(finish.code, Some(0), "{}", finish.all());
}

/// A password typed at a terminal is not shown on it.
///
/// The command runs on a pseudo-terminal with echo on, as a shell leaves it;
/// the test waits for the prompt, types the password, and reads back
/// everything the terminal displayed.
#[cfg(unix)]
#[tokio::test]
async fn user_create_does_not_echo_a_typed_password() {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    let db = migrated("cli_user_tty").await;
    let master =
        rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
            .unwrap();
    rustix::pty::grantpt(&master).unwrap();
    rustix::pty::unlockpt(&master).unwrap();
    let slave_path = rustix::pty::ptsname(&master, Vec::new()).unwrap();
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(slave_path.to_str().unwrap())
        .unwrap();
    let attrs = rustix::termios::tcgetattr(&slave).unwrap();
    assert!(
        attrs
            .local_modes
            .contains(rustix::termios::LocalModes::ECHO),
        "the terminal starts with echo on"
    );

    let work = TempDir::new("cli-tty");
    let mut child = command(&db.url(), &work)
        .args(["user", "create", "cli_tty", "--mail", "tty@example.com"])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("spawn trovato");
    drop(slave);

    let mut master = std::fs::File::from(master);
    let mut writer = master.try_clone().unwrap();
    let transcript = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = transcript.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    let shown = || String::from_utf8_lossy(&transcript.lock().unwrap()).to_string();

    let deadline = Instant::now() + Duration::from_secs(60);
    while !shown().contains("Password:") {
        assert!(
            Instant::now() < deadline,
            "no password prompt on the terminal:\n{}",
            shown()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    writer
        .write_all(format!("{PASSWORD}\n").as_bytes())
        .unwrap();

    let status = tokio::time::timeout(Duration::from_secs(120), child.wait())
        .await
        .expect("the command finishes")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let shown = shown();
    assert!(status.success(), "{status}:\n{shown}");
    assert!(
        !shown.contains(PASSWORD),
        "the typed password was shown on the terminal:\n{shown}"
    );
    assert!(shown.contains("Created active user 'cli_tty'"), "{shown}");
    assert!(password_verifies(db.pool(), "cli_tty", PASSWORD).await);
}

/// A pseudo-terminal, with echo on as a shell leaves it, and the path to its
/// slave side (reopening it afterwards reads the pty's termios back without
/// needing to keep a slave file descriptor of our own open).
#[cfg(unix)]
fn open_pty() -> (std::fs::File, std::fs::File, PathBuf) {
    let master =
        rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
            .unwrap();
    rustix::pty::grantpt(&master).unwrap();
    rustix::pty::unlockpt(&master).unwrap();
    let slave_path = PathBuf::from(
        rustix::pty::ptsname(&master, Vec::new())
            .unwrap()
            .to_str()
            .unwrap(),
    );
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slave_path)
        .unwrap();
    let attrs = rustix::termios::tcgetattr(&slave).unwrap();
    assert!(
        attrs
            .local_modes
            .contains(rustix::termios::LocalModes::ECHO),
        "the terminal starts with echo on"
    );
    (std::fs::File::from(master), slave, slave_path)
}

/// Starts `trovato user create` on a pty and returns it once the password
/// prompt has reached the terminal, along with everything the terminal has
/// shown so far (shared with the caller, who keeps reading it).
#[cfg(unix)]
async fn user_create_waiting_for_password(
    db: &ScratchDb,
    work: &TempDir,
    slave: std::fs::File,
    master: std::fs::File,
) -> (
    Child,
    std::fs::File,
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    let mut cmd = command(&db.url(), work);
    cmd.args(["user", "create", "cli_tty", "--mail", "tty@example.com"])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    // SAFETY: `setsid` and the `ioctl` only touch the child's own session and
    // its already-redirected stdin (tokio dup2s stdio before running this),
    // not any memory shared with the parent. Without this, the pty has no
    // foreground process group, so the interrupt character typed into it
    // (`interrupted_password_prompt::interrupt_character`) has nowhere to
    // deliver `SIGINT` to.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn trovato");
    drop(slave);

    let mut reader = master.try_clone().unwrap();
    let transcript = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = transcript.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let shown = String::from_utf8_lossy(&transcript.lock().unwrap()).to_string();
        if shown.contains("Password:") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no password prompt on the terminal:\n{shown}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    (child, master, transcript)
}

/// Interrupting the password prompt leaves the terminal exactly as it was
/// (B224): echo on, as the shell left it, not off as the read had set it to
/// answer the prompt without showing what was typed.
///
/// Three ways a shell delivers that interruption, each its own case: a signal
/// sent directly (`kill`, as `SIGINT` or `SIGTERM` reach a foreground
/// process), and the interrupt character typed at the terminal itself, which
/// the pty's own line discipline turns into `SIGINT` before `trovato` ever
/// sees a byte of it — the path a person pressing Ctrl-C actually takes.
#[cfg(unix)]
mod interrupted_password_prompt {
    use super::*;

    async fn assert_restores_terminal(
        label: &str,
        interrupt: impl FnOnce(&Child, &std::fs::File),
        expected_code: i32,
    ) {
        let db = migrated(label).await;
        let (master, slave, slave_path) = open_pty();
        let work = TempDir::new(label);
        let (mut child, master, transcript) =
            user_create_waiting_for_password(&db, &work, slave, master).await;

        interrupt(&child, &master);

        let status = tokio::time::timeout(Duration::from_secs(60), child.wait())
            .await
            .expect("the command finishes")
            .unwrap();
        let shown = String::from_utf8_lossy(&transcript.lock().unwrap()).to_string();
        assert_eq!(status.code(), Some(expected_code), "{label}:\n{shown}");

        // Reopening the slave reads the pty's termios back without racing the
        // child's own exit: it is a property of the pty itself, not of any
        // one file descriptor onto it.
        let reopened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&slave_path)
            .unwrap();
        let attrs = rustix::termios::tcgetattr(&reopened).unwrap();
        assert!(
            attrs
                .local_modes
                .contains(rustix::termios::LocalModes::ECHO),
            "{label}: echo was left off after the interrupt:\n{shown}"
        );
    }

    #[tokio::test]
    async fn sigint() {
        assert_restores_terminal(
            "cli-tty-sigint",
            |child, _master| {
                let pid = child.id().expect("child has a pid") as libc::pid_t;
                assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
            },
            130,
        )
        .await;
    }

    #[tokio::test]
    async fn sigterm() {
        assert_restores_terminal(
            "cli-tty-sigterm",
            |child, _master| {
                let pid = child.id().expect("child has a pid") as libc::pid_t;
                assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
            },
            143,
        )
        .await;
    }

    #[tokio::test]
    async fn sighup() {
        assert_restores_terminal(
            "cli-tty-sighup",
            |child, _master| {
                let pid = child.id().expect("child has a pid") as libc::pid_t;
                assert_eq!(unsafe { libc::kill(pid, libc::SIGHUP) }, 0);
            },
            129,
        )
        .await;
    }

    /// The interrupt character (Ctrl-C, byte `0x03`), typed at the terminal
    /// rather than sent with `kill`: the pty's line discipline (`ISIG` is on
    /// by default) turns it into `SIGINT` for the foreground process itself,
    /// so `trovato` never reads the byte.
    #[tokio::test]
    async fn interrupt_character() {
        use std::io::Write;

        assert_restores_terminal(
            "cli-tty-ctrl-c",
            |_child, master| {
                let mut writer = master.try_clone().unwrap();
                writer.write_all(&[0x03]).unwrap();
            },
            130,
        )
        .await;
    }
}
