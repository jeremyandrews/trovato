//! Authentication routes (login, logout, registration, email verification).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use tower_sessions::Session;
use tracing::info;

use crate::form::csrf::generate_csrf_token;
use crate::middleware::language::SESSION_ACTIVE_LANGUAGE;
use crate::models::email_verification::{
    EmailVerificationToken, PURPOSE_EMAIL_CHANGE, PURPOSE_REGISTRATION,
};
use crate::models::{CreateUser, SiteConfig, User};
use crate::routes::helpers::{
    CsrfOnlyForm, JsonError, JsonSuccess, html_escape, is_valid_email, is_valid_timezone,
    require_csrf, validate_password, validate_username,
};
use crate::state::AppState;

/// Check if an anyhow error wraps a sqlx unique constraint violation.
fn is_unique_violation(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        if let Some(sqlx::Error::Database(db_err)) = cause.downcast_ref::<sqlx::Error>() {
            db_err.is_unique_violation()
        } else {
            false
        }
    })
}

/// Session key for storing the authenticated user ID.
pub const SESSION_USER_ID: &str = "user_id";

/// Session key for storing the active stage.
pub const SESSION_ACTIVE_STAGE: &str = "active_stage";

/// Session key for remember_me flag.
pub const SESSION_REMEMBER_ME: &str = "remember_me";

/// Login request body.
#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: bool,
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
            let escaped_token = html_escape(&csrf_token);
            Html(format!(
                r#"<!DOCTYPE html>
<html><head><title>Log in</title></head>
<body style="font-family: sans-serif; max-width: 400px; margin: 100px auto; padding: 2rem;">
<h1>Log in</h1>
<form method="post" action="/user/login">
<input type="hidden" name="_token" value="{escaped_token}">
<p><label>Username<br><input type="text" name="username" required></label></p>
<p><label>Password<br><input type="password" name="password" required></label></p>
<p><button type="submit">Log in</button></p>
</form>
</body></html>"#
            ))
            .into_response()
        }
    }
}

/// Form-based login request.
#[derive(Deserialize)]
pub struct LoginFormRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: Option<String>,
    #[serde(rename = "_token")]
    pub csrf_token: String,
}

/// Form-based login handler.
///
/// POST /user/login (form data)
async fn login_form_submit(
    State(state): State<AppState>,
    session: Session,
    headers: axum::http::HeaderMap,
    Form(form): Form<LoginFormRequest>,
) -> Response {
    // Rate limit login attempts by IP
    let client_id = crate::middleware::get_client_id(None, &headers);
    if let Err(retry_after) = state.rate_limiter().check("login", &client_id).await {
        return crate::middleware::rate_limit_response(retry_after);
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.csrf_token).await {
        return resp;
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
            html_escape(error)
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
    // Rotate session ID to prevent session fixation attacks.
    session.cycle_id().await.map_err(|e| {
        tracing::error!(error = %e, "failed to rotate session ID");
        LoginError::Internal("Internal server error".to_string())
    })?;

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
            if let Err(e) = state
                .lockout()
                .record_failed_attempt(&request.username)
                .await
            {
                tracing::warn!(error = %e, username = %request.username, "failed to record failed login attempt");
            }
            return Err(LoginError::InvalidCredentials);
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during login");
            return Err(LoginError::Internal("Internal server error".to_string()));
        }
    };

    // Check if user is active
    if !user.is_active() {
        if let Err(e) = state
            .lockout()
            .record_failed_attempt(&request.username)
            .await
        {
            tracing::warn!(error = %e, username = %request.username, "failed to record failed login attempt");
        }
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
    if let Err(e) = state.lockout().clear_attempts(&request.username).await {
        tracing::warn!(error = %e, username = %request.username, "failed to clear login attempts");
    }

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
    headers: axum::http::HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Json<JsonSuccess>, (StatusCode, Json<JsonError>)> {
    // Rate limit login attempts by IP
    let client_id = crate::middleware::get_client_id(None, &headers);
    if let Err(retry_after) = state.rate_limiter().check("login", &client_id).await {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(JsonError {
                error: format!("Rate limit exceeded. Retry after {retry_after} seconds."),
            }),
        ));
    }

    match do_login(&state, &session, &request).await {
        Ok(()) => Ok(Json(JsonSuccess {
            success: true,
            message: "Login successful".to_string(),
        })),
        Err(e) => {
            let status = e.status_code();
            Err((
                status,
                Json(JsonError {
                    error: e.message().to_string(),
                }),
            ))
        }
    }
}

