//! Performance tests, Gander style: exact counts per page, not timings.
//!
//! A [`PerfSite`] is a whole site on a database of its own: a scratch
//! database, migrated, with a fixed set of plugins enabled, filled by
//! `trovato generate` with a small fixed seed, served by the real router, and
//! a Redis database number nobody else uses. [`PerfSite::visit`] loads one
//! page as one visitor with cold or warm caches and returns what the request
//! did, read from the [`RequestProfileSnapshot`] the timing layer leaves in
//! the response.
//!
//! The expected numbers live in `tests/fixtures/performance_counts.toml`, one
//! table per page, visitor and cache state. A change that adds or removes a
//! query, a cache call or a plugin call fails its page's test until the
//! expected number is changed in the same commit, so the change shows in
//! review as a one-line diff. To rewrite the file from what the pages do now:
//!
//! ```sh
//! TROVATO_PERF_RECORD=1 cargo test -p trovato-kernel --test performance_test -- --test-threads=1
//! ```
//!
//! See `docs/performance-tests.md`.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use axum::body::{Body, to_bytes};
use axum::http::Request;
use trovato_kernel::telemetry::RequestProfileSnapshot;

use super::TestApp;

/// The plugin modules a performance site loads: the blog (its type, listing,
/// tag pages and feed) and search. CI builds both. The site sees a plugin
/// directory holding only these, so the counts do not depend on which other
/// plugin modules a machine happens to have built.
pub const SITE_PLUGINS: &[&str] = &["trovato_blog", "trovato_search"];

/// Enabled without a module: `trovato_comments` turns on the kernel's comment
/// threads, which is all the item page needs from it (its module only
/// declares permissions and a menu entry). CI does not build its module.
const KERNEL_FEATURE_PLUGINS: &[&str] = &["trovato_comments"];

/// The password of every generated account.
pub const PASSWORD: &str = "perf-site-password-0";

/// The Redis database number performance sites use, and flush.
const REDIS_DB: u32 = 13;

/// The expectations file.
fn expectations_path() -> std::path::PathBuf {
    super::project_root().join("crates/kernel/tests/fixtures/performance_counts.toml")
}

/// Who is looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visitor {
    /// No session.
    Anonymous,
    /// A signed-in member (`gen_user_1`).
    User,
    /// The signed-in administrator (`gen_admin`).
    Admin,
}

impl Visitor {
    fn key(self) -> &'static str {
        match self {
            Visitor::Anonymous => "anonymous",
            Visitor::User => "user",
            Visitor::Admin => "admin",
        }
    }
}

/// The caches' state when the measured request starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caches {
    /// Every in-process cache cleared and the Redis cache emptied, except the
    /// cache generation counters and the visitors' sessions.
    Cold,
    /// The same request made once just before.
    Warm,
}

impl Caches {
    fn key(self) -> &'static str {
        match self {
            Caches::Cold => "cold",
            Caches::Warm => "warm",
        }
    }
}

/// What one measured request did, in the form the expectations file holds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Counts {
    /// Database statements.
    pub queries: u32,
    /// Cache lookups, in process and in Redis.
    pub cache_gets: u32,
    /// Lookups that missed.
    pub cache_misses: u32,
    /// Values written to the Redis cache.
    pub cache_sets: u32,
    /// Plugin exports called.
    pub plugin_calls: u32,
    /// Host functions plugins called.
    pub host_calls: u32,
    /// Gather queries run.
    pub gathers: u32,
    /// Template renders entered.
    pub renders: u32,
    /// Each distinct statement, by fingerprint: how many times it ran and its
    /// normalised text (every literal and parameter a `?`), so a change to the
    /// file shows in review as the statement it is.
    pub statements: BTreeMap<String, Statement>,
}

/// One distinct statement of a request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Statement {
    /// How many times the request ran it.
    pub count: u32,
    /// The normalised statement.
    pub sql: String,
}

