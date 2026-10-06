#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Common test utilities for integration tests.
//!
//! This module provides test infrastructure that uses the REAL kernel code,
//! not mock implementations. This ensures tests verify actual behavior.
//!
//! A single [`TestApp`] instance is shared across all tests via [`shared_app`]
//! to avoid exhausting virtual memory — each wasmtime pooling allocator
//! reserves ~64 GB of address space.
//!
//! ## Runtime Safety
//!
//! The shared `TestApp` is initialized on a long-lived, multi-threaded Tokio
//! runtime that outlives any individual `#[tokio::test]` runtime. This prevents
//! 500 errors from session-layer Redis connections being dropped when the
//! initializing test's runtime shuts down.

#![allow(dead_code)]

pub mod smtp_sink;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use axum::response::Response;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use trovato_kernel::{AppState, Config, ConfigStorage};

/// Shared Tokio runtime that outlives all individual test runtimes.
///
/// PgPool and Redis connections need an active I/O driver. By keeping this
/// runtime alive for the entire test binary, the shared `TestApp`'s connections
/// remain valid across all tests.
///
/// All tests run on this runtime via [`run_test`] to prevent cross-runtime
/// connection migration (connections opened on one runtime becoming stale
/// when that runtime shuts down).
pub static SHARED_RT: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to build shared test runtime")
    });

/// Advisory-lock key guarding the shared conference fixture seeding.
///
/// `ensure_conference_type` / `ensure_conference_items` are check-then-insert,
/// and their "Idempotent — safe to call from any test" promise was false under
/// concurrency: three `gather_semantic_test` tests call the seeder in parallel,
/// two observe `EXISTS = false` in the same window, and both insert. On a virgin
/// database that file alone reliably produced TWO of each conference, which is
/// what made `tutorial_test`'s "exactly 3 hand-created conferences" assertion
/// fail under `cargo test --all` while passing in isolation.
///
/// A Postgres advisory lock rather than a process-local mutex, deliberately:
/// every test binary shares one database, so the mutual exclusion has to be in
/// the database. That also matters more than it used to — CI now runs the test
/// targets in three shards, so what serializes them has to work across
/// processes.
const CONFERENCE_SEED_LOCK: i64 = 0x_C0FF_EE00_0001;

/// Global shared test app — initialized once on the shared runtime, reused
/// by every test.
static SHARED_APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

/// Get a reference to the shared [`TestApp`].
///
/// The app is lazily initialized on first call and reused thereafter.
/// Initialization runs on a dedicated multi-thread Tokio runtime (via
/// `SHARED_RT`) so that async resources survive across tests.
pub async fn shared_app() -> &'static TestApp {
    SHARED_APP.get_or_init(|| {
        // Use the shared runtime's handle to initialize inside a
        // separate OS thread (avoiding nested block_on).
        let handle = SHARED_RT.handle().clone();
        std::thread::spawn(move || handle.block_on(TestApp::new()))
            .join()
            .expect("TestApp init thread panicked")
    })
}

/// A handle to the shared runtime, for test files that need to initialize their
/// own app on it (see `recovery_plugin_flow_test`).
pub fn shared_runtime_handle() -> tokio::runtime::Handle {
    SHARED_RT.handle().clone()
}

/// Run an async test body on [`SHARED_RT`].
///
/// Using a single runtime for all tests prevents the "Tokio context is being
/// shutdown" error that occurs when PgPool connections opened on one
/// `#[tokio::test]` runtime are reused by another after the first shuts down.
///
/// It is also where anything the test registered with [`defer_cleanup`] is
/// awaited, including while a failing assert unwinds — which is the point:
/// teardown written at the bottom of a test body is teardown that is skipped on
/// exactly the run where it mattered.
pub fn run_test<F: std::future::Future<Output = ()> + Send>(f: F) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| SHARED_RT.block_on(f)));
    run_deferred_cleanups();
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

// =============================================================================
// Cleanup that runs even when the test fails
// =============================================================================

/// A cleanup the running test registered, awaited once that test ends.
///
/// An `FnOnce` returning a boxed future rather than a stored future: a future
/// built at registration time would have to be polled to do anything, and the
/// whole point is that it is polled later, after the body has either finished
/// or unwound.
type DeferredCleanup =
    Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>;