/// Logout handler.
///
/// POST /user/logout
/// - Validates CSRF token
/// - Dispatches tap_user_logout before destroying session
/// - Deletes session from Redis
/// - Clears session cookie
async fn logout(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

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

    if let Err(e) = session.delete().await {
        tracing::error!(error = %e, "failed to delete session");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response();
    }

    Redirect::to("/user/login").into_response()
}

// ─── Registration ────────────────────────────────────────────────────────────

/// Registration form request (deserialized from form POST).
#[derive(Deserialize)]
struct RegisterFormRequest {
    username: String,
    mail: String,
    password: String,
    confirm_password: String,
    #[serde(rename = "_token")]
    csrf_token: String,
}

/// JSON registration request body.
#[derive(Deserialize)]
struct RegisterJsonRequest {
    username: String,
    mail: String,
    password: String,
    #[serde(default)]
    confirm_password: Option<String>,
}

/// Check whether user registration is enabled via site config.
async fn is_registration_enabled(state: &AppState) -> bool {
    SiteConfig::get(state.db(), "allow_user_registration")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Registration form handler.
///
/// GET /user/register
/// - Renders registration form with CSRF token
/// - Returns 404 if registration is disabled
async fn register_form(State(state): State<AppState>, session: Session) -> Response {
    // Check if already logged in
    if session
        .get::<uuid::Uuid>(SESSION_USER_ID)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return Redirect::to("/admin").into_response();
    }

    if !is_registration_enabled(&state).await {
        return (StatusCode::NOT_FOUND, "Registration is not enabled.").into_response();
    }

    let csrf_token = match generate_csrf_token(&session).await {
        Ok(token) => token,
        Err(e) => {
            tracing::error!(error = %e, "failed to generate CSRF token");
            return Html("<h1>Error</h1><p>Failed to generate form token</p>".to_string())
                .into_response();
        }
    };

    render_register_form(&state, &csrf_token, None, None, None).await
}

/// Render the registration form with optional context.
async fn render_register_form(
    state: &AppState,
    csrf_token: &str,
    errors: Option<&[String]>,
    success: Option<&str>,
    values: Option<&serde_json::Value>,
) -> Response {
    let mut context = tera::Context::new();
    context.insert("csrf_token", csrf_token);
    if let Some(errors) = errors {
        context.insert("errors", errors);
    }
    if let Some(success) = success {
        context.insert("success", success);
    }
    if let Some(values) = values {
        context.insert("values", values);
    }

    match state.theme().tera().render("user/register.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render register form");
            Html("<h1>Error</h1><p>Failed to render registration form</p>".to_string())
                .into_response()
        }
    }
}

