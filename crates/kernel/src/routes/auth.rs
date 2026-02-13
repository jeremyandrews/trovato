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
        Err(error_message) => render_login_error(&state, &session, &error_message).await,
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

/// Perform login and return error message on failure.
async fn do_login(
    state: &AppState,
    session: &Session,
    request: &LoginRequest,
) -> Result<(), String> {
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
            return Err(message);
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
            return Err("Invalid username or password".to_string());
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during login");
            return Err("Internal server error".to_string());
        }
    };

    // Check if user is active
    if !user.is_active() {
        let _ = state
            .lockout()
            .record_failed_attempt(&request.username)
            .await;
        return Err("Invalid username or password".to_string());
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
                    return Err(
                        "Account temporarily locked due to too many failed attempts.".to_string(),
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to record failed attempt");
            }
        }
        return Err("Invalid username or password".to_string());
    }

    // Successful login - clear any failed attempts
    let _ = state.lockout().clear_attempts(&request.username).await;

    // Update login timestamp
    if let Err(e) = User::touch_login(state.db(), user.id).await {
        tracing::warn!(error = %e, user_id = %user.id, "failed to update login timestamp");
    }

    // Create session
    session
        .insert(SESSION_USER_ID, user.id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert user_id into session");
            "Internal server error".to_string()
        })?;

    session
        .insert(SESSION_ACTIVE_STAGE, Option::<String>::None)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert active_stage into session");
            "Internal server error".to_string()
        })?;

    session
        .insert(SESSION_REMEMBER_ME, request.remember_me)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert remember_me into session");
            "Internal server error".to_string()
        })?;

    info!(user_id = %user.id, "user logged in");
    Ok(())
}

/// JSON login handler.
///
/// POST /user/login
/// - Checks for account lockout
/// - Verifies username and password
/// - Creates session on success
/// - Updates login/access timestamps
/// - Returns 401 on failure without revealing which field was wrong
async fn login(
    State(state): State<AppState>,
    session: Session,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
    // Generic error message that doesn't reveal which field was wrong
    let auth_error = || {
        (
            StatusCode::UNAUTHORIZED,
            Json(AuthError {
                error: "Invalid username or password".to_string(),
            }),
        )
    };

    // Check if account is locked
    match state.lockout().is_locked(&request.username).await {
        Ok(true) => {
            // Get remaining lockout time for user-friendly message
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

            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(AuthError { error: message }),
            ));
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!(error = %e, "failed to check lockout status");
            // Continue with login attempt even if lockout check fails
        }
    }

    // Find user by username
    let user = match User::find_by_name(state.db(), &request.username).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            // Record failed attempt even for non-existent users (prevent enumeration)
            let _ = state
                .lockout()
                .record_failed_attempt(&request.username)
                .await;
            return Err(auth_error());
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during login");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Check if user is active
    if !user.is_active() {
        let _ = state
            .lockout()
            .record_failed_attempt(&request.username)
            .await;
        return Err(auth_error());
    }

    // Verify password
    if !user.verify_password(&request.password) {
        // Record failed attempt
        match state
            .lockout()
            .record_failed_attempt(&request.username)
            .await
        {
            Ok((locked, remaining)) => {
                if locked {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(AuthError {
                            error: "Account temporarily locked due to too many failed attempts. Try again in 15 minutes.".to_string(),
                        }),
                    ));
                } else {
                    tracing::info!(
                        username = %request.username,
                        remaining_attempts = remaining,
                        "failed login attempt"
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to record failed attempt");
            }
        }
        return Err(auth_error());
    }

    // Successful login - clear any failed attempts
    let _ = state.lockout().clear_attempts(&request.username).await;

    // Update login timestamp
    if let Err(e) = User::touch_login(state.db(), user.id).await {
        tracing::warn!(error = %e, user_id = %user.id, "failed to update login timestamp");
    }

    // Create session
    session
        .insert(SESSION_USER_ID, user.id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert user_id into session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

    // Set default stage (None = live)
    session
        .insert(SESSION_ACTIVE_STAGE, Option::<String>::None)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert active_stage into session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

    // Store remember_me preference in session
    session
        .insert(SESSION_REMEMBER_ME, request.remember_me)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert remember_me into session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

    if request.remember_me {
        info!(user_id = %user.id, "user logged in with remember_me");
    } else {
        info!(user_id = %user.id, "user logged in");
    }

    Ok(Json(LoginResponse {
        success: true,
        message: "Login successful".to_string(),
    }))
}

/// Logout handler.
///
/// GET /user/logout
/// - Deletes session from Redis
/// - Clears session cookie
async fn logout(session: Session) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
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