impl Counts {
    fn of(s: &RequestProfileSnapshot) -> Self {
        let mut statements: BTreeMap<String, Statement> = BTreeMap::new();
        for (fingerprint, sql) in s.fingerprints.iter().zip(&s.statements) {
            statements
                .entry(fingerprint.clone())
                .or_insert_with(|| Statement {
                    count: 0,
                    sql: sql.clone(),
                })
                .count += 1;
        }
        Self {
            queries: s.queries,
            cache_gets: s.cache_gets,
            cache_misses: s.cache_misses,
            cache_sets: s.cache_sets,
            plugin_calls: s.plugin_calls,
            host_calls: s.host_calls,
            gathers: s.gathers,
            renders: s.renders,
            statements,
        }
    }

    /// What differs between `self` (expected) and `actual`, or `None`.
    pub fn mismatch(&self, actual: &Counts) -> Option<String> {
        if self == actual {
            return None;
        }
        let mut out = String::new();
        for (name, e, a) in [
            ("queries", self.queries, actual.queries),
            ("cache_gets", self.cache_gets, actual.cache_gets),
            ("cache_misses", self.cache_misses, actual.cache_misses),
            ("cache_sets", self.cache_sets, actual.cache_sets),
            ("plugin_calls", self.plugin_calls, actual.plugin_calls),
            ("host_calls", self.host_calls, actual.host_calls),
            ("gathers", self.gathers, actual.gathers),
            ("renders", self.renders, actual.renders),
        ] {
            if e != a {
                out.push_str(&format!("  {name}: expected {e}, got {a}\n"));
            }
        }
        let keys: std::collections::BTreeSet<&String> = self
            .statements
            .keys()
            .chain(actual.statements.keys())
            .collect();
        for key in keys {
            let (e, a) = (self.statements.get(key), actual.statements.get(key));
            let count = |s: Option<&Statement>| s.map_or(0, |s| s.count);
            if count(e) != count(a) {
                let sql = a.or(e).map_or("", |s| s.sql.as_str());
                out.push_str(&format!(
                    "  {key}: expected {}x, got {}x\n      {sql}\n",
                    count(e),
                    count(a)
                ));
            }
        }
        Some(out)
    }
}

/// One measured request.
pub struct Visit {
    /// The response status.
    pub status: u16,
    /// What it did.
    pub counts: Counts,
    /// The key it is recorded under: `page.visitor.caches`.
    pub key: String,
    /// The page it answered.
    pub body: String,
}

/// One write [`PerfSite::write`] sent.
pub struct Written {
    /// What it did.
    pub visit: Visit,
    /// How long the request took.
    pub elapsed: std::time::Duration,
    /// The cookies to send next.
    pub cookies: String,
}

/// A site of its own for performance tests.
pub struct PerfSite {
    /// The app, on the scratch database.
    pub app: TestApp,
    /// The scratch database. Never dropped, as the site is a `static`: the
    /// next run's `ScratchDb::drop_stale` removes it.
    _db: trovato_test_utils::ScratchDb,
    redis: redis::Client,
    /// The newest post's page, with three comments.
    pub item_path: String,
    user_cookies: String,
    admin_cookies: String,
    /// Held for a whole visit: tests in a binary run in parallel, and one
    /// test emptying the caches must not land inside another's request.
    one_at_a_time: tokio::sync::Mutex<()>,
}

/// The Redis URL with the database number replaced by [`REDIS_DB`].
fn perf_redis_url() -> String {
    let base = trovato_test_utils::env::redis_url();
    let (base, query) = base
        .split_once('?')
        .map_or((base.as_str(), None), |(b, q)| (b, Some(q)));
    let after_scheme = base.find("://").map_or(0, |p| p + 3);
    let host_part = match base[after_scheme..].find('/') {
        Some(p) => &base[..after_scheme + p],
        None => base,
    };
    match query {
        Some(q) => format!("{host_part}/{REDIS_DB}?{q}"),
        None => format!("{host_part}/{REDIS_DB}"),
    }
}