/// Registration form submit handler.
///
/// POST /user/register
/// - Validates CSRF token, form input, uniqueness
/// - Creates inactive user, sends verification email
async fn register_form_submit(
    State(state): State<AppState>,
    session: Session,
    headers: axum::http::HeaderMap,
    Form(form): Form<RegisterFormRequest>,
) -> Response {
    if !is_registration_enabled(&state).await {
        return (StatusCode::NOT_FOUND, "Registration is not enabled.").into_response();
    }

    // Rate limit registration attempts (separate bucket from login)
    let client_id = crate::middleware::get_client_id(None, &headers);
    if let Err(retry_after) = state.rate_limiter().check("register", &client_id).await {
        return crate::middleware::rate_limit_response(retry_after);
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.csrf_token).await {
        return resp;
    }

    let username = form.username.trim().to_string();
    let mail = form.mail.trim().to_string();

    let values = serde_json::json!({
        "username": username,
        "mail": mail,
    });

    // Validate input
    let mut errors = Vec::new();
    validate_registration_input(&state, &username, &mail, &form.password, &mut errors).await;

    if form.password != form.confirm_password {
        errors.push("Passwords do not match.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        return render_register_form(&state, &csrf_token, Some(&errors), None, Some(&values)).await;
    }

    // Create inactive user
    match do_register(&state, &username, &mail, &form.password).await {
        Ok(result) => {
            let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
            let message = if result.email_sent {
                "Registration successful! Check your email for a verification link \
                 to activate your account."
            } else {
                "Registration successful! However, we were unable to send the \
                 verification email. Please contact the site administrator to \
                 activate your account."
            };
            render_register_form(&state, &csrf_token, None, Some(message), None).await
        }
        Err(e) => {
            tracing::error!(error = %e, "registration failed");
            let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
            // Check for DB unique constraint violation (TOCTOU race)
            let msg = if is_unique_violation(&e) {
                "Username or email is already in use."
            } else {
                "An unexpected error occurred. Please try again later."
            };
            let errors = vec![msg.to_string()];
            render_register_form(&state, &csrf_token, Some(&errors), None, Some(&values)).await
        }
    }
}

/// Validate registration input fields.
async fn validate_registration_input(
    state: &AppState,
    username: &str,
    mail: &str,
    password: &str,
    errors: &mut Vec<String>,
) {
    let username = username.trim();
    let mail = mail.trim();

    if let Err(msg) = validate_username(username) {
        errors.push(msg.to_string());
    }

    if mail.is_empty() {
        errors.push("Email address is required.".to_string());
    } else if !is_valid_email(mail) {
        errors.push("Please enter a valid email address.".to_string());
    }

    if let Err(msg) = validate_password(password) {
        errors.push(msg.to_string());
    }

    // Check username and email uniqueness — use generic message to prevent enumeration
    let mut uniqueness_conflict = false;

    if !username.is_empty()
        && let Ok(Some(_)) = User::find_by_name(state.db(), username).await
    {
        uniqueness_conflict = true;
    }

    if !mail.is_empty()
        && is_valid_email(mail)
        && let Ok(Some(_)) = User::find_by_mail(state.db(), mail).await
    {
        uniqueness_conflict = true;
    }

    if uniqueness_conflict {
        errors.push("Username or email is already in use.".to_string());
    }
}

/// Result of a successful registration.
struct RegistrationResult {
    /// Whether the verification email was actually sent.
    email_sent: bool,
}

/// Core registration logic shared by form and JSON handlers.
async fn do_register(
    state: &AppState,
    username: &str,
    mail: &str,
    password: &str,
) -> anyhow::Result<RegistrationResult> {
    let input = CreateUser {
        name: username.trim().to_string(),
        password: password.to_string(),
        mail: mail.trim().to_string(),
        is_admin: false,
    };

    // Create user with status=0 (inactive, pending email verification)
    let user = User::create_with_status(state.db(), input, 0).await?;

    // Create verification token
    let (_, plain_token) =
        EmailVerificationToken::create(state.db(), user.id, PURPOSE_REGISTRATION).await?;

    // Send verification email
    let site_name = SiteConfig::site_name(state.db()).await.unwrap_or_default();
    let mut email_sent = false;
    if let Some(email_service) = state.email() {
        match email_service
            .send_verification_email(mail.trim(), &plain_token, &site_name)
            .await
        {
            Ok(()) => {
                email_sent = true;
                info!(user_id = %user.id, email = %mail.trim(), "verification email sent");
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to send verification email");
            }
        }
    } else {
        tracing::warn!(
            user_id = %user.id,
            email = %mail.trim(),
            "SMTP not configured; verification email not sent"
        );
    }

    // Dispatch tap_user_register
    let tap_input = serde_json::json!({ "user_id": user.id.to_string() });
    let tap_state =
        crate::tap::RequestState::without_services(crate::tap::UserContext::anonymous());
    state
        .tap_dispatcher()
        .dispatch("tap_user_register", &tap_input.to_string(), tap_state)
        .await;

    info!(user_id = %user.id, name = %username.trim(), "user registered (pending verification)");
    Ok(RegistrationResult { email_sent })
}

/// JSON registration handler.
///
/// POST /user/register/json
/// - Creates inactive user, sends verification email
/// - Returns JSON response
async fn register_json(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<RegisterJsonRequest>,
) -> Result<Json<JsonSuccess>, (StatusCode, Json<JsonError>)> {
    // Rate limit registration attempts (separate bucket from login)
    let client_id = crate::middleware::get_client_id(None, &headers);
    if let Err(retry_after) = state.rate_limiter().check("register", &client_id).await {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(JsonError {
                error: format!("Rate limit exceeded. Retry after {retry_after} seconds."),
            }),
        ));
    }

    if !is_registration_enabled(&state).await {
        return Err((
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Registration is not enabled.".to_string(),
            }),
        ));
    }

    // Validate
    let mut errors = Vec::new();
    validate_registration_input(
        &state,
        &request.username,
        &request.mail,
        &request.password,
        &mut errors,
    )
    .await;

    // Validate password confirmation if provided
    if let Some(ref confirm) = request.confirm_password
        && confirm != &request.password
    {
        errors.push("Passwords do not match.".to_string());
    }

    if !errors.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(JsonError {
                error: errors.join(" "),
            }),
        ));
    }

    match do_register(&state, &request.username, &request.mail, &request.password).await {
        Ok(result) => {
            let message = if result.email_sent {
                "Registration successful. Check your email for a verification link."
            } else {
                "Registration successful. However, the verification email could not be sent. \
                 Please contact the site administrator."
            };
            Ok(Json(JsonSuccess {
                success: true,
                message: message.to_string(),
            }))
        }
        Err(e) => {
            tracing::error!(error = %e, "JSON registration failed");
            let (status, msg) = if is_unique_violation(&e) {
                (StatusCode::CONFLICT, "Username or email is already in use.")
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Registration failed. Please try again later.",
                )
            };
            Err((
                status,
                Json(JsonError {
                    error: msg.to_string(),
                }),
            ))
        }
    }
}

