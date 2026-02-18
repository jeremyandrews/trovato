//! Authentication routes (login, logout).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use tracing::info;

use crate::form::csrf::{generate_csrf_token, verify_csrf_token};
use crate::middleware::language::SESSION_ACTIVE_LANGUAGE;
use crate::models::User;
use crate::state::AppState;

/// Session key for storing the authenticated user ID.
pub const SESSION_USER_ID: &str = "user_id";

/// Session key for storing the active stage.
pub const SESSION_ACTIVE_STAGE: &str = "active_stage";

/// Session key for remember_me flag.
pub const SESSION_REMEMBER_ME: &str = "remember_me";

/// Login request body.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: bool,
}

/// Login response.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub message: String,
}

/// Error response for authentication failures.
#[derive(Debug, Serialize)]
pub struct AuthError {
    pub error: String,
}

/// Typed login error for explicit status code mapping.
///
/// Avoids brittle substring matching on error strings by encoding
/// the error category in the enum variant.
#[derive(Debug)]
enum LoginError {
    /// Account temporarily locked due to too many failed attempts (429).
    Locked(String),
    /// Invalid credentials — wrong username or password (401).
    InvalidCredentials,
    /// Internal server error — database failure, etc. (500).
    Internal(String),
}

impl LoginError {
    fn status_code(&self) -> StatusCode {
        match self {
            LoginError::Locked(_) => StatusCode::TOO_MANY_REQUESTS,
            LoginError::InvalidCredentials => StatusCode::UNAUTHORIZED,
            LoginError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            LoginError::Locked(msg) => msg,
            LoginError::InvalidCredentials => "Invalid username or password",
            LoginError::Internal(msg) => msg,
        }
    }
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Login form handler.
///
/// GET /user/login
/// - Renders login form with CSRF token
async fn login_form(State(state): State<AppState>, session: Session) -> Response {
    // Generate CSRF token
    let csrf_token = match generate_csrf_token(&session).await {
        Ok(token) => token,
        Err(e) => {
            tracing::error!(error = %e, "failed to generate CSRF token");
            return Html("<h1>Error</h1><p>Failed to generate form token</p>".to_string())
                .into_response();
        }
    };

    // Render login form
    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);

    match state.theme().tera().render("user/login.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render login form");
            Html(format!(
                r#"<!DOCTYPE html>
<html><head><title>Log in</title></head>
<body style="font-family: sans-serif; max-width: 400px; margin: 100px auto; padding: 2rem;">
<h1>Log in</h1>
<form method="post" action="/user/login">
<input type="hidden" name="_token" value="{}">
<p><label>Username<br><input type="text" name="username" required></label></p>
<p><label>Password<br><input type="password" name="password" required></label></p>
<p><button type="submit">Log in</button></p>
</form>
</body></html>"#,
                csrf_token
            ))
            .into_response()
        }
    }
}

/// Form-based login request.
#[derive(Debug, Deserialize)]
pub struct LoginFormRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: Option<String>,
    #[serde(rename = "_token")]
    pub csrf_token: Option<String>,
}

/// Form-based login handler.
///
/// POST /user/login (form data)
async fn login_form_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<LoginFormRequest>,
) -> Response {
    // Verify CSRF token
    if let Some(token) = &form.csrf_token {
        match verify_csrf_token(&session, token).await {
            Ok(true) => {}
            _ => {
                return render_login_error(
                    &state,
                    &session,
                    "Invalid form token. Please try again.",
                )
                .await;
            }
        }
    }

    // Convert to internal request format
    let request = LoginRequest {
        username: form.username,
        password: form.password,
        remember_me: form.remember_me.is_some(),
    };

    // Perform login
    match do_login(&state, &session, &request).await {
        Ok(_) => Redirect::to("/admin").into_response(),
        Err(e) => render_login_error(&state, &session, e.message()).await,
    }
}

/// Render login form with error message.
async fn render_login_error(state: &AppState, session: &Session, error: &str) -> Response {
    let csrf_token = generate_csrf_token(session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("error", error);

    match state.theme().tera().render("user/login.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(_) => Html(format!(
            "<h1>Login Error</h1><p>{}</p><p><a href=\"/user/login\">Try again</a></p>",
            error
        ))
        .into_response(),
    }
}