/// What `trovato generate` writes for the performance site.
pub fn site_options() -> trovato_kernel::generate::GenerateOptions {
    trovato_kernel::generate::GenerateOptions {
        items: 40,
        // Three on each post, by three different members (`users` shares no
        // factor with `items`), so a per-comment cost shows on the item page.
        comments: 120,
        tags: 6,
        users: 7,
        seed: 1,
        batch: 1000,
        parallel: 1,
    }
}

/// A directory holding links to the [`SITE_PLUGINS`] directories only.
fn site_plugins_dir() -> std::path::PathBuf {
    // A fixed place under `target/`, replaced each run, so runs leave nothing.
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("perf-plugins");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the perf plugin directory");
    for name in SITE_PLUGINS {
        let source = super::project_root().join("plugins").join(name);
        assert!(
            source.join(format!("{name}.wasm")).exists(),
            "the performance site needs {name}.wasm: build it as CI does"
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, dir.join(name)).expect("link a site plugin");
    }
    dir
}

impl PerfSite {
    /// Build the site. Slow (a database, migrations, a generated site): one
    /// per test binary, through [`shared_site`].
    pub async fn start() -> Self {
        // The site lives in a `static`, which is never dropped: clear what
        // earlier runs left before making this one's.
        trovato_test_utils::ScratchDb::drop_stale("perf").await;
        let db = trovato_test_utils::ScratchDb::create("perf").await;
        trovato_kernel::db::run_migrations(db.pool())
            .await
            .expect("migrate the performance site");
        let redis_url = perf_redis_url();
        let redis = redis::Client::open(redis_url.as_str()).expect("perf Redis URL");
        {
            let mut conn = redis
                .get_multiplexed_async_connection()
                .await
                .expect("perf Redis");
            let _: () = redis::cmd("FLUSHDB")
                .query_async(&mut conn)
                .await
                .expect("flush the perf Redis database");
        }
        let plugins = site_plugins_dir();
        let url = db.url();
        let app = TestApp::for_site(|c| {
            c.database_url = url;
            c.redis_url = redis_url;
            c.plugins_dirs = vec![plugins];
            c.disabled_plugins = Vec::new();
            c.runtime.profile_fingerprints = true;
            // Mail configured, so the signup and comment paths queue their
            // messages (BG18); no worker runs in a test, so nothing is sent.
            c.smtp_host = Some("127.0.0.1".to_string());
            c.smtp_port = 9;
            c.smtp_encryption = "none".to_string();
        })
        .await;
        for name in KERNEL_FEATURE_PLUGINS {
            app.ensure_plugin_enabled(name).await;
        }
        app.state.init_comments_service();

        trovato_kernel::generate::generate(&app.db, &site_options(), PASSWORD, |_| {})
            .await
            .expect("generate the performance site");
        // Open registration for the signup measurement (BG18) here, before
        // anything is measured: saved from the test, it would land inside
        // another test's measured request, as tests in a target run in
        // parallel.
        trovato_kernel::models::RegistrationMode::Open
            .save(&app.db)
            .await
            .expect("open registration");
        // The generator wrote site settings the app had already read.
        app.state.clear_local_caches().await;

        let item_id: uuid::Uuid = sqlx::query_scalar("SELECT md5('1:item:1')::uuid")
            .fetch_one(&app.db)
            .await
            .expect("the newest post");
        let user_cookies = app.login("gen_user_1", PASSWORD).await;
        let admin_cookies = app.login("gen_admin", PASSWORD).await;
        // A signed-in session's first request registers it in the session
        // index (and writes an audit row): do that now, not in a measured
        // request.
        for cookies in [&user_cookies, &admin_cookies] {
            let response = app
                .request(
                    Request::get("/")
                        .header("cookie", cookies.as_str())
                        .body(Body::empty())
                        .expect("request"),
                )
                .await;
            let _ = to_bytes(response.into_body(), usize::MAX).await;
        }
        super::settle_cache_generations().await;

        Self {
            app,
            _db: db,
            redis,
            item_path: format!("/item/{item_id}"),
            user_cookies,
            admin_cookies,
            one_at_a_time: tokio::sync::Mutex::new(()),
        }
    }

