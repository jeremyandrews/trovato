#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Shared test infrastructure for MCP server integration tests.
//!
//! Uses the same shared-runtime pattern as kernel integration tests
//! to avoid PgPool connection staling across test runtimes.

use std::sync::OnceLock;

use uuid::Uuid;

use trovato_kernel::Config;
use trovato_kernel::models::User;
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use trovato_mcp::server::TrovatoMcpServer;

/// Shared Tokio runtime that outlives all test runtimes.
///
/// Environment variables are set here before the runtime is built,
/// ensuring no data race with concurrent test threads.
pub static SHARED_RT: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| {
        // Set env vars before any threads are spawned by the runtime.
        // This is the earliest safe point — no test threads are running yet.
        init_test_env();

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to build shared test runtime")
    });

/// Global shared test context — initialized once.
static SHARED_CTX: OnceLock<TestContext> = OnceLock::new();

/// Run an async test body on the shared runtime.
pub fn run_test<F: std::future::Future<Output = ()> + Send>(f: F) {
    SHARED_RT.block_on(f);
}

/// Get the shared test context.
pub async fn shared_app() -> &'static TestContext {
    SHARED_CTX.get_or_init(|| {
        let handle = SHARED_RT.handle().clone();
        std::thread::spawn(move || handle.block_on(TestContext::new()))
            .join()
            .expect("TestContext init thread panicked")
    })
}

/// Set environment variables needed by tests.
///
/// Called once during `SHARED_RT` initialization, before any async work
/// or test threads are spawned.
fn init_test_env() {
    dotenvy::dotenv().ok();

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let project_root = std::path::Path::new(&manifest_dir)
        .parent() // crates/
        .and_then(|p| p.parent()) // project root
        .unwrap_or(std::path::Path::new("."));

    // SAFETY: No other threads exist yet — SHARED_RT LazyLock is the first
    // thing initialized and the runtime hasn't been built at this point.
    if std::env::var("TEMPLATES_DIR").is_err() {
        unsafe { std::env::set_var("TEMPLATES_DIR", project_root.join("templates")) };
    }
    if std::env::var("STATIC_DIR").is_err() {
        unsafe { std::env::set_var("STATIC_DIR", project_root.join("static")) };
    }
    if std::env::var("DATABASE_MAX_CONNECTIONS").is_err() {
        unsafe { std::env::set_var("DATABASE_MAX_CONNECTIONS", "10") };
    }
}

/// Shared test context with AppState and pre-created test users.
pub struct TestContext {
    pub state: AppState,
    /// Admin user for privileged operations.
    pub admin_user: User,
    /// Admin user context with pre-loaded permissions.
    pub admin_user_ctx: UserContext,
    /// Unprivileged user context (no roles/permissions).
    pub unprivileged_user_ctx: UserContext,
}

impl TestContext {
    async fn new() -> Self {
        let config = Config::from_env().expect("Failed to load config");
        let state = AppState::new(&config)
            .await
            .expect("Failed to initialize AppState");

        // Create admin user
        let admin_user = create_test_user(state.db(), "mcp_test_admin", true).await;
        let admin_user_ctx = trovato_mcp::auth::build_user_context(&state, &admin_user)
            .await
            .expect("build admin user context");

        // Create unprivileged user (no roles/permissions)
        let unprivileged_user = create_test_user(state.db(), "mcp_test_nopriv", false).await;
        let unprivileged_user_ctx =
            trovato_mcp::auth::build_user_context(&state, &unprivileged_user)
                .await
                .expect("build unprivileged user context");

        Self {
            state,
            admin_user,
            admin_user_ctx,
            unprivileged_user_ctx,
        }
    }

    /// Build an MCP server instance for the admin user.
    pub fn mcp_server(&self) -> TrovatoMcpServer {
        TrovatoMcpServer::new(
            self.state.clone(),
            "test_token_for_mcp".to_string(),
            self.admin_user_ctx.clone(),
        )
    }
}

/// Create a test user in the database, returning the `User` model.
async fn create_test_user(pool: &sqlx::PgPool, username: &str, is_admin: bool) -> User {
    use argon2::{
        Argon2,
        password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
    };

    let password = "testpassword12";
    let password_owned = password.to_owned();

    let password_hash = tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        let params = argon2::Params::new(4 * 1024, 1, 1, None).expect("test Argon2 params");
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon2
            .hash_password(password_owned.as_bytes(), &salt)
            .expect("hash password")
            .to_string()
    })
    .await
    .expect("hash task");

    let id = Uuid::now_v7();
    let email = format!("{username}@test.local");

    sqlx::query(
        r#"
        INSERT INTO users (id, name, pass, mail, status, is_admin)
        VALUES ($1, $2, $3, $4, 1, $5)
        ON CONFLICT ((LOWER(name))) DO UPDATE SET pass = $3, is_admin = $5
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(username)
    .bind(&password_hash)
    .bind(&email)
    .bind(is_admin)
    .execute(pool)
    .await
    .expect("create test user");

    // Load the user back to get the definitive record
    User::find_by_name(pool, username)
        .await
        .expect("find user")
        .expect("user should exist")
}
