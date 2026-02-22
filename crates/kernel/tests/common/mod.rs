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

/// Run an async test body on [`SHARED_RT`].
///
/// Using a single runtime for all tests prevents the "Tokio context is being
/// shutdown" error that occurs when PgPool connections opened on one
/// `#[tokio::test]` runtime are reused by another after the first shuts down.
pub fn run_test<F: std::future::Future<Output = ()> + Send>(f: F) {
    SHARED_RT.block_on(f);
}

/// Test application wrapper using the REAL kernel routes and state.
pub struct TestApp {
    router: Router,
    pub db: PgPool,
    pub state: AppState,
}

impl TestApp {
    /// Create a new test application with full kernel initialization.
    pub async fn new() -> Self {
        // Load test environment
        dotenvy::dotenv().ok();

        // Set templates directory to project root templates/
        // Tests run from crates/kernel/, so we need to go up two levels
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let project_root = std::path::Path::new(&manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()) // project root
            .unwrap_or(std::path::Path::new("."));

        if std::env::var("TEMPLATES_DIR").is_err() {
            let templates_dir = project_root.join("templates");
            // SAFETY: We're setting the environment variable before spawning threads
            unsafe { std::env::set_var("TEMPLATES_DIR", templates_dir) };
        }

        if std::env::var("STATIC_DIR").is_err() {
            let static_dir = project_root.join("static");
            unsafe { std::env::set_var("STATIC_DIR", static_dir) };
        }

        // Tests run 100 tests concurrently — bump the default pool size so
        // serialization locks don't starve other tests of connections.
        if std::env::var("DATABASE_MAX_CONNECTIONS").is_err() {
            unsafe { std::env::set_var("DATABASE_MAX_CONNECTIONS", "25") };
        }

        // Create config from environment
        let config = Config::from_env().expect("Failed to load config");

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

        // Build the REAL router with all kernel routes (must match main.rs)
        let router = Router::new()
            .merge(trovato_kernel::routes::front::router())
            .merge(trovato_kernel::routes::install::router())
            .merge(trovato_kernel::routes::auth::router())
            .merge(trovato_kernel::routes::admin::router())
            .merge(trovato_kernel::routes::password_reset::router())
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
            .merge(trovato_kernel::routes::tile_admin::router())
            .merge(trovato_kernel::routes::static_files::router())
            // Plugin-gated routes — runtime middleware returns 404 when disabled
            .merge(trovato_kernel::routes::gated_plugin_routes(&state))
            // Path alias middleware runs first (last added = first executed)
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::resolve_path_alias,
            ))
            .layer(session_layer)
            .layer(tower_http::trace::TraceLayer::new_for_http())
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
    pub async fn cleanup_content_type(&self, machine_name: &str) {
        sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(machine_name)
            .execute(&self.db)
            .await
            .ok();
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

/// Derive a deterministic fake IP from a username.
///
/// Each test user gets a unique IP in the 10.x.x.x range so that parallel
/// tests never share a rate-limit bucket.
fn test_ip_for(username: &str) -> String {
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
