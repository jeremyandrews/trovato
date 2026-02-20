//! Authentication routes (login, logout, registration, email verification).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use tracing::info;

use crate::form::csrf::{generate_csrf_token, verify_csrf_token};
use crate::middleware::language::SESSION_ACTIVE_LANGUAGE;
use crate::models::email_verification::EmailVerificationToken;
use crate::models::{CreateUser, SiteConfig, User};
use crate::routes::helpers::{CsrfOnlyForm, html_escape};
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
<input type="hidden" name="_token" value="{csrf_token}">
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
    // Verify CSRF token — always required, reject if missing
    match &form.csrf_token {
        Some(token) => match verify_csrf_token(&session, token).await {
            Ok(true) => {}
            _ => {
                return render_login_error(
                    &state,
                    &session,
                    "Invalid form token. Please try again.",
                )
                .await;
            }
        },
        None => {
            return render_login_error(&state, &session, "Missing form token. Please try again.")
                .await;
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
/// POST /user/logout
/// - Validates CSRF token
/// - Dispatches tap_user_logout before destroying session
/// - Deletes session from Redis
/// - Clears session cookie
async fn logout(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CsrfOnlyForm>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
    // Verify CSRF token
    let valid = crate::form::csrf::verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);
    if !valid {
        return Err((
            StatusCode::FORBIDDEN,
            Json(AuthError {
                error: "Invalid or expired form token. Please try again.".to_string(),
            }),
        ));
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

// ─── Registration ────────────────────────────────────────────────────────────

/// Registration form request (deserialized from form POST).
#[derive(Debug, Deserialize)]
struct RegisterFormRequest {
    username: String,
    mail: String,
    password: String,
    confirm_password: String,
    #[serde(rename = "_token")]
    csrf_token: Option<String>,
}

/// JSON registration request body.
#[derive(Debug, Deserialize)]
struct RegisterJsonRequest {
    username: String,
    mail: String,
    password: String,
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
    Form(form): Form<RegisterFormRequest>,
) -> Response {
    if !is_registration_enabled(&state).await {
        return (StatusCode::NOT_FOUND, "Registration is not enabled.").into_response();
    }

    // Verify CSRF token
    match &form.csrf_token {
        Some(token) => match verify_csrf_token(&session, token).await {
            Ok(true) => {}
            _ => {
                let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
                let errors = vec!["Invalid form token. Please try again.".to_string()];
                return render_register_form(&state, &csrf_token, Some(&errors), None, None).await;
            }
        },
        None => {
            let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
            let errors = vec!["Missing form token. Please try again.".to_string()];
            return render_register_form(&state, &csrf_token, Some(&errors), None, None).await;
        }
    }

    let values = serde_json::json!({
        "username": form.username,
        "mail": form.mail,
    });

    // Validate input
    let mut errors = Vec::new();
    validate_registration_input(
        &state,
        &form.username,
        &form.mail,
        &form.password,
        &mut errors,
    )
    .await;

    if form.password != form.confirm_password {
        errors.push("Passwords do not match.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        return render_register_form(&state, &csrf_token, Some(&errors), None, Some(&values)).await;
    }

    // Create inactive user
    match do_register(&state, &form.username, &form.mail, &form.password).await {
        Ok(_) => {
            let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
            render_register_form(
                &state,
                &csrf_token,
                None,
                Some(
                    "Registration successful! Check your email for a verification link \
                     to activate your account.",
                ),
                None,
            )
            .await
        }
        Err(e) => {
            tracing::error!(error = %e, "registration failed");
            let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
            let errors = vec!["An unexpected error occurred. Please try again later.".to_string()];
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

    if username.is_empty() {
        errors.push("Username is required.".to_string());
    } else if username.len() > 60 {
        errors.push("Username must be 60 characters or fewer.".to_string());
    }

    if mail.is_empty() {
        errors.push("Email address is required.".to_string());
    } else if !mail.contains('@') || !mail.contains('.') {
        errors.push("Please enter a valid email address.".to_string());
    }

    if password.len() < 8 {
        errors.push("Password must be at least 8 characters.".to_string());
    }

    // Check username uniqueness
    if !username.is_empty()
        && let Ok(Some(_)) = User::find_by_name(state.db(), username).await
    {
        errors.push(format!(
            "Username '{}' is already taken.",
            html_escape(username)
        ));
    }

    // Check email uniqueness
    if !mail.is_empty()
        && mail.contains('@')
        && let Ok(Some(_)) = User::find_by_mail(state.db(), mail).await
    {
        errors.push("An account with that email address already exists.".to_string());
    }
}

/// Core registration logic shared by form and JSON handlers.
async fn do_register(
    state: &AppState,
    username: &str,
    mail: &str,
    password: &str,
) -> anyhow::Result<uuid::Uuid> {
    let input = CreateUser {
        name: username.trim().to_string(),
        password: password.to_string(),
        mail: mail.trim().to_string(),
        is_admin: false,
    };

    // Create user with status=0 (inactive, pending email verification)
    let user = User::create_with_status(state.db(), input, 0).await?;

    // Create verification token
    let (_, plain_token) = EmailVerificationToken::create(state.db(), user.id).await?;

    // Send verification email
    let site_name = SiteConfig::site_name(state.db()).await.unwrap_or_default();
    if let Some(email_service) = state.email() {
        if let Err(e) = email_service
            .send_verification_email(mail.trim(), &plain_token, &site_name)
            .await
        {
            tracing::error!(error = %e, "failed to send verification email");
            // Log the URL as fallback for dev environments
            tracing::debug!(
                verify_url = format!("/user/verify/{}", plain_token),
                "Verification URL (email send failed)"
            );
        } else {
            info!(user_id = %user.id, email = %mail.trim(), "verification email sent");
        }
    } else {
        // SMTP not configured — log the token for development
        tracing::debug!(
            user_id = %user.id,
            email = %mail.trim(),
            token = %plain_token,
            "Registration verification (SMTP not configured, token logged)"
        );
        tracing::debug!(
            verify_url = format!("/user/verify/{}", plain_token),
            "Verification URL for testing"
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
    Ok(user.id)
}

/// JSON registration handler.
///
/// POST /user/register/json
/// - Creates inactive user, sends verification email
/// - Returns JSON response
async fn register_json(
    State(state): State<AppState>,
    Json(request): Json<RegisterJsonRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
    if !is_registration_enabled(&state).await {
        return Err((
            StatusCode::NOT_FOUND,
            Json(AuthError {
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

    if !errors.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(AuthError {
                error: errors.join(" "),
            }),
        ));
    }

    match do_register(&state, &request.username, &request.mail, &request.password).await {
        Ok(_) => Ok(Json(LoginResponse {
            success: true,
            message: "Registration successful. Check your email for a verification link."
                .to_string(),
        })),
        Err(e) => {
            tracing::error!(error = %e, "JSON registration failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Registration failed. Please try again later.".to_string(),
                }),
            ))
        }
    }
}

/// Email verification handler.
///
/// GET /user/verify/{token}
/// - Validates the token, activates the user, redirects to login
async fn verify_email(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let verification = match EmailVerificationToken::find_valid(state.db(), &token).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            let mut context = tera::Context::new();
            context.insert("csrf_token", "");
            context.insert(
                "error",
                "Invalid or expired verification link. Please register again.",
            );
            return match state.theme().tera().render("user/register.html", &context) {
                Ok(html) => Html(html).into_response(),
                Err(_) => Html(
                    "<h1>Verification Failed</h1>\
                     <p>Invalid or expired verification link.</p>\
                     <p><a href=\"/user/register\">Register again</a></p>"
                        .to_string(),
                )
                .into_response(),
            };
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during email verification");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response();
        }
    };

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

    // Mark token as used
    if let Err(e) = EmailVerificationToken::mark_used(state.db(), verification.id).await {
        tracing::warn!(error = %e, "failed to mark verification token as used");
    }

    // Invalidate any other verification tokens for this user
    if let Err(e) =
        EmailVerificationToken::invalidate_user_tokens(state.db(), verification.user_id).await
    {
        tracing::warn!(error = %e, "failed to invalidate remaining verification tokens");
    }

    info!(user_id = %verification.user_id, "email verified, account activated");

    // Render login page with success message
    let mut context = tera::Context::new();
    context.insert("csrf_token", "");
    context.insert(
        "success",
        "Your account has been verified! You can now log in.",
    );

    match state.theme().tera().render("user/login.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(_) => Redirect::to("/user/login").into_response(),
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
}