/// Email verification handler.
///
/// GET /user/verify/{token}
/// - Validates the token, activates the user, redirects to login
async fn verify_email(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(token): Path<String>,
) -> Response {
    // Rate limit verification attempts to prevent token brute-force
    let client_id = crate::middleware::get_client_id(None, &headers);
    if let Err(retry_after) = state.rate_limiter().check("verify_email", &client_id).await {
        return crate::middleware::rate_limit_response(retry_after);
    }

    let verification =
        match EmailVerificationToken::find_valid(state.db(), &token, PURPOSE_REGISTRATION).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                return Html(
                    "<h1>Verification Failed</h1>\
                     <p>Invalid or expired verification link.</p>\
                     <p><a href=\"/user/register\">Register again</a></p>"
                        .to_string(),
                )
                .into_response();
            }
            Err(e) => {
                tracing::error!(error = %e, "database error during email verification");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
                    .into_response();
            }
        };

    // Mark token as used first to prevent replay if activation fails midway
    if let Err(e) = EmailVerificationToken::mark_used(state.db(), verification.id).await {
        tracing::warn!(error = %e, "failed to mark verification token as used");
    }

    // Invalidate any other verification tokens for this user
    if let Err(e) =
        EmailVerificationToken::invalidate_user_tokens(state.db(), verification.user_id).await
    {
        tracing::warn!(error = %e, "failed to invalidate remaining verification tokens");
    }

    // Activate the user (status=1)
    if let Err(e) = User::update(
        state.db(),
        verification.user_id,
        crate::models::UpdateUser {
            status: Some(1),
            ..Default::default()
        },
    )
    .await
    {
        tracing::error!(error = %e, user_id = %verification.user_id, "failed to activate user");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to activate account",
        )
            .into_response();
    }

    info!(user_id = %verification.user_id, "email verified, account activated");

    Redirect::to("/user/login").into_response()
}