    fn cookies(&self, visitor: Visitor) -> &str {
        match visitor {
            Visitor::Anonymous => "",
            Visitor::User => &self.user_cookies,
            Visitor::Admin => &self.admin_cookies,
        }
    }

    /// The session ids the two signed-in visitors hold, which a cold start
    /// must not delete.
    fn session_keys(&self) -> Vec<String> {
        [&self.user_cookies, &self.admin_cookies]
            .iter()
            .flat_map(|c| c.split(';'))
            .filter_map(|kv| kv.trim().split_once('='))
            .map(|(_, v)| v.to_string())
            .collect()
    }

    /// Empty every cache: the in-process ones, and every Redis key but the
    /// sessions, the session index (without its entry a registered session
    /// counts as revoked and is ended) and the cache generation counters
    /// (deleting those would make the generation watcher clear caches at a
    /// moment of its choosing).
    async fn make_cold(&self) {
        self.make_cold_keeping("").await;
    }

    /// [`Self::make_cold`], keeping the sessions `cookies` holds too.
    async fn make_cold_keeping(&self, cookies: &str) {
        self.app.state.clear_local_caches().await;
        let mut keep = self.session_keys();
        keep.extend(
            cookies
                .split(';')
                .filter_map(|kv| kv.trim().split_once('='))
                .map(|(_, v)| v.to_string()),
        );
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .expect("perf Redis");
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg("*")
            .query_async(&mut conn)
            .await
            .expect("list perf Redis keys");
        for key in keys {
            if key == "cache:gen" || key.starts_with("user_sessions:") || keep.contains(&key) {
                continue;
            }
            let _: () = redis::cmd("DEL")
                .arg(&key)
                .query_async(&mut conn)
                .await
                .expect("delete a perf Redis key");
        }
    }