/// Initialize session state after successful authentication.
async fn setup_session(
    session: &Session,
    user_id: uuid::Uuid,
    remember_me: bool,
) -> Result<(), LoginError> {
    session
        .insert(SESSION_USER_ID, user_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert user_id into session");
            LoginError::Internal("Internal server error".to_string())
        })?;

    session
        .insert(SESSION_ACTIVE_STAGE, Option::<String>::None)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert active_stage into session");
            LoginError::Internal("Internal server error".to_string())
        })?;

    // Only initialize active_language if not already set in the session.
    // This preserves the user's language preference across login — they may
    // have been browsing in a specific language before authenticating.
    let has_language = session
        .get::<Option<String>>(SESSION_ACTIVE_LANGUAGE)
        .await
        .ok()
        .and_then(|v| v) // None if key doesn't exist, Some(_) if it does
        .is_some();

    if !has_language {
        session
            .insert(SESSION_ACTIVE_LANGUAGE, Option::<String>::None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to insert active_language into session");
                LoginError::Internal("Internal server error".to_string())
            })?;
    }

    session
        .insert(SESSION_REMEMBER_ME, remember_me)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert remember_me into session");
            LoginError::Internal("Internal server error".to_string())
        })?;

    Ok(())
}

/// Perform login and return typed error on failure.
async fn do_login(
    state: &AppState,
    session: &Session,
    request: &LoginRequest,
) -> Result<(), LoginError> {
    // Check if account is locked
    match state.lockout().is_locked(&request.username).await {
        Ok(true) => {
            let remaining = state
                .lockout()
                .get_lockout_remaining(&request.username)
                .await
                .unwrap_or(None);

            let message = if let Some(secs) = remaining {
                format!(
                    "Account temporarily locked. Try again in {} minutes.",
                    (secs / 60) + 1
                )
            } else {
                "Account temporarily locked. Try again later.".to_string()
            };
            return Err(LoginError::Locked(message));
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!(error = %e, "failed to check lockout status");
        }
    }

    // Find user by username
    let user = match User::find_by_name(state.db(), &request.username).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            let _ = state
                .lockout()
                .record_failed_attempt(&request.username)
                .await;
            return Err(LoginError::InvalidCredentials);
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during login");
            return Err(LoginError::Internal("Internal server error".to_string()));
        }
    };

    // Check if user is active
    if !user.is_active() {
        let _ = state
            .lockout()
            .record_failed_attempt(&request.username)
            .await;
        return Err(LoginError::InvalidCredentials);
    }

    // Verify password
    if !user.verify_password(&request.password) {
        match state
            .lockout()
            .record_failed_attempt(&request.username)
            .await
        {
            Ok((locked, _)) => {
                if locked {
                    return Err(LoginError::Locked(
                        "Account temporarily locked due to too many failed attempts.".to_string(),
                    ));
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to record failed attempt");
            }
        }
        return Err(LoginError::InvalidCredentials);
    }

    // Successful login - clear any failed attempts
    let _ = state.lockout().clear_attempts(&request.username).await;

    // Update login timestamp
    if let Err(e) = User::touch_login(state.db(), user.id).await {
        tracing::warn!(error = %e, user_id = %user.id, "failed to update login timestamp");
    }

    // Create session
    setup_session(session, user.id, request.remember_me).await?;

    // Dispatch tap_user_login
    let tap_input = serde_json::json!({ "user_id": user.id.to_string() });
    let tap_state = crate::tap::RequestState::without_services(
        crate::tap::UserContext::authenticated(user.id, vec![]),
    );
    state
        .tap_dispatcher()
        .dispatch("tap_user_login", &tap_input.to_string(), tap_state)
        .await;

    info!(user_id = %user.id, "user logged in");
    Ok(())
}

/// JSON login handler.
///
/// POST /user/login/json
/// - Delegates to `do_login` for all auth logic
/// - Maps typed `LoginError` variants to appropriate HTTP status codes
async fn login(
    State(state): State<AppState>,
    session: Session,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
    match do_login(&state, &session, &request).await {
        Ok(()) => Ok(Json(LoginResponse {
            success: true,
            message: "Login successful".to_string(),
        })),
        Err(e) => {
            let status = e.status_code();
            Err((
                status,
                Json(AuthError {
                    error: e.message().to_string(),
                }),
            ))
        }
    }
}

/// Logout handler.
///
/// GET /user/logout
/// - Dispatches tap_user_logout before destroying session
/// - Deletes session from Redis
/// - Clears session cookie
async fn logout(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
    // Extract user_id before deleting the session
    let user_id: Option<uuid::Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    // Dispatch tap_user_logout
    if let Some(uid) = user_id {
        let tap_input = serde_json::json!({ "user_id": uid.to_string() });
        let tap_state = crate::tap::RequestState::without_services(
            crate::tap::UserContext::authenticated(uid, vec![]),
        );
        state
            .tap_dispatcher()
            .dispatch("tap_user_logout", &tap_input.to_string(), tap_state)
            .await;
    }

    session.delete().await.map_err(|e| {
        tracing::error!(error = %e, "failed to delete session");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AuthError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    Ok(Json(LoginResponse {
        success: true,
        message: "Logout successful".to_string(),
    }))
}

/// Create the auth router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/login", get(login_form).post(login_form_submit))
        .route("/user/login/json", post(login))
        .route("/user/logout", get(logout))
}