/// Email change verification handler.
///
/// GET /user/verify-email/{token}
/// - Validates the token, updates the user's email to the pending address
async fn verify_email_change(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(token): Path<String>,
) -> Response {
    // Rate limit verification attempts to prevent token brute-force
    let client_id = crate::middleware::get_client_id(None, &headers);
    if let Err(retry_after) = state.rate_limiter().check("verify_email", &client_id).await {
        return crate::middleware::rate_limit_response(retry_after);
    }

    let verification =
        match EmailVerificationToken::find_valid(state.db(), &token, PURPOSE_EMAIL_CHANGE).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                return Html(
                    "<h1>Verification Failed</h1>\
                 <p>Invalid or expired verification link.</p>\
                 <p><a href=\"/user/profile\">Return to profile</a></p>"
                        .to_string(),
                )
                .into_response();
            }
            Err(e) => {
                tracing::error!(error = %e, "database error during email change verification");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
                    .into_response();
            }
        };

    // Get the user and check for pending_email in data
    let Some(user) = User::find_by_id(state.db(), verification.user_id)
        .await
        .ok()
        .flatten()
    else {
        return (StatusCode::NOT_FOUND, "User not found").into_response();
    };

    let pending_email = user
        .data
        .get("pending_email")
        .and_then(|v| v.as_str())
        .map(String::from);

    let Some(new_email) = pending_email else {
        return Html(
            "<h1>No Pending Change</h1>\
             <p>There is no pending email change for this account.</p>\
             <p><a href=\"/user/profile\">Return to profile</a></p>"
                .to_string(),
        )
        .into_response();
    };

    // Re-validate the pending email address (defensive — the value was validated
    // when the change was requested, but user.data is mutable by other code paths)
    if !is_valid_email(&new_email) {
        let _ = EmailVerificationToken::mark_used(state.db(), verification.id).await;
        return Html(
            "<h1>Email Change Failed</h1>\
             <p>The pending email address is invalid. \
             Please update your profile with a valid email address.</p>\
             <p><a href=\"/user/profile\">Return to profile</a></p>"
                .to_string(),
        )
        .into_response();
    }

    // Re-check email uniqueness at verification time to prevent race condition
    // (another user may have registered with this email since the change was requested)
    if let Ok(Some(_)) = User::find_by_mail(state.db(), &new_email).await {
        // Mark token as used since the change can't proceed
        let _ = EmailVerificationToken::mark_used(state.db(), verification.id).await;
        return Html(
            "<h1>Email Change Failed</h1>\
             <p>This email address is now in use by another account. \
             Please update your profile with a different email address.</p>\
             <p><a href=\"/user/profile\">Return to profile</a></p>"
                .to_string(),
        )
        .into_response();
    }

    // Mark token as used first to prevent replay if email update fails midway
    if let Err(e) = EmailVerificationToken::mark_used(state.db(), verification.id).await {
        tracing::warn!(error = %e, "failed to mark email change token as used");
    }

    // Invalidate remaining tokens
    if let Err(e) =
        EmailVerificationToken::invalidate_user_tokens(state.db(), verification.user_id).await
    {
        tracing::warn!(error = %e, "failed to invalidate remaining verification tokens");
    }

    // Update the user's email and clear pending_email from data
    let mut data = user.data.clone();
    if let Some(obj) = data.as_object_mut() {
        obj.remove("pending_email");
    }

    let update = crate::models::UpdateUser {
        mail: Some(new_email),
        data: Some(data),
        ..Default::default()
    };

    if let Err(e) = User::update(state.db(), user.id, update).await {
        tracing::error!(error = %e, user_id = %user.id, "failed to update email");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to update email").into_response();
    }

    info!(user_id = %user.id, "email address updated via verification");

    Redirect::to("/user/profile").into_response()
}

// ─── Profile & Password Management ──────────────────────────────────────────

/// Profile edit form request.
#[derive(Deserialize)]
struct ProfileFormRequest {
    name: String,
    mail: String,
    #[serde(default)]
    timezone: String,
    #[serde(default)]
    current_password: String,
    #[serde(rename = "_token")]
    csrf_token: String,
}

/// Password change form request.
#[derive(Deserialize)]
struct PasswordChangeRequest {
    current_password: String,
    new_password: String,
    confirm_password: String,
    #[serde(rename = "_token")]
    csrf_token: String,
}