    async fn request(
        &self,
        visitor: Visitor,
        path: &str,
        extra_query: bool,
    ) -> (u16, RequestProfileSnapshot, String) {
        let mut request = Request::get(path);
        let cookies = self.cookies(visitor);
        if !cookies.is_empty() {
            request = request.header("cookie", cookies);
        }
        if extra_query {
            request = request.header("x-perf-extra-query", "1");
        }
        let response = self
            .app
            .request(request.body(Body::empty()).expect("request"))
            .await;
        let status = response.status().as_u16();
        let snapshot = response
            .extensions()
            .get::<RequestProfileSnapshot>()
            .cloned()
            .expect("the timing layer leaves a profile in every response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        (status, snapshot, body)
    }

    /// POST `json` to `path` as `visitor` and say what the write did, under
    /// the key `page.visitor.post` (BG18: the signup and comment paths, where
    /// queueing mail must add one insert and nothing else).
    ///
    /// The front page is loaded first, for the CSRF token a signed-in API
    /// write needs; that request is not measured.
    pub async fn submit(
        &self,
        page: &str,
        visitor: Visitor,
        path: &str,
        json: serde_json::Value,
    ) -> Visit {
        let _turn = self.one_at_a_time.lock().await;
        // From a known cache state, as a cold visit is: tests run in parallel,
        // so whatever ran just before must not decide what this request finds
        // cached. The token page then warms the same caches every time.
        self.make_cold().await;
        let (_, _, html) = self.request(visitor, "/", false).await;
        let marker = "name=\"csrf-token\" content=\"";
        let token = html
            .find(marker)
            .and_then(|start| html[start + marker.len()..].split('"').next())
            .unwrap_or_default()
            .to_string();
        super::settle_cache_generations().await;

        let mut request = Request::post(path)
            .header("content-type", "application/json")
            .header("x-csrf-token", token);
        let cookies = self.cookies(visitor);
        if !cookies.is_empty() {
            request = request.header("cookie", cookies);
        }
        let response = self
            .app
            .request(request.body(Body::from(json.to_string())).expect("request"))
            .await;
        let status = response.status().as_u16();
        let snapshot = response
            .extensions()
            .get::<RequestProfileSnapshot>()
            .cloned()
            .expect("the timing layer leaves a profile in every response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        Visit {
            status,
            counts: Counts::of(&snapshot),
            key: format!("{page}.{}.post", visitor.key()),
            body,
        }
    }
    /// The session cookies `visitor` signs in with.
    pub fn cookies_of(&self, visitor: Visitor) -> &str {
        self.cookies(visitor)
    }

    /// Load `page` with `cookies`, then send the write `post` makes of that
    /// page's HTML, and say what the write did under `key`, how long it took
    /// (the request alone, from sending it to reading its whole body), and
    /// the cookies to send next (the ones it reissued, or `cookies`).
    ///
    /// With `cold`, the caches are emptied before the page loads, as
    /// [`Self::submit`] does: the page then warms the same caches on every
    /// run, so the write's counts are the same on every run too. Without it
    /// nothing is emptied, for timing a write the way a busy site serves it.
    pub async fn write(
        &self,
        key: &str,
        cookies: &str,
        page: &str,
        cold: bool,
        post: impl FnOnce(&str) -> Request<Body>,
    ) -> Written {
        let _turn = self.one_at_a_time.lock().await;
        if cold {
            self.make_cold_keeping(cookies).await;
        }
        let mut get = Request::get(page);
        if !cookies.is_empty() {
            get = get.header("cookie", cookies);
        }
        let response = self
            .app
            .request(get.body(Body::empty()).expect("request"))
            .await;
        assert_eq!(response.status().as_u16(), 200, "{key}: {page} did not load");
        let html = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let mut request = post(&html);
        if !cookies.is_empty() {
            request
                .headers_mut()
                .insert("cookie", cookies.parse().expect("cookie header"));
        }
        if cold {
            super::settle_cache_generations().await;
        }

        let started = std::time::Instant::now();
        let response = self.app.request(request).await;
        let status = response.status().as_u16();
        let reissued = super::extract_cookies(&response);
        let snapshot = response
            .extensions()
            .get::<RequestProfileSnapshot>()
            .cloned()
            .expect("the timing layer leaves a profile in every response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let elapsed = started.elapsed();

        Written {
            visit: Visit {
                status,
                counts: Counts::of(&snapshot),
                key: key.to_string(),
                body,
            },
            elapsed,
            cookies: if reissued.is_empty() {
                cookies.to_string()
            } else {
                reissued
            },
        }
    }

    /// Load `path` as `visitor` with `caches`, and say what the request did.
    pub async fn visit(&self, page: &str, visitor: Visitor, caches: Caches, path: &str) -> Visit {
        self.visit_with(page, visitor, caches, path, false).await
    }

    /// [`Self::visit`], optionally asking the test router for one more
    /// statement.
    pub async fn visit_with(
        &self,
        page: &str,
        visitor: Visitor,
        caches: Caches,
        path: &str,
        extra_query: bool,
    ) -> Visit {
        let _turn = self.one_at_a_time.lock().await;
        match caches {
            Caches::Cold => self.make_cold().await,
            Caches::Warm => {
                self.request(visitor, path, false).await;
            }
        }
        // Writes made by the request before (a login, a view count) bump
        // cache generations, and the watcher acts on a bump up to 250 ms
        // later: let it, so it never lands inside the measured request.
        super::settle_cache_generations().await;
        let (status, snapshot, body) = self.request(visitor, path, extra_query).await;
        let key = format!("{page}.{}.{}", visitor.key(), caches.key());
        // The member's visits are signed in and the anonymous ones are not,
        // or the counts are of the wrong visitor. (The administrator's pages
        // answer 200 only when signed in.)
        let signed_in = body.contains("site-nav__logout");
        match visitor {
            Visitor::User => assert!(signed_in, "{key}: the member was not signed in"),
            Visitor::Anonymous => assert!(!signed_in, "{key}: the visitor was signed in"),
            Visitor::Admin => {}
        }
        Visit {
            status,
            counts: Counts::of(&snapshot),
            key,
            body,
        }
    }
}

/// The site every performance test in a binary shares.
static SITE: OnceLock<PerfSite> = OnceLock::new();

/// The shared [`PerfSite`], built on the shared runtime on first use.
pub fn shared_site() -> &'static PerfSite {
    // Statements are counted by the query listener, as in production
    // (`init_tracing`). The binary's one global subscriber, installed once
    // even when building the site fails, so a second test reports that
    // failure rather than a second subscriber.
    static SUBSCRIBER: std::sync::Once = std::sync::Once::new();
    SUBSCRIBER.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt;
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry()
                .with(trovato_kernel::telemetry::query_listener::query_listener()),
        )
        .expect("the performance tests install the only subscriber");
    });
    SITE.get_or_init(|| {
        let handle = super::SHARED_RT.handle().clone();
        std::thread::spawn(move || handle.block_on(PerfSite::start()))
            .join()
            .expect("performance site init thread panicked")
    })
}

