//! Common test utilities for integration tests.
//!
//! This module provides a self-contained test harness that mirrors the kernel's
//! route handlers without depending on the kernel's internal modules. This allows
//! tests to run against a simplified but functionally equivalent implementation.
//!
//! ## Architecture
//!
//! - `TestApp`: Main test harness with database and router
//! - `TestAppState`: Simplified version of kernel's `AppState`
//! - `TestLockoutService`: Redis-based account lockout (mirrors `LockoutService`)
//! - Route builders: `health_router`, `auth_router`, `admin_router`, `password_reset_router`
//!
//! ## Extending
//!
//! When adding new kernel functionality:
//!
//! 1. Add a new route builder function (e.g., `fn content_router(...)`)
//! 2. Define request/response structs locally within the function
//! 3. Implement handlers that use `TestAppState` (db, redis, lockout)
//! 4. Merge the router in `build_test_router`
//! 5. Add cleanup logic in `TestApp::cleanup_test_data` if needed
//!
//! ## Why Not Use Kernel Modules Directly?
//!
//! The kernel's route handlers depend on `AppState` which requires full
//! initialization (WASM runtime, template engine, etc.). This test harness
//! provides isolated, fast tests that verify HTTP behavior without that overhead.

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// Test application wrapper that provides a configured router and database.
pub struct TestApp {
    router: Router,
    db: PgPool,
}

impl TestApp {
    /// Create a new test application with fresh database state.
    pub async fn new() -> Self {
        // Load test environment
        dotenvy::dotenv().ok();

        // Get database URL from environment or use default
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());

        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

        // Create database pool
        let db = PgPool::connect(&database_url)
            .await
            .expect("Failed to connect to database. Is docker-compose running?");

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&db)
            .await
            .expect("Failed to run migrations");

        // Create Redis client
        let redis = redis::Client::open(redis_url.as_str())
            .expect("Failed to create Redis client. Is docker-compose running?");

        // Clean up test data from previous runs
        Self::cleanup_test_data(&db, &redis).await;

        // Build the router (simplified version without full AppState)
        let router = build_test_router(&db, &redis, &redis_url).await;

        Self { router, db }
    }

    /// Send a request to the test application.
    pub async fn request(&self, request: Request<Body>) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("Failed to send request")
    }

    /// Create a test user directly in the database.
    pub async fn create_test_user(&self, username: &str, password: &str, email: &str) {
        use argon2::{
            password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
            Argon2,
        };

        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .expect("Failed to hash password")
            .to_string();

        let id = Uuid::now_v7();

        sqlx::query(
            r#"
            INSERT INTO users (id, name, pass, mail, status, is_admin)
            VALUES ($1, $2, $3, $4, 1, false)
            ON CONFLICT (name) DO UPDATE SET pass = $3
            "#,
        )
        .bind(id)
        .bind(username)
        .bind(&password_hash)
        .bind(email)
        .execute(&self.db)
        .await
        .expect("Failed to create test user");
    }

    /// Clean up test data from previous runs.
    async fn cleanup_test_data(db: &PgPool, redis: &redis::Client) {
        use redis::AsyncCommands;

        // Clear lockout keys from Redis
        if let Ok(mut conn) = redis.get_multiplexed_async_connection().await {
            // Clear all lockout keys for test users
            let _: Result<(), _> = conn.del("lockout:locked:locktest").await;
            let _: Result<(), _> = conn.del("lockout:attempts:locktest").await;
            let _: Result<(), _> = conn.del("lockout:locked:testuser").await;
            let _: Result<(), _> = conn.del("lockout:attempts:testuser").await;
            let _: Result<(), _> = conn.del("lockout:locked:nonexistent").await;
            let _: Result<(), _> = conn.del("lockout:attempts:nonexistent").await;
        }

        // Clear test users (keep any real data by only deleting specific test patterns)
        sqlx::query("DELETE FROM users WHERE name LIKE 'test%' OR name LIKE 'lock%'")
            .execute(db)
            .await
            .ok();

        // Clear password reset tokens
        sqlx::query("DELETE FROM password_reset_tokens")
            .execute(db)
            .await
            .ok();
    }
}