/// Get the current active user from session, or redirect to login.
///
/// Returns the user only if they exist AND have an active account (status=1).
/// Blocked or deactivated users are redirected to login.
async fn get_current_user(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: uuid::Uuid = session
        .get(SESSION_USER_ID)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| Redirect::to("/user/login").into_response())?;

    let user = User::find_by_id(state.db(), user_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| Redirect::to("/user/login").into_response())?;

    // Reject blocked/deactivated users — destroy their session
    if !user.is_active() {
        let _ = session.delete().await;
        return Err(Redirect::to("/user/login").into_response());
    }

    Ok(user)
}

/// Render the profile page with context.
///
/// When `values` is provided, those values override the user's stored values
/// (used to preserve submitted form data on validation errors).
///
/// Uses separate CSRF tokens for the profile and password forms since
/// tokens are single-use (consumed on verification).
async fn render_profile(
    state: &AppState,
    profile_csrf: &str,
    password_csrf: &str,
    user: &User,
    errors: Option<&[String]>,
    success: Option<&str>,
    error: Option<&str>,
    values: Option<&serde_json::Value>,
) -> Response {
    let mut context = tera::Context::new();
    context.insert("csrf_token", profile_csrf);
    context.insert("password_csrf_token", password_csrf);
    context.insert(
        "user",
        values.unwrap_or(&serde_json::json!({
            "name": user.name,
            "mail": user.mail,
            "timezone": user.timezone,
        })),
    );
    if let Some(errors) = errors {
        context.insert("errors", errors);
    }
    if let Some(success) = success {
        context.insert("success", success);
    }
    if let Some(error) = error {
        context.insert("error", error);
    }

    match state.theme().tera().render("user/profile.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render profile form");
            Html("<h1>Error</h1><p>Failed to render profile page</p>".to_string()).into_response()
        }
    }
}

/// Generate a pair of CSRF tokens for the profile page (one per form).
async fn profile_csrf_pair(session: &Session) -> (String, String) {
    let profile = generate_csrf_token(session).await.unwrap_or_default();
    let password = generate_csrf_token(session).await.unwrap_or_default();
    (profile, password)
}

/// Profile view/edit form handler.
///
/// GET /user/profile
async fn profile_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match get_current_user(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let (profile_csrf, password_csrf) = profile_csrf_pair(&session).await;
    render_profile(
        &state,
        &profile_csrf,
        &password_csrf,
        &user,
        None,
        None,
        None,
        None,
    )
    .await
}