thread_local! {
    /// Cleanups registered by the test running on this thread.
    ///
    /// Thread-local rather than global: [`run_test`] polls the body on the
    /// calling thread, which is libtest's own per-test thread, so two tests
    /// running in parallel never see each other's entries and neither can tear
    /// down rows the other is still using.
    static DEFERRED_CLEANUPS: std::cell::RefCell<Vec<DeferredCleanup>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register `f` to run when the current test ends, whether it passes or fails.
///
/// For state that outlives a test and that the kernel will not reclaim on its
/// own: a role, an AI usage row, a per-user budget override. Register it
/// immediately after creating the thing, not at the bottom of the body — a
/// cleanup below a failing assert never runs, and the next run of the suite
/// inherits the mess.
///
/// Cleanups run last-registered-first, like nested `Drop`s, so one that depends
/// on another's rows still finds them.
pub fn defer_cleanup<F, Fut>(f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    DEFERRED_CLEANUPS.with(|pending| pending.borrow_mut().push(Box::new(move || Box::pin(f()))));
}

/// Drain and await every cleanup the finished test registered.
///
/// One at a time, with the borrow released around each `block_on`, so a cleanup
/// is free to register another.
fn run_deferred_cleanups() {
    while let Some(cleanup) = DEFERRED_CLEANUPS.with(|pending| pending.borrow_mut().pop()) {
        SHARED_RT.block_on(cleanup());
    }
}

/// Delete `role_id` when the current test ends.
///
/// Every role a test creates has to go again, because
/// `/admin/people/permissions` renders every permission for every role on one
/// page. Left to accumulate at roughly a hundred a run, the grid outgrew the
/// 4 MB body cap its own tests read it under, and the suite stopped being
/// runnable twice against one database — a page size limit standing in for the
/// real defect, which is that tests did not own their state.
pub fn track_test_role(pool: &PgPool, role_id: Uuid) {
    let pool = pool.clone();
    defer_cleanup(move || async move {
        let _ = trovato_kernel::models::Role::delete(&pool, role_id).await;
    });
}

/// Test application wrapper using the REAL kernel routes and state.
pub struct TestApp {
    router: Router,
    pub db: PgPool,
    pub state: AppState,
}

/// The project root, resolved from this crate's manifest directory.
///
/// Integration tests run with `crates/kernel/` as the working directory, so any
/// path a fixture points the kernel at has to be built from here rather than
/// left relative.
pub fn project_root() -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    std::path::Path::new(&manifest_dir)
        .parent() // crates/
        .and_then(|p| p.parent()) // project root
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf()
}

/// Languages every test app knows about, beyond the `en` the kernel migration
/// seeds as the default.
///
/// A one-language fixture cannot exercise anything multilingual: language
/// prefixes are only stripped for *known* non-default languages, so `/it/x` on a
/// monolingual site is just a 404 and no test could tell a working translation
/// from a broken one. `he` is here for its direction rather than its content —
/// it is the only way to see `text_direction` take a value that is not the
/// default.
const EXTRA_TEST_LANGUAGES: &[(&str, &str, &str)] =
    &[("it", "Italian", "ltr"), ("he", "Hebrew", "rtl")];

/// Applied once per test binary; see [`ensure_database_migrated`].
static DATABASE_MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Bring the test database up to date, before anything here reads a table.
///
/// `AppState::new` runs the migrations itself, which looks like enough and is
/// not: the fixtures around it touch the database *first*. This module seeds
/// `language` before building the app, because `AppState` snapshots the known
/// languages at construction, and several test files install a plugin before
/// that again, because `AppState` resolves its enabled plugin set at
/// construction too. On a database that has never been migrated every one of
/// those fails on a table that does not exist yet, and `cargo test` after a
/// bare `createdb` collapsed at the first target that needed the shared app.
///
/// CI hid it by running `sqlx migrate run` before the test job, so the property
/// nobody could check was the one a new contributor hits first.
///
/// Idempotent and cheap to call: the `OnceCell` makes it one statement per test
/// binary, and `MIGRATOR` takes its own advisory lock, so the binaries sharing a
/// database cannot race each other.
pub async fn ensure_database_migrated(database_url: &str) {
    DATABASE_MIGRATED
        .get_or_init(|| async {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .connect(database_url)
                .await
                .unwrap_or_else(|e| panic!("connect to migrate the test database: {e}"));
            trovato_kernel::db::run_migrations(&pool)
                .await
                .unwrap_or_else(|e| panic!("migrate the test database: {e}"));
            pool.close().await;
        })
        .await;
}

/// Insert [`EXTRA_TEST_LANGUAGES`], idempotently, on a short-lived pool.
///
/// Its own connection rather than the app's: this has to run before
/// `AppState::new`, which is what builds the app's pool.
async fn seed_test_languages(database_url: &str) {
    let pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
    {
        Ok(pool) => pool,
        Err(e) => panic!("connect to seed test languages: {e}"),
    };

    for (id, label, direction) in EXTRA_TEST_LANGUAGES {
        sqlx::query(
            "INSERT INTO language (id, label, weight, is_default, direction) \
             VALUES ($1, $2, 10, false, $3) ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .bind(label)
        .bind(direction)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("seed language '{id}': {e}"));
    }

    pool.close().await;
}

impl TestApp {
    /// Create a new test application with full kernel initialization.
    pub async fn new() -> Self {
        Self::with_config(|_| {}).await
    }