/// The expectations, read once; rewritten in record mode.
static EXPECTED: Mutex<Option<BTreeMap<String, Counts>>> = Mutex::new(None);

fn recording() -> bool {
    std::env::var_os("TROVATO_PERF_RECORD").is_some()
}

/// The expected counts for `key`, or `None` if the file has none.
pub fn expected(key: &str) -> Option<Counts> {
    let mut guard = EXPECTED.lock().unwrap_or_else(|p| p.into_inner());
    let map = guard.get_or_insert_with(read_expectations);
    map.get(key).cloned()
}

/// The expectations file, parsed; empty if there is none. A file that does
/// not parse is a failure, not an empty file: recording over it would drop
/// every table the run did not visit.
fn read_expectations() -> BTreeMap<String, Counts> {
    let path = expectations_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => panic!("cannot read {}: {e}", path.display()),
    }
}

/// Record `counts` under `key` and rewrite the file (record mode only). The
/// file is read first, so recording one test keeps every other table.
fn record(key: &str, counts: &Counts) {
    let mut guard = EXPECTED.lock().unwrap_or_else(|p| p.into_inner());
    let map = guard.get_or_insert_with(read_expectations);
    map.insert(key.to_string(), counts.clone());
    let header = "# Expected work per page, read by tests/performance_test.rs.\n\
                  # One table per page.visitor.caches. Regenerate with\n\
                  #   TROVATO_PERF_RECORD=1 cargo test -p trovato-kernel --test performance_test -- --test-threads=1\n\
                  # and commit the change with the code that caused it.\n\n";
    let body = toml::to_string_pretty(map).expect("serialize expectations");
    std::fs::write(expectations_path(), format!("{header}{body}")).expect("write expectations");
}

/// Check `visit` against the file: panics with the difference, or records it.
pub fn check(visit: &Visit) {
    check_answered(visit, 200);
}

/// [`check`], for a request whose success answers `status` (a redirect after
/// a form, `201` after a create).
pub fn check_answered(visit: &Visit, status: u16) {
    assert_eq!(
        visit.status, status,
        "{} answered {}: {}",
        visit.key, visit.status, visit.body
    );
    // A page with empty caches reads the database: none counted means the
    // counting is broken, not that the page is free.
    assert!(
        !visit.key.ends_with(".cold") || visit.counts.queries > 0,
        "{} counted no statement with cold caches: is the query listener installed?",
        visit.key
    );
    if recording() {
        record(&visit.key, &visit.counts);
        return;
    }
    let Some(expected) = expected(&visit.key) else {
        panic!(
            "no expected counts for {} in {}; record them with TROVATO_PERF_RECORD=1.\nActual: {:#?}",
            visit.key,
            expectations_path().display(),
            visit.counts
        );
    };
    if let Some(diff) = expected.mismatch(&visit.counts) {
        panic!(
            "{} does different work than tests/fixtures/performance_counts.toml expects:\n{diff}\
             If the change is intended, update the file in the same commit \
             (TROVATO_PERF_RECORD=1 rewrites it).",
            visit.key
        );
    }
}