/// Profile update handler.
///
/// POST /user/profile
async fn profile_update(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ProfileFormRequest>,
) -> Response {
    let user = match get_current_user(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    // Rate limit profile updates per user
    if let Err(retry_after) = state
        .rate_limiter()
        .check("profile", &user.id.to_string())
        .await
    {
        return crate::middleware::rate_limit_response(retry_after);
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.csrf_token).await {
        return resp;
    }

    let name = form.name.trim();
    let mail = form.mail.trim();
    let timezone = form.timezone.trim();
    let email_changing = !mail.eq_ignore_ascii_case(&user.mail);
    let name_changing = !name.eq_ignore_ascii_case(&user.name);

    // Build form values for re-rendering on validation errors
    let form_values = serde_json::json!({
        "name": name,
        "mail": mail,
        "timezone": timezone,
    });

    let mut errors = Vec::new();

    if let Err(msg) = validate_username(name) {
        errors.push(msg.to_string());
    }

    if mail.is_empty() {
        errors.push("Email address is required.".to_string());
    } else if !is_valid_email(mail) {
        errors.push("Please enter a valid email address.".to_string());
    }

    if !timezone.is_empty() && !is_valid_timezone(timezone) {
        errors.push("Please enter a valid timezone (e.g., America/New_York).".to_string());
    }

    // Require current password when changing username or email (login credentials)
    if email_changing || name_changing {
        // Check lockout before attempting password verification
        if let Ok(true) = state.lockout().is_locked(&user.name).await {
            errors.push("Account temporarily locked due to too many failed attempts.".to_string());
        } else if form.current_password.is_empty() {
            errors
                .push("Current password is required to change your username or email.".to_string());
        } else if !user.verify_password(&form.current_password) {
            // Record failed attempt for lockout tracking
            if let Err(e) = state.lockout().record_failed_attempt(&user.name).await {
                tracing::warn!(error = %e, "failed to record failed password attempt");
            }
            errors.push("Current password is incorrect.".to_string());
        }
    }

    // Check uniqueness — use generic message to prevent enumeration.
    // Exclude the user's own record to allow case-only changes (e.g. alice → Alice).
    let mut uniqueness_conflict = false;

    if name_changing
        && !name.is_empty()
        && let Ok(Some(existing)) = User::find_by_name(state.db(), name).await
        && existing.id != user.id
    {
        uniqueness_conflict = true;
    }

    if email_changing
        && !mail.is_empty()
        && let Ok(Some(existing)) = User::find_by_mail(state.db(), mail).await
        && existing.id != user.id
    {
        uniqueness_conflict = true;
    }

    if uniqueness_conflict {
        errors.push("Username or email is already in use.".to_string());
    }

    if !errors.is_empty() {
        let (pc, pwc) = profile_csrf_pair(&session).await;
        return render_profile(
            &state,
            &pc,
            &pwc,
            &user,
            Some(&errors),
            None,
            None,
            Some(&form_values),
        )
        .await;
    }

    // Build update — do NOT change email directly; require verification.
    // Always persist name changes including case-only changes (e.g. alice → Alice).
    let update = crate::models::UpdateUser {
        name: if name != user.name {
            Some(name.to_string())
        } else {
            None
        },
        timezone: if timezone != user.timezone.as_deref().unwrap_or("") {
            Some(timezone.to_string())
        } else {
            None
        },
        ..Default::default()
    };

    // If email is changing, store pending_email and send verification
    let pending_email_data = if email_changing {
        let mut data = user.data.clone();
        if let Some(obj) = data.as_object_mut() {
            obj.insert(
                "pending_email".to_string(),
                serde_json::Value::String(mail.to_string()),
            );
        }
        Some(data)
    } else {
        None
    };

    // Merge data update into the main update if needed
    let update = if let Some(data) = pending_email_data {
        crate::models::UpdateUser {
            data: Some(data),
            ..update
        }
    } else {
        update
    };

    match User::update(state.db(), user.id, update).await {
        Ok(Some(updated_user)) => {
            // Send verification email if email is changing
            if email_changing {
                // Invalidate any outstanding tokens before creating a new one
                if let Err(e) =
                    EmailVerificationToken::invalidate_user_tokens(state.db(), user.id).await
                {
                    tracing::warn!(error = %e, "failed to invalidate old verification tokens");
                }

                // Create verification token
                match EmailVerificationToken::create(state.db(), user.id, PURPOSE_EMAIL_CHANGE)
                    .await
                {
                    Ok((_, plain_token)) => {
                        let site_name = SiteConfig::site_name(state.db()).await.unwrap_or_default();
                        if let Some(email_service) = state.email() {
                            let verify_url = format!(
                                "{}/user/verify-email/{}",
                                email_service.site_url(),
                                plain_token
                            );
                            let body = format!(
                                "You requested to change your email address at {site_name}.\n\n\
                                 To confirm this change, visit the following link:\n\
                                 {verify_url}\n\n\
                                 If you did not request this change, you can safely ignore this email.\n\n\
                                 This link will expire in 24 hours."
                            );
                            if let Err(e) = email_service
                                .send(mail, &format!("Confirm email change at {site_name}"), &body)
                                .await
                            {
                                tracing::warn!(
                                    error = %e,
                                    "failed to send email change verification"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to create email change token");
                    }
                }
            }

            // Dispatch tap_user_update
            let tap_input = serde_json::json!({ "user_id": user.id.to_string() });
            let tap_state = crate::tap::RequestState::without_services(
                crate::tap::UserContext::authenticated(user.id, vec![]),
            );
            state
                .tap_dispatcher()
                .dispatch("tap_user_update", &tap_input.to_string(), tap_state)
                .await;

            let (pc, pwc) = profile_csrf_pair(&session).await;
            let success_msg = if email_changing {
                "Profile updated. A verification email has been sent to your new address. \
                 Your email will be updated after you confirm the change."
            } else {
                "Profile updated successfully."
            };
            render_profile(
                &state,
                &pc,
                &pwc,
                &updated_user,
                None,
                Some(success_msg),
                None,
                None,
            )
            .await
        }
        Ok(None) => {
            let (pc, pwc) = profile_csrf_pair(&session).await;
            render_profile(
                &state,
                &pc,
                &pwc,
                &user,
                None,
                None,
                Some("User not found."),
                None,
            )
            .await
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update profile");
            let (pc, pwc) = profile_csrf_pair(&session).await;
            render_profile(
                &state,
                &pc,
                &pwc,
                &user,
                None,
                None,
                Some("An error occurred while saving your profile."),
                None,
            )
            .await
        }
    }
}

/// Password change handler.
///
/// POST /user/password
async fn password_change(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<PasswordChangeRequest>,
) -> Response {
    let user = match get_current_user(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    // Rate limit password changes per user
    if let Err(retry_after) = state
        .rate_limiter()
        .check("password", &user.id.to_string())
        .await
    {
        return crate::middleware::rate_limit_response(retry_after);
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.csrf_token).await {
        return resp;
    }

    let mut errors = Vec::new();

    // Check lockout before attempting password verification
    if let Ok(true) = state.lockout().is_locked(&user.name).await {
        errors.push("Account temporarily locked due to too many failed attempts.".to_string());
    } else if form.current_password.is_empty() {
        errors.push("Current password is required.".to_string());
    } else if !user.verify_password(&form.current_password) {
        // Record failed attempt for lockout tracking
        if let Err(e) = state.lockout().record_failed_attempt(&user.name).await {
            tracing::warn!(error = %e, "failed to record failed password attempt");
        }
        errors.push("Current password is incorrect.".to_string());
    }

    if let Err(msg) = validate_password(&form.new_password) {
        errors.push(msg.to_string());
    }

    if form.new_password != form.confirm_password {
        errors.push("New passwords do not match.".to_string());
    }

    if !errors.is_empty() {
        let (pc, pwc) = profile_csrf_pair(&session).await;
        return render_profile(&state, &pc, &pwc, &user, Some(&errors), None, None, None).await;
    }

    match User::update_password(state.db(), user.id, &form.new_password).await {
        Ok(true) => {
            // Rotate session ID so a stolen pre-change session is invalid
            if let Err(e) = session.cycle_id().await {
                tracing::warn!(error = %e, "failed to cycle session after password change");
            }

            // Dispatch tap_user_update
            let tap_input = serde_json::json!({ "user_id": user.id.to_string() });
            let tap_state = crate::tap::RequestState::without_services(
                crate::tap::UserContext::authenticated(user.id, vec![]),
            );
            state
                .tap_dispatcher()
                .dispatch("tap_user_update", &tap_input.to_string(), tap_state)
                .await;

            info!(user_id = %user.id, "password changed via self-service");

            let (pc, pwc) = profile_csrf_pair(&session).await;
            render_profile(
                &state,
                &pc,
                &pwc,
                &user,
                None,
                Some("Password changed successfully."),
                None,
                None,
            )
            .await
        }
        _ => {
            let (pc, pwc) = profile_csrf_pair(&session).await;
            render_profile(
                &state,
                &pc,
                &pwc,
                &user,
                None,
                None,
                Some("An error occurred while changing your password."),
                None,
            )
            .await
        }
    }
}

/// Create the auth router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/login", get(login_form).post(login_form_submit))
        .route("/user/login/json", post(login))
        .route("/user/logout", post(logout))
        .route(
            "/user/register",
            get(register_form).post(register_form_submit),
        )
        .route("/user/register/json", post(register_json))
        .route("/user/verify/{token}", get(verify_email))
        .route("/user/verify-email/{token}", get(verify_email_change))
        .route("/user/profile", get(profile_form).post(profile_update))
        .route("/user/password", post(password_change))
}