    /// Build a test app, adjusting the loaded [`Config`] before the app is made.
    ///
    /// Prefer this over setting an environment variable. `Config` is an owned
    /// value handed to `AppState::new`, so a field override steers this app and
    /// nothing else — where a `set_var` steers every other test thread in the
    /// binary too, and cannot be undone once another thread may have read it.
    pub async fn with_config(customize: impl FnOnce(&mut Config)) -> Self {
        // `dotenvy::dotenv` calls `set_var` for every line it applies, so it goes
        // through the workspace env lock like any other mutation. It is also the
        // last environment *mutation* left in this fixture: everything below is a
        // field override, because `Config` now owns every value the kernel used
        // to re-read from the environment at the point of use.
        trovato_test_utils::env::load_dotenv();

        let project_root = project_root();

        // Create config from environment
        let mut config = Config::from_env().expect("Failed to load config");

        // Tests run from `crates/kernel/`, so the defaults (`./templates`,
        // `./static`) resolve to nothing. These used to be `set_env_default`
        // writes, because the theme engine and the static-file handler read the
        // variables themselves; they are `Config` fields now, so pointing them at
        // the real directories steers this app and touches nothing global.
        //
        // Each still defers to the environment when it says something, so a
        // deployment-shaped run can override them exactly as before.
        if std::env::var_os("TEMPLATES_DIR").is_none() {
            config.templates_dirs = vec![project_root.join("templates")];
        }
        if std::env::var_os("STATIC_DIR").is_none() {
            config.runtime.static_dirs = vec![project_root.join("static")];
        }

        // Trust the mocked loopback peer (see MockConnectInfo below) so the
        // per-test X-Forwarded-For isolation `login` relies on passes the RATE-1
        // trusted-proxy gate.
        if std::env::var_os("TRUSTED_PROXIES").is_none() {
            config.trusted_proxies = vec![std::net::IpAddr::from([127, 0, 0, 1])];
        }

        // Tests run 100 tests concurrently — bump the default pool size so
        // serialization locks don't starve other tests of connections.
        if std::env::var_os("DATABASE_MAX_CONNECTIONS").is_none() {
            config.database_max_connections = 25;
        }

        customize(&mut config);

        // Migrate before seeding, not after: `AppState::new` below runs the
        // migrations, and the seed on the next line writes to a table one of
        // them creates.
        ensure_database_migrated(&config.database_url).await;

        // Seed the extra languages before the app reads them. `AppState` snapshots
        // `known_languages` and `default_language` once at construction, so a
        // language inserted afterwards is invisible to negotiation for the life of
        // the app — it has to be in the table first.
        seed_test_languages(&config.database_url).await;

        // Initialize the REAL AppState (database, redis, plugins, templates, etc.)
        let state = AppState::new(&config)
            .await
            .expect("Failed to initialize AppState");

        let db = state.db().clone();

        // Create session layer
        let session_layer = trovato_kernel::session::create_session_layer(
            &config.redis_url,
            tower_sessions::cookie::SameSite::Strict,
        )
        .await
        .expect("Failed to create session layer");

        // Build the REAL router with all kernel routes (must match main.rs).
        //
        // Path alias resolution uses a fallback handler, not middleware, because
        // in Axum 0.8 Router::layer() middleware runs AFTER route matching — URI
        // rewrites in middleware cannot change which route is matched. The
        // fallback receives all unmatched requests, resolves any URL alias, and
        // re-dispatches to the inner router with the rewritten URI.
        let inner_router: Router<trovato_kernel::state::AppState> = Router::new()
            .merge(trovato_kernel::routes::front::router())
            .merge(trovato_kernel::routes::install::router())
            .merge(trovato_kernel::routes::auth::router())
            .merge(trovato_kernel::routes::user_delete::router())
            .merge(trovato_kernel::routes::admin::router())
            .merge(trovato_kernel::routes::password_reset::router())
            .merge(trovato_kernel::routes::webauthn::router())
            .merge(trovato_kernel::routes::sessions::router())
            .merge(trovato_kernel::routes::recovery::router())
            .merge(trovato_kernel::routes::health::router())
            .merge(trovato_kernel::routes::item::router())
            .merge(trovato_kernel::routes::gather::router())
            .merge(trovato_kernel::routes::gather_admin::router())
            .merge(trovato_kernel::routes::plugin_admin::router())
            .merge(trovato_kernel::routes::search::router())
            .merge(trovato_kernel::routes::cron::router())
            .merge(trovato_kernel::routes::file::router())
            .merge(trovato_kernel::routes::metrics::router())
            .merge(trovato_kernel::routes::batch::router())
            .merge(trovato_kernel::routes::api_token::router())
            .merge(trovato_kernel::routes::api_ai_assist::router())
            .merge(trovato_kernel::routes::api_chat::router())
            .merge(trovato_kernel::routes::assistant::router())
            .merge(trovato_kernel::routes::api_search::router())
            .merge(trovato_kernel::routes::api_v1::router())
            .merge(trovato_kernel::routes::tile_admin::router())
            .merge(trovato_kernel::routes::static_files::router())
            .merge(trovato_kernel::routes::sitemap::router())
            // Plugin-gated routes — runtime middleware returns 404 when disabled
            .merge(trovato_kernel::routes::gated_plugin_routes(&state))
            // RSS feeds declared by gather query display configs.
            .merge(trovato_kernel::routes::feed::build_feed_router(
                &state.gather().list_queries(),
            ))
            // Plugin-served API routes (G-NO-PLUGIN-HTTP).
            .merge(trovato_kernel::routes::plugin_api::build_plugin_api_router(
                &state.menu_registry().all().cloned().collect::<Vec<_>>(),
            ));

        let inner_with_state: Router = inner_router.clone().with_state(state.clone());
        let shared_router = std::sync::Arc::new(inner_with_state);

        let router = inner_router
            .fallback({
                let router = shared_router.clone();
                let app_state = state.clone();
                move |session: tower_sessions::Session, request: axum::extract::Request| {
                    let router = router.clone();
                    let app_state = app_state.clone();
                    async move {
                        trovato_kernel::middleware::path_alias_fallback(
                            app_state, session, router, request,
                        )
                        .await
                    }
                }
            })
            // Middleware layers (must match main.rs ordering):
            // MockConnectInfo → resolve_client_ip → TraceLayer → session →
            // api_token → negotiate_language → routes
            // FR-7b: maintain the per-user session index. Inside the session
            // layer so it can read the session the handler just wrote.
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::track_session,
            ))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::negotiate_language,
            ))
            // `Authorization: Bearer <api-token>` authentication, as in
            // production. Absent here until K1, which made the difference
            // observable: `routes::plugin_api` exempts a *bearer*-authenticated
            // write from CSRF (G-CSRF-NO-BEARER-BYPASS) and that posture cannot
            // be tested through a harness that never runs the middleware.
            // Inside the session layer, which it writes the user id into; a
            // request with no Authorization header passes straight through.
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::authenticate_api_token,
            ))
            .layer(session_layer)
            // Resolve the trusted-proxy-gated client IP (RATE-1) so handlers see
            // the `ClientIp` extension exactly as in production.
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::resolve_client_ip,
            ))
            // Security response headers, outside the session layer as in
            // production. Without it no test could see the CSP a browser
            // enforces, which is how an inline script the policy blocks shipped
            // on the login page. It only adds headers.
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::inject_security_headers,
            ))
            // Request timing, outermost as in production, so a test can see the
            // `Server-Timing` header the middleware is there to add.
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::track_request_timing,
            ))
            .layer(tower_http::trace::TraceLayer::new_for_http())
            // Supply a loopback peer (outermost, runs first) so
            // `resolve_client_ip` has a trusted socket address — mirroring
            // `into_make_service_with_connect_info` in production. Inserted
            // explicitly (rather than via MockConnectInfo) so the request
            // extension is unambiguously present before resolve runs.
            .layer(axum::middleware::from_fn(
                |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
                    req.extensions_mut().insert(axum::extract::ConnectInfo(
                        std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    ));
                    next.run(req).await
                },
            ))
            .with_state(state.clone());

        // Pre-warm all pool connections on SHARED_RT so that no connection
        // is ever first created on a per-test #[tokio::test] runtime.
        // Without this, connections lazily opened on test runtimes become
        // invalid when those runtimes shut down, causing "Tokio context
        // is being shutdown" errors in later tests that reuse them.
        {
            let mut conns = Vec::new();
            for _ in 0..config.database_max_connections {
                if let Ok(c) = db.acquire().await {
                    conns.push(c);
                }
            }
            drop(conns);
        }

        // Note: We don't do global cleanup here because it interferes with parallel tests.
        // Each test should use unique identifiers and clean up its own data if needed.

        Self { router, db, state }
    }

    /// Get the config storage for direct access.
    pub fn config_storage(&self) -> &std::sync::Arc<dyn ConfigStorage> {
        self.state.config_storage()
    }

    /// Get the stage service for direct access.
    pub fn stage(&self) -> &std::sync::Arc<trovato_kernel::stage::StageService> {
        self.state.stage()
    }

    /// Clean up a specific test content type by machine name.
    ///
    /// The registry entry goes with the row: `AppState` fills the registry from
    /// the database at construction and `/admin/structure/types` lists the
    /// registry, so deleting only the row would leave this app advertising a
    /// type that no longer exists.
    pub async fn cleanup_content_type(&self, machine_name: &str) {
        sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(machine_name)
            .execute(&self.db)
            .await
            .ok();
        self.state.content_types().invalidate(machine_name);
    }

    /// Delete the content type `machine_name` when the current test ends.
    ///
    /// For the tests that create a type through the admin UI: they name it
    /// uniquely so they cannot collide, which also means every run leaves
    /// another one behind, and `/admin/structure/types` and
    /// `/admin/content/add` both list every type there is.
    pub fn cleanup_content_type_on_exit(&self, machine_name: &str) {
        let pool = self.db.clone();
        let registry = std::sync::Arc::clone(self.state.content_types());
        let machine_name = machine_name.to_string();
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item_type WHERE type = $1")
                .bind(&machine_name)
                .execute(&pool)
                .await;
            registry.invalidate(&machine_name);
        });
    }

    /// Put the content type `machine_name` back the way it is now when the
    /// current test ends, row and registry entry alike.
    ///
    /// For a test that modifies a type the whole suite shares. `page` is the one
    /// that matters: a test that adds a field there and does not take it away
    /// again leaves it for every later test and every later run. It used to
    /// leave another copy of the same field each time, because the admin form
    /// appended without checking for a name the type already had, and four runs
    /// in `page` had three `search_test_field` entries and the translation form
    /// stopped rendering any of the type's fields at all. The registry refuses
    /// the duplicate now, so the second add fails rather than accumulating; the
    /// field still has to go away again, which is what this does.
    pub async fn restore_content_type_on_exit(&self, machine_name: &str) {
        let row: Option<(String, Option<String>, serde_json::Value)> =
            sqlx::query_as("SELECT label, description, settings FROM item_type WHERE type = $1")
                .bind(machine_name)
                .fetch_optional(&self.db)
                .await
                .unwrap_or_else(|e| panic!("read '{machine_name}' before changing it: {e}"));

        let Some((label, description, settings)) = row else {
            // No such type yet: removing it again is the right restoration.
            self.cleanup_content_type_on_exit(machine_name);
            return;
        };

        let registry = std::sync::Arc::clone(self.state.content_types());
        let machine_name = machine_name.to_string();
        defer_cleanup(move || async move {
            // `create` upserts the row AND refreshes the cached definition,
            // which is what makes this a restoration rather than half of one.
            let _ = registry
                .create(&machine_name, &label, description.as_deref(), settings)
                .await;
        });
    }

    /// Send a request to the test application.
    pub async fn request(&self, request: Request<Body>) -> Response {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("Failed to send request")
    }

    /// Send a request with cookies from a previous response.
    pub async fn request_with_cookies(
        &self,
        mut request: Request<Body>,
        cookies: &str,
    ) -> Response {
        if !cookies.is_empty() {
            request.headers_mut().insert(
                header::COOKIE,
                cookies.parse().expect("Invalid cookie header"),
            );
        }
        self.request(request).await
    }

    /// Login via JSON API and return session cookies.
    ///
    /// Each login uses a per-username `X-Forwarded-For` header so that
    /// parallel tests don't share the same rate-limit bucket.
    ///
    /// # Panics
    ///
    /// Panics if the login response is not 200 OK (e.g. rate-limited or
    /// invalid credentials).
    pub async fn login(&self, username: &str, password: &str) -> String {
        // Clear lockout state so the user can log in.
        self.state.lockout().clear_all(username).await.ok();

        // Derive a unique fake IP from the username so each test gets its own
        // rate-limit bucket and parallel tests can't starve each other.
        let fake_ip = test_ip_for(username);
        self.state
            .rate_limiter()
            .reset("login", &fake_ip)
            .await
            .ok();

        let response = self
            .request(
                Request::post("/user/login/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &fake_ip)
                    .body(Body::from(
                        serde_json::json!({
                            "username": username,
                            "password": password
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;

        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Login failed for user '{username}' (status {})",
            response.status()
        );

        extract_cookies(&response)
    }

    /// Create a test user and return session cookies after logging in.
    pub async fn create_and_login_user(
        &self,
        username: &str,
        password: &str,
        email: &str,
    ) -> String {
        self.create_test_user(username, password, email).await;
        self.login(username, password).await
    }

    /// Create a test admin user and return session cookies after logging in.
    pub async fn create_and_login_admin(
        &self,
        username: &str,
        password: &str,
        email: &str,
    ) -> String {
        self.create_test_admin(username, password, email).await;
        self.login(username, password).await
    }

    /// Create a test admin user directly in the database.
    pub async fn create_test_admin(&self, username: &str, password: &str, email: &str) {
        self.create_test_user_inner(username, password, email, true)
            .await;
    }

    /// Create a test user directly in the database.
    pub async fn create_test_user(&self, username: &str, password: &str, email: &str) {
        self.create_test_user_inner(username, password, email, false)
            .await;
    }

    /// Ensure a plugin is installed in the DB and enabled in-memory.
    ///
    /// Tests that hit plugin-gated routes must call this to make the routes
    /// accessible, since CI starts with a clean database (no plugins installed).
    pub async fn ensure_plugin_enabled(&self, plugin_name: &str) {
        trovato_kernel::plugin::status::install_plugin(&self.db, plugin_name, "1.0.0")
            .await
            .expect("Failed to install plugin in test DB");
        self.state.set_plugin_enabled(plugin_name, true);
    }

    /// Ensure the `conference` item type exists with the 12-field tutorial schema.
    ///
    /// The tutorial walks users through creating this type via the admin UI.
    /// This method seeds the same structure programmatically. Idempotent — safe
    /// to call from any test.
    pub async fn ensure_conference_type(&self) {
        use trovato_sdk::types::{FieldDefinition, FieldType};

        // Serialize the whole check-then-insert against every other seeder, in
        // any thread or process, sharing this database. See
        // `CONFERENCE_SEED_LOCK` for why a plain EXISTS check is not enough.
        let mut tx = self.db.begin().await.expect("begin conference type seed");
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(CONFERENCE_SEED_LOCK)
            .execute(&mut *tx)
            .await
            .expect("take conference seed lock");

        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM item_type WHERE type = 'conference')")
                .fetch_one(&mut *tx)
                .await
                .unwrap();

        if exists {
            return;
        }

        let fields = vec![
            FieldDefinition::new("field_url", FieldType::Text { max_length: None })
                .label("Website URL"),
            FieldDefinition::new("field_start_date", FieldType::Date)
                .label("Start Date")
                .required(),
            FieldDefinition::new("field_end_date", FieldType::Date)
                .label("End Date")
                .required(),
            FieldDefinition::new("field_city", FieldType::Text { max_length: None }).label("City"),
            FieldDefinition::new("field_country", FieldType::Text { max_length: None })
                .label("Country"),
            FieldDefinition::new("field_online", FieldType::Boolean).label("Online"),
            FieldDefinition::new("field_cfp_url", FieldType::Text { max_length: None })
                .label("CFP URL"),
            FieldDefinition::new("field_cfp_end_date", FieldType::Date).label("CFP End Date"),
            FieldDefinition::new("field_description", FieldType::Blocks).label("Description"),
            FieldDefinition::new("field_language", FieldType::Text { max_length: None })
                .label("Language"),
            FieldDefinition::new("field_source_id", FieldType::Text { max_length: None })
                .label("Source ID"),
            FieldDefinition::new("field_editor_notes", FieldType::TextLong).label("Editor Notes"),
            FieldDefinition::new("field_logo", FieldType::File).label("Logo"),
            FieldDefinition::new("field_venue_photo", FieldType::File).label("Venue Photo"),
        ];

        let settings = serde_json::json!({
            "fields": serde_json::to_value(&fields).unwrap(),
            "title_label": "Conference Name",
        });

        sqlx::query(
            r#"INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings)
               VALUES ('conference', 'Conference', 'A tech conference or meetup event', true,
                       'Conference Name', 'core', $1)
               ON CONFLICT (type) DO NOTHING"#,
        )
        .bind(&settings)
        .execute(&mut *tx)
        .await
        .expect("failed to seed conference item type");

        tx.commit().await.expect("commit conference type seed");

        // Register in the content type cache so API endpoints see it
        self.state
            .content_types()
            .create(
                "conference",
                "Conference",
                Some("A tech conference or meetup event"),
                settings,
            )
            .await
            .ok(); // Ignore error if already cached
    }

    /// Seed the 3 tutorial conferences (RustConf 2026, EuroRust 2026,
    /// WasmCon Online 2026). Ensures the conference type exists first.
    /// Idempotent — safe to call from any test.
    pub async fn ensure_conference_items(&self) {
        self.ensure_conference_type().await;

        let now = chrono::Utc::now().timestamp();
        let nil_author = Uuid::nil();
        let live_stage = trovato_kernel::models::stage::LIVE_STAGE_ID;

        // One lock for the whole batch, so a concurrent seeder either sees all
        // three conferences or none — never a half-seeded set.
        let mut tx = self.db.begin().await.expect("begin conference item seed");
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(CONFERENCE_SEED_LOCK)
            .execute(&mut *tx)
            .await
            .expect("take conference seed lock");

        let conferences = [
            (
                "RustConf 2026",
                serde_json::json!({
                    "field_url": "https://rustconf.com",
                    "field_start_date": "2026-09-09",
                    "field_end_date": "2026-09-11",
                    "field_city": "Portland",
                    "field_country": "United States",
                    "field_cfp_url": "https://rustconf.com/cfp",
                    "field_cfp_end_date": "2026-06-15",
                    "field_description": "The official Rust conference, featuring talks on the latest Rust developments.",
                    "field_language": "en"
                }),
            ),
            (
                "EuroRust 2026",
                serde_json::json!({
                    "field_url": "https://eurorust.eu",
                    "field_start_date": "2026-10-15",
                    "field_end_date": "2026-10-16",
                    "field_city": "Paris",
                    "field_country": "France",
                    "field_description": "Europe's premier Rust conference, bringing together Rustaceans from across the continent.",
                    "field_language": "en"
                }),
            ),
            (
                "WasmCon Online 2026",
                serde_json::json!({
                    "field_url": "https://wasmcon.dev",
                    "field_start_date": "2026-07-22",
                    "field_end_date": "2026-07-23",
                    "field_online": "1",
                    "field_description": "A virtual conference dedicated to WebAssembly, covering toolchains, runtimes, and the component model.",
                    "field_language": "en"
                }),
            ),
        ];

        for (title, fields) in &conferences {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM item WHERE type = 'conference' AND title = $1)",
            )
            .bind(title)
            .fetch_one(&mut *tx)
            .await
            .unwrap();

            if exists {
                continue;
            }

            let item_id = Uuid::now_v7();
            let rev_id = Uuid::now_v7();

            sqlx::query(
                r#"INSERT INTO item (id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id)
                   VALUES ($1, 'conference', $2, $3, 1, $4, $4, 0, 0, $5, $6)"#,
            )
            .bind(item_id)
            .bind(title)
            .bind(nil_author)
            .bind(now)
            .bind(fields)
            .bind(live_stage)
            .execute(&mut *tx)
            .await
            .expect("failed to seed conference item");

            sqlx::query(
                r#"INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log)
                   VALUES ($1, $2, $3, $4, 1, $5, $6, 'Tutorial seed')"#,
            )
            .bind(rev_id)
            .bind(item_id)
            .bind(nil_author)
            .bind(title)
            .bind(fields)
            .bind(now)
            .execute(&mut *tx)
            .await
            .expect("failed to seed conference revision");

            sqlx::query("UPDATE item SET current_revision_id = $1 WHERE id = $2")
                .bind(rev_id)
                .bind(item_id)
                .execute(&mut *tx)
                .await
                .expect("failed to link revision to item");
        }

        tx.commit().await.expect("commit conference item seed");
    }

    async fn create_test_user_inner(
        &self,
        username: &str,
        password: &str,
        email: &str,
        is_admin: bool,
    ) {
        use argon2::{
            Argon2,
            password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
        };

        // Use minimal Argon2 params for test speed — production uses RFC 9106
        // params (m=65536, t=3, p=4) but that's too slow for 50+ test users.
        let password = password.to_owned();
        let password_hash = tokio::task::spawn_blocking(move || {
            let salt = SaltString::generate(&mut OsRng);
            let params = argon2::Params::new(
                4 * 1024, // 4 MiB (minimum viable, 16x less than production)
                1,        // 1 iteration
                1,        // 1 lane
                None,
            )
            .expect("test Argon2 params are valid");
            let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            argon2
                .hash_password(password.as_bytes(), &salt)
                .expect("Failed to hash password")
                .to_string()
        })
        .await
        .expect("Argon2 hashing task panicked");

        let id = Uuid::now_v7();

        sqlx::query(
            r#"
            INSERT INTO users (id, name, pass, mail, status, is_admin)
            VALUES ($1, $2, $3, $4, 1, $5)
            ON CONFLICT ((LOWER(name))) DO UPDATE SET pass = $3, is_admin = $5
            "#,
        )
        .bind(id)
        .bind(username)
        .bind(&password_hash)
        .bind(email)
        .bind(is_admin)
        .execute(&self.db)
        .await
        .expect("Failed to create test user");
    }
}

/// Extract Set-Cookie headers from a response for use in subsequent requests.
pub fn extract_cookies(response: &Response) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|cookie| {
            // Extract just the cookie name=value, ignoring attributes
            cookie.split(';').next()
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Derive a deterministic fake IP from a username, or from any other key.
///
/// Each test user gets a unique IP in the 10.x.x.x range so that parallel tests
/// never share a rate-limit bucket. `TestApp::login` uses it for the `login`
/// bucket; a test that makes several requests of its own should use it for those
/// too, or they all land in the shared `127.0.0.1` bucket, which is 100 requests
/// a minute for the whole suite.
pub fn test_ip_for(username: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    username.hash(&mut hasher);
    let h = hasher.finish();
    format!(
        "10.{}.{}.{}",
        (h >> 16) as u8,
        (h >> 8) as u8,
        (h as u8).max(1) // avoid .0 which could be confused with a network address
    )
}

/// Serializes the plugin migration below across tests, binaries and shards.
const TRANSLATION_MIGRATION_LOCK: i64 = 0x_7A11_0000_0001;

/// Apply `trovato_content_translation`'s own migrations, which create
/// `item_translation`.
///
/// Shared by every test that touches `item_translation`.
///
/// The shared app discovers no plugins (its plugin directory is relative to the
/// test's working directory), so the table exists only if some earlier test in
/// the same database booted with the real plugins directory. Depending on that
/// is depending on shard order, so the fixture runs the migration itself. It is
/// idempotent: applied files are recorded in `plugin_migration` and skipped.
///
/// Driven on its own thread against the shared runtime, the way `shared_app`
/// builds the app: the migration runner's future does not meet `run_test`'s
/// `Send` bound.
pub fn ensure_translation_table(app: &TestApp) {
    let dir = project_root().join("plugins/trovato_content_translation");
    let info = trovato_kernel::plugin::PluginInfo::parse(
        &dir.join("trovato_content_translation.info.toml"),
    )
    .expect("parse the translation plugin manifest");
    let db = app.db.clone();
    let handle = shared_runtime_handle();

    std::thread::spawn(move || {
        handle.block_on(async move {
            // Held for the length of the transaction; the migration runs on its
            // own connection meanwhile, and any concurrent caller waits here.
            let mut guard = db.begin().await.expect("begin migration lock");
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(TRANSLATION_MIGRATION_LOCK)
                .execute(&mut *guard)
                .await
                .expect("take migration lock");
            let result = trovato_kernel::plugin::migration::run_plugin_migrations(
                &db,
                "trovato_content_translation",
                &info,
                &dir,
            )
            .await;
            guard.commit().await.expect("release migration lock");
            result.expect("run the translation plugin migrations");
        });
    })
    .join()
    .expect("translation migration thread panicked");
}

// =============================================================================
// Users, roles and stages
//
// These four were private copies in several test files each (`user_holding` and
// `grant_via_role` in three, `create_test_stage` in two). The read-path access
// work needed them in more files again, so they live here once rather than
// becoming a fourth and third copy.
// =============================================================================

/// A unique username, so parallel test binaries never share a user, a
/// rate-limit bucket, or a password.
pub fn username(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

/// The id of an already-created test user.
pub async fn user_id_of(app: &TestApp, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(&app.db)
        .await
        .expect("test user should exist")
}

/// Create a role named `name`, and delete it again when the test ends.
///
/// The one way a test should make a role. Several files grew their own copy of
/// this insert, each with its own name prefix and none of them removing what it
/// made; routing them all through here is what keeps the role table from
/// growing by a hundred rows a run. See [`track_test_role`] for why that
/// matters.
///
/// Upserts on the name, like the hand-written copies it replaces, so a fixture
/// that derives a role name from a user it already created is idempotent.
pub async fn create_test_role(app: &TestApp, name: &str) -> Uuid {
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (id, name) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(name)
    .fetch_one(&app.db)
    .await
    .unwrap_or_else(|e| panic!("create test role '{name}': {e}"));
    track_test_role(&app.db, role_id);
    role_id
}

/// Grant `permissions` to `user_id` through a role, the way a real site does.
pub async fn grant_via_role(app: &TestApp, user_id: Uuid, permissions: &[&str]) {
    use trovato_kernel::models::Role;

    let role = Role::create(&app.db, &format!("testrole-{}", Uuid::now_v7().simple()))
        .await
        .expect("create role");
    track_test_role(&app.db, role.id);
    for permission in permissions {
        Role::add_permission(&app.db, role.id, permission)
            .await
            .expect("add permission to role");
    }
    Role::assign_to_user(&app.db, user_id, role.id)
        .await
        .expect("assign role to user");
    app.state.permissions().invalidate_user(user_id);
}

/// Create a non-superuser holding exactly `permissions`, and log them in.
///
/// Returns the user's id and their session cookies.
pub async fn user_holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String) {
    let name = username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = user_id_of(app, &name).await;
    if !permissions.is_empty() {
        grant_via_role(app, id, permissions).await;
    }
    let cookies = app.login(&name, "test-password-123").await;
    (id, cookies)
}

/// Create a stage with a unique label and machine name.
///
/// `visibility: None` takes the column default, which is **internal** — the
/// property the autocomplete and MCP list tests rely on.
pub async fn create_test_stage(app: &TestApp, prefix: &str) -> Uuid {
    use trovato_kernel::models::stage::{CreateStage, Stage};

    let suffix = &Uuid::now_v7().simple().to_string()[..8];
    let stage = Stage::create(
        &app.db,
        CreateStage {
            label: format!("{prefix} {suffix}"),
            machine_name: format!("{prefix}_{suffix}"),
            description: None,
            visibility: None,
            is_default: None,
            weight: None,
        },
    )
    .await
    .expect("failed to create test stage");
    stage.id
}

// =============================================================================
// An ItemService with the field-access reference plugin loaded
// =============================================================================

/// Build an [`ItemService`] whose dispatcher has `trovato_field_access_ref`
/// loaded, over `pool`.
///
/// Its default rules deny `ssn` on type `person` to a viewer without
/// `view pii`, which is how a test asks for a real field-access **denial**: the
/// shared `TestApp` loads no field-access plugin, so every governed field there
/// fails open.
///
/// `pool` is the caller's: `field_access_plugin_test` passes a lazy,
/// never-connected pool (the plugin's `variables_get` then falls back to its
/// baked-in rules, so that file needs no infrastructure), while the AI chat and
/// Pagefind tests pass the live test pool because the kernel code under test
/// reads items out of it.
///
/// Panics with a build hint if the fixture `.wasm` is missing — CI builds it
/// before the test job; locally `cargo build -p trovato_field_access_ref
/// --target wasm32-wasip1 --release && cp …` is the same step.
pub fn item_service_with_ref_plugin(pool: PgPool) -> trovato_kernel::content::ItemService {
    use std::sync::Arc;
    use std::time::Duration;
    use trovato_kernel::content::ItemService;
    use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
    use trovato_kernel::tap::{RequestServices, TapDispatcher, TapRegistry};

    let name = "trovato_field_access_ref";
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&project_root().join("plugins").join(name))
        .unwrap_or_else(|e| {
            panic!(
                "failed to load fixture '{name}': {e:#}\n\
                 build it first: cargo build -p {name} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{name}.wasm plugins/{name}/"
            )
        });
    let runtime = Arc::new(runtime);
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    let dispatcher = Arc::new(TapDispatcher::new(Arc::clone(&runtime), registry));

    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(Arc::clone(&runtime));

    ItemService::new(
        pool,
        dispatcher,
        services,
        Duration::from_secs(60),
        None,
        None,
    )
}
