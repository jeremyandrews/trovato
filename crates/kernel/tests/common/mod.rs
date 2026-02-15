//! Common test utilities for integration tests.
//!
//! This module provides test infrastructure that uses the REAL kernel code,
//! not mock implementations. This ensures tests verify actual behavior.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use axum::response::Response;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use trovato_kernel::{AppState, Config, ConfigStorage};

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

        // Create config from environment
        let config = Config::from_env().expect("Failed to load config");

        // Initialize the REAL AppState (database, redis, plugins, templates, etc.)
        let state = AppState::new(&config)
            .await
            .expect("Failed to initialize AppState");

        let db = state.db().clone();

        // Create session layer
        let session_layer = trovato_kernel::session::create_session_layer(&config.redis_url)
            .await
            .expect("Failed to create session layer");

        // Build the REAL router with all kernel routes
        let router = Router::new()
            .merge(trovato_kernel::routes::install::router())
            .merge(trovato_kernel::routes::auth::router())
            .merge(trovato_kernel::routes::admin::router())
            .merge(trovato_kernel::routes::password_reset::router())
            .merge(trovato_kernel::routes::health::router())
            .merge(trovato_kernel::routes::item::router())
            .merge(trovato_kernel::routes::category::router())
            .merge(trovato_kernel::routes::comment::router())
            .merge(trovato_kernel::routes::gather::router())
            .merge(trovato_kernel::routes::search::router())
            .merge(trovato_kernel::routes::cron::router())
            .merge(trovato_kernel::routes::file::router())
            .merge(trovato_kernel::routes::metrics::router())
            .merge(trovato_kernel::routes::batch::router())
            .merge(trovato_kernel::routes::static_files::router())
            // Path alias middleware runs first (last added = first executed)
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                trovato_kernel::middleware::resolve_path_alias,
            ))
            .layer(session_layer)
            .layer(tower_http::trace::TraceLayer::new_for_http())
            .with_state(state.clone());

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
    pub async fn login(&self, username: &str, password: &str) -> String {
        let response = self
            .request(
                Request::post("/user/login/json")
                    .header("content-type", "application/json")
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

    /// Create a test user directly in the database.
    pub async fn create_test_user(&self, username: &str, password: &str, email: &str) {
        use argon2::{
            Argon2,
            password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
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