/// Build a test router with all routes.
async fn build_test_router(db: &PgPool, redis: &redis::Client, redis_url: &str) -> Router {
    use axum::Router;
    use fred::prelude::*;
    use tower_sessions::SessionManagerLayer;
    use tower_sessions_redis_store::RedisStore;

    // Create a minimal AppState-like struct for testing
    // We'll use a simplified approach that mirrors the real AppState

    // Create fred pool for sessions
    let fred_config = fred::prelude::Config::from_url(redis_url)
        .expect("Failed to parse Redis URL for fred");
    let fred_pool = Builder::from_config(fred_config)
        .build_pool(5)
        .expect("Failed to create fred pool");
    fred_pool.init().await.expect("Failed to init fred pool");

    let session_store = RedisStore::new(fred_pool);
    let session_layer = SessionManagerLayer::new(session_store);

    // Create the app state
    let state = TestAppState {
        db: db.clone(),
        redis: redis.clone(),
        lockout: TestLockoutService::new(redis.clone()),
    };

    // Build router with all routes
    Router::new()
        .merge(health_router(state.clone()))
        .merge(auth_router(state.clone()))
        .merge(admin_router(state.clone()))
        .merge(password_reset_router(state.clone()))
        .layer(session_layer)
        .with_state(state)
}

// =============================================================================
// Minimal AppState for Testing
// =============================================================================

#[derive(Clone)]
struct TestAppState {
    db: PgPool,
    redis: redis::Client,
    lockout: TestLockoutService,
}

#[derive(Clone)]
struct TestLockoutService {
    redis: redis::Client,
}

impl TestLockoutService {
    fn new(redis: redis::Client) -> Self {
        Self { redis }
    }

    async fn is_locked(&self, username: &str) -> anyhow::Result<bool> {
        use redis::AsyncCommands;
        let key = format!("lockout:locked:{}", username);
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let locked: bool = conn.exists(&key).await?;
        Ok(locked)
    }

    async fn record_failed_attempt(&self, username: &str) -> anyhow::Result<(bool, u32)> {
        use redis::AsyncCommands;
        let attempts_key = format!("lockout:attempts:{}", username);
        let lockout_key = format!("lockout:locked:{}", username);

        let mut conn = self.redis.get_multiplexed_async_connection().await?;

        let attempts: u32 = conn.incr(&attempts_key, 1).await?;
        if attempts == 1 {
            conn.expire::<_, ()>(&attempts_key, 15 * 60).await?;
        }

        if attempts >= 5 {
            conn.set_ex::<_, _, ()>(&lockout_key, "locked", 15 * 60)
                .await?;
            conn.del::<_, ()>(&attempts_key).await?;
            return Ok((true, 0));
        }

        Ok((false, 5 - attempts))
    }

    async fn clear_attempts(&self, username: &str) -> anyhow::Result<()> {
        use redis::AsyncCommands;
        let key = format!("lockout:attempts:{}", username);
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        conn.del::<_, ()>(&key).await?;
        Ok(())
    }

    async fn get_lockout_remaining(&self, username: &str) -> anyhow::Result<Option<u64>> {
        use redis::AsyncCommands;
        let key = format!("lockout:locked:{}", username);
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let ttl: i64 = conn.ttl(&key).await?;
        if ttl > 0 {
            Ok(Some(ttl as u64))
        } else {
            Ok(None)
        }
    }
}

// =============================================================================
// Route Builders (simplified versions that work with TestAppState)
// =============================================================================

fn health_router(state: TestAppState) -> Router<TestAppState> {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Json;
    use serde::Serialize;

    #[derive(Serialize)]
    struct HealthResponse {
        status: &'static str,
        postgres: bool,
        redis: bool,
    }

    async fn health_check(State(state): State<TestAppState>) -> (StatusCode, Json<HealthResponse>) {
        let postgres = sqlx::query("SELECT 1")
            .fetch_one(&state.db)
            .await
            .is_ok();

        let redis = state
            .redis
            .get_multiplexed_async_connection()
            .await
            .is_ok();

        let status = if postgres && redis {
            "healthy"
        } else {
            "unhealthy"
        };
        let status_code = if postgres && redis {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };

        (status_code, Json(HealthResponse { status, postgres, redis }))
    }

    Router::new().route("/health", get(health_check))
}

fn auth_router(state: TestAppState) -> Router<TestAppState> {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::Json;
    use serde::{Deserialize, Serialize};
    use tower_sessions::Session;

    #[derive(Deserialize)]
    struct LoginRequest {
        username: String,
        password: String,
        #[serde(default)]
        remember_me: bool,
    }

    #[derive(Serialize)]
    struct LoginResponse {
        success: bool,
        message: String,
    }

    #[derive(Serialize)]
    struct AuthError {
        error: String,
    }

    async fn login(
        State(state): State<TestAppState>,
        session: Session,
        Json(request): Json<LoginRequest>,
    ) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
        use argon2::{password_hash::PasswordVerifier, Argon2, PasswordHash};

        let auth_error = || {
            (
                StatusCode::UNAUTHORIZED,
                Json(AuthError {
                    error: "Invalid username or password".to_string(),
                }),
            )
        };

        // Check lockout
        match state.lockout.is_locked(&request.username).await {
            Ok(true) => {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(AuthError {
                        error: "Account temporarily locked".to_string(),
                    }),
                ));
            }
            _ => {}
        }

        // Find user
        let user: Option<(Uuid, String, i16)> = sqlx::query_as(
            "SELECT id, pass, status FROM users WHERE name = $1",
        )
        .bind(&request.username)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

        let (user_id, password_hash, status) = match user {
            Some(u) => u,
            None => {
                let _ = state.lockout.record_failed_attempt(&request.username).await;
                return Err(auth_error());
            }
        };

        if status != 1 {
            let _ = state.lockout.record_failed_attempt(&request.username).await;
            return Err(auth_error());
        }

        // Verify password
        let parsed_hash = PasswordHash::new(&password_hash).map_err(|_| auth_error())?;
        if Argon2::default()
            .verify_password(request.password.as_bytes(), &parsed_hash)
            .is_err()
        {
            match state.lockout.record_failed_attempt(&request.username).await {
                Ok((locked, _)) if locked => {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(AuthError {
                            error: "Account temporarily locked due to too many failed attempts"
                                .to_string(),
                        }),
                    ));
                }
                _ => {}
            }
            return Err(auth_error());
        }

        // Success
        let _ = state.lockout.clear_attempts(&request.username).await;
        session.insert("user_id", user_id).await.ok();

        Ok(Json(LoginResponse {
            success: true,
            message: "Login successful".to_string(),
        }))
    }

    async fn logout(session: Session) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
        session.delete().await.ok();
        Ok(Json(LoginResponse {
            success: true,
            message: "Logout successful".to_string(),
        }))
    }

    Router::new()
        .route("/user/login", post(login))
        .route("/user/logout", get(logout))
}

fn admin_router(_state: TestAppState) -> Router<TestAppState> {
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::Json;
    use serde::{Deserialize, Serialize};
    use tower_sessions::Session;

    const SESSION_ACTIVE_STAGE: &str = "active_stage";

    #[derive(Deserialize)]
    struct StageSwitchRequest {
        stage_id: Option<String>,
    }

    #[derive(Serialize)]
    struct StageSwitchResponse {
        success: bool,
        active_stage: Option<String>,
    }

    #[derive(Serialize)]
    struct AdminError {
        error: String,
    }

    async fn switch_stage(
        session: Session,
        Json(request): Json<StageSwitchRequest>,
    ) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
        session
            .insert(SESSION_ACTIVE_STAGE, request.stage_id.clone())
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(AdminError {
                        error: "Failed to switch stage".to_string(),
                    }),
                )
            })?;

        Ok(Json(StageSwitchResponse {
            success: true,
            active_stage: request.stage_id,
        }))
    }

    async fn get_current_stage(
        session: Session,
    ) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
        let active_stage: Option<String> = session
            .get(SESSION_ACTIVE_STAGE)
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(AdminError {
                        error: "Failed to get stage".to_string(),
                    }),
                )
            })?
            .flatten();

        Ok(Json(StageSwitchResponse {
            success: true,
            active_stage,
        }))
    }

    Router::new()
        .route("/admin/stage/switch", post(switch_stage))
        .route("/admin/stage/current", get(get_current_stage))
}

fn password_reset_router(state: TestAppState) -> Router<TestAppState> {
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::Json;
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    #[derive(Deserialize)]
    struct RequestResetInput {
        email: String,
    }

    #[derive(Serialize)]
    struct ResetResponse {
        success: bool,
        message: String,
    }

    #[derive(Serialize)]
    struct ResetError {
        error: String,
    }

    async fn request_reset(
        State(_state): State<TestAppState>,
        Json(_input): Json<RequestResetInput>,
    ) -> Json<ResetResponse> {
        // Always return success for security
        Json(ResetResponse {
            success: true,
            message: "If an account with that email exists, a reset link has been sent."
                .to_string(),
        })
    }

    async fn validate_token(
        State(state): State<TestAppState>,
        Path(token): Path<String>,
    ) -> Result<Json<ResetResponse>, (StatusCode, Json<ResetError>)> {
        let token_hash = {
            let mut hasher = Sha256::new();
            hasher.update(token.as_bytes());
            hex::encode(hasher.finalize())
        };

        let exists: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM password_reset_tokens WHERE token_hash = $1 AND expires_at > NOW() AND used_at IS NULL",
        )
        .bind(&token_hash)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ResetError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

        if exists.is_some() {
            Ok(Json(ResetResponse {
                success: true,
                message: "Token is valid. You may set a new password.".to_string(),
            }))
        } else {
            Err((
                StatusCode::BAD_REQUEST,
                Json(ResetError {
                    error: "Invalid or expired reset token".to_string(),
                }),
            ))
        }
    }

    Router::new()
        .route("/user/password-reset", post(request_reset))
        .route("/user/password-reset/{token}", get(validate_token))
}
