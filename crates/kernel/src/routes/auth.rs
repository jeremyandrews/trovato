//! Authentication routes (login, logout, registration, email verification).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use tower_sessions::Session;
use tracing::info;

use crate::error::AppError;
use crate::form::csrf::generate_csrf_token;
use crate::middleware::language::SESSION_ACTIVE_LANGUAGE;
use crate::models::email_verification::{
    EmailVerificationToken, PURPOSE_EMAIL_CHANGE, PURPOSE_REGISTRATION,
};
use crate::models::{CreateUser, SiteConfig, User};
use crate::routes::helpers::{
    CsrfOnlyForm, JsonSuccess, is_valid_email, is_valid_timezone, require_csrf, validate_password,
    validate_username,
};
use crate::services::email::{MailSecret, NewMail};
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

/// Session key for a one-time notice the login page shows and removes, such as
/// "your password has been reset".
pub const SESSION_LOGIN_NOTICE: &str = "login_notice";

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
pub(crate) enum LoginError {
    /// Account temporarily locked due to too many failed attempts (429).
    Locked(String),
    /// Invalid credentials — wrong username or password (401).
    InvalidCredentials,
    /// An access rule turns this username or address away (403).
    Blocked(String),
    /// Internal server error — database failure, etc. (500).
    Internal(String),
}

impl LoginError {
    pub(crate) fn status_code(&self) -> StatusCode {
        match self {
            LoginError::Locked(_) => StatusCode::TOO_MANY_REQUESTS,
            LoginError::InvalidCredentials => StatusCode::UNAUTHORIZED,
            LoginError::Blocked(_) => StatusCode::FORBIDDEN,
            LoginError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            LoginError::Locked(msg) => msg,
            LoginError::InvalidCredentials => "Invalid username or password",
            LoginError::Blocked(msg) => msg,
            LoginError::Internal(msg) => msg,
        }
    }
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Whether `destination` is a page on this site that a sign in may land on.
///
/// A local absolute path (`routes::front::is_local_path`: no scheme, no host,
/// no `//` or `/\` that a browser reads as another host), and not the login
/// form itself, which would only show the form again. Anything else is
/// dropped and the sign in lands on `/`, so the parameter can never be used to
/// send a freshly signed-in visitor to another site.
pub fn is_safe_destination(destination: &str) -> bool {
    let path = destination.split(['?', '#']).next().unwrap_or("");
    super::front::is_local_path(destination) && path != "/user/login"
}

/// The `?destination=` a login form was opened with.
#[derive(Debug, Default, Deserialize)]
pub struct LoginQuery {
    #[serde(default)]
    pub destination: Option<String>,
}

/// Login form handler.
///
/// GET /user/login
/// - Renders login form with CSRF token
/// - `?destination=/some/page` is carried in the form, and a successful sign
///   in lands there (`comments.html` and the access-denied page link to it so)
async fn login_form(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<LoginQuery>,
) -> Response {
    let notice = session
        .remove::<String>(SESSION_LOGIN_NOTICE)
        .await
        .ok()
        .flatten();
    render_login_page(
        &state,
        &session,
        None,
        notice.as_deref(),
        query.destination.as_deref(),
    )
    .await
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
    /// The page to land on after signing in; `/` when absent or not local.
    #[serde(default)]
    pub destination: Option<String>,
}

/// Form-based login handler.
///
/// POST /user/login (form data)
async fn login_form_submit(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: axum::http::HeaderMap,
    Form(form): Form<LoginFormRequest>,
) -> Response {
    // The `login` bucket is counted once, per client IP, by the rate-limit
    // middleware (`categorize_path` puts this path in it). It used to be counted
    // here as well, which spent each attempt twice. The address is still the
    // lockout's key.
    let client_id = client_ip.0;

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
    let destination = form
        .destination
        .as_deref()
        .filter(|d| is_safe_destination(d));

    // Perform login
    match do_login(&state, &session, &request, &client_id, user_agent(&headers)).await {
        Ok(_) => Redirect::to(destination.unwrap_or("/")).into_response(),
        Err(e) => render_login_page(&state, &session, Some(e.message()), None, destination).await,
    }
}

/// Render the login form, with an error or a notice, keeping `destination`
/// (when it is a safe one) for the next attempt.
async fn render_login_page(
    state: &AppState,
    session: &Session,
    error: Option<&str>,
    notice: Option<&str>,
    destination: Option<&str>,
) -> Response {
    let csrf_token = generate_csrf_token(session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    if let Some(error) = error {
        context.insert("error", error);
    }
    if let Some(notice) = notice {
        context.insert("success", notice);
    }
    if let Some(destination) = destination.filter(|d| is_safe_destination(d)) {
        context.insert("destination", destination);
    }
    context.insert("webauthn_available", &state.webauthn().is_some());
    super::helpers::inject_site_context(state, session, &mut context, "/user/login").await;

    match state.theme().tera().render("user/login.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render login form");
            super::helpers::render_server_error("The login form could not be displayed.")
        }
    }
}

/// Initialize session state after successful authentication.
///
/// **The single session-establishment seam.** Its first act is
/// `session.cycle_id()`, which is the session-fixation defence (FR-6/FR-9), so
/// every authentication path must come through here rather than writing
/// `SESSION_USER_ID` itself. Passkey login (`routes::webauthn`) and the session
/// established after account recovery both call it for exactly that reason:
/// the invariant holds with no new code on the seam.
pub(crate) async fn setup_session(
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

/// Perform login and return typed error on failure, and record the attempt
/// in the site log (`log_type = "user"`): the username and the outcome, never
/// the password. The attempt is also a security event, `auth.login_succeeded`
/// or `auth.login_failed` with `details.method = "password"`, emitted the way
/// a passkey login emits its own.
async fn do_login(
    state: &AppState,
    session: &Session,
    request: &LoginRequest,
    client_ip: &str,
    user_agent: &str,
) -> Result<(), LoginError> {
    let result = attempt_login(state, session, request, client_ip).await;
    audit_password_login(state, session, request, client_ip, user_agent, &result).await;
    match &result {
        Ok(()) => info!(
            log_type = "user",
            username = %request.username,
            "user logged in"
        ),
        Err(LoginError::InvalidCredentials) => info!(
            log_type = "user",
            username = %request.username,
            "login failed: wrong username or password"
        ),
        Err(LoginError::Locked(_)) => info!(
            log_type = "user",
            username = %request.username,
            "login refused: sign in is locked"
        ),
        Err(LoginError::Blocked(_)) => info!(
            log_type = "user",
            username = %request.username,
            "login refused: an access rule blocks it"
        ),
        Err(LoginError::Internal(_)) => {}
    }
    result
}

/// The request's `User-Agent`, or empty.
fn user_agent(headers: &axum::http::HeaderMap) -> &str {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// The security event for one password login attempt.
async fn audit_password_login(
    state: &AppState,
    session: &Session,
    request: &LoginRequest,
    client_ip: &str,
    user_agent: &str,
    result: &Result<(), LoginError>,
) {
    use crate::audit::{SecurityEvent, SecurityEventKind};

    let event = match result {
        Ok(()) => {
            let user_id = session
                .get::<uuid::Uuid>(SESSION_USER_ID)
                .await
                .ok()
                .flatten();
            SecurityEvent::new(SecurityEventKind::LoginSucceeded).maybe_user(user_id)
        }
        Err(e) => {
            let reason = match e {
                LoginError::InvalidCredentials => "invalid_credentials",
                LoginError::Locked(_) => "locked",
                LoginError::Blocked(_) => "access_rule",
                LoginError::Internal(_) => "internal_error",
            };
            // The account the attempt named, when there is one. The username
            // itself is not recorded: an unknown one is often a password typed
            // into the wrong field.
            let user_id = find_login_account(state, &request.username)
                .await
                .ok()
                .flatten()
                .map(|u| u.id);
            SecurityEvent::failure(SecurityEventKind::LoginFailed)
                .maybe_user(user_id)
                .detail("reason", reason)
        }
    };
    let mut event = event.ip(client_ip).detail("method", "password");
    if !user_agent.is_empty() {
        event = event.user_agent(user_agent);
    }
    state.security_audit().emit(event).await;
}

/// The account a sign in names: the one with this name, else, when the
/// identifier looks like an e-mail address, the one with this address. An
/// account found by address is still held to the username access rules, as
/// if its name had been typed; one they deny is not found.
async fn find_login_account(state: &AppState, identifier: &str) -> anyhow::Result<Option<User>> {
    if let Some(user) = state.users().find_by_name(identifier).await? {
        return Ok(Some(user));
    }
    if !identifier.contains('@') {
        return Ok(None);
    }
    let Some(user) = state.users().find_by_mail(identifier.trim()).await? else {
        return Ok(None);
    };
    use crate::services::access_rule::{self, RuleType};
    if access_rule::is_blocked(state.db(), RuleType::Username, &user.name).await {
        return Ok(None);
    }
    Ok(Some(user))
}

/// [`do_login`] without the log entry.
async fn attempt_login(
    state: &AppState,
    session: &Session,
    request: &LoginRequest,
    client_ip: &str,
) -> Result<(), LoginError> {
    // Check if sign-in is locked, for this address or for the account
    match state
        .lockout()
        .is_locked(&request.username, client_ip)
        .await
    {
        Ok(true) => {
            let remaining = state
                .lockout()
                .get_lockout_remaining(&request.username, client_ip)
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

    // Access rules (B108): a denied username or client address does not
    // reach the password check at all. Checked after the lockout, so a
    // blocked client still spends its attempts.
    use crate::services::access_rule::{self, RuleType};
    if access_rule::is_blocked(state.db(), RuleType::Username, &request.username).await {
        return Err(LoginError::Blocked(
            "Sign in with this username is not allowed on this site.".to_string(),
        ));
    }
    if access_rule::is_blocked(state.db(), RuleType::Host, client_ip).await {
        return Err(LoginError::Blocked(
            "Sign in from your address is not allowed on this site.".to_string(),
        ));
    }

    // Find the account by its name, or by its e-mail address when what was
    // typed is one (B156: LoginToboggan's "log in with your e-mail address",
    // a function Drupal sites had). Both lookups fail the same way, through
    // the branch below, so the answer still never says which part was wrong.
    let user = match find_login_account(state, &request.username).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            // The same Argon2 work a real verification costs, so an unknown
            // username is not told apart by how long the answer takes.
            crate::models::User::verify_password_or_dummy(None, &request.password);
            if let Err(e) = state
                .lockout()
                .record_failed_attempt(&request.username, client_ip)
                .await
            {
                tracing::warn!(error = %e, "failed to record failed login attempt");
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
        // As for an unknown username: one Argon2 verification, result unused.
        crate::models::User::verify_password_or_dummy(None, &request.password);
        if let Err(e) = state
            .lockout()
            .record_failed_attempt(&request.username, client_ip)
            .await
        {
            tracing::warn!(error = %e, user_id = %user.id, "failed to record failed login attempt");
        }
        return Err(LoginError::InvalidCredentials);
    }

    // Verify password (one Argon2 verification, as on the two paths above;
    // an account with no password verifies against the dummy hash)
    if !crate::models::User::verify_password_or_dummy(Some(&user), &request.password) {
        match state
            .lockout()
            .record_failed_attempt(&request.username, client_ip)
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
                tracing::error!(error = %e, user_id = %user.id, "failed to record failed attempt");
            }
        }
        return Err(LoginError::InvalidCredentials);
    }

    // Successful login - clear any failed attempts
    if let Err(e) = state
        .lockout()
        .clear_attempts(&request.username, client_ip)
        .await
    {
        tracing::warn!(error = %e, user_id = %user.id, "failed to clear login attempts");
    }

    // Record login (updates timestamp + dispatches tap_user_login)
    if let Err(e) = state.users().record_login(&user).await {
        tracing::warn!(error = %e, user_id = %user.id, "failed to record login");
    }

    // Create session
    setup_session(session, user.id, request.remember_me).await?;

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
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: axum::http::HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Json<JsonSuccess>, AppError> {
    // The `login` bucket is counted once, per client IP, by the rate-limit
    // middleware (`categorize_path` puts this path in it). It used to be counted
    // here as well, which spent each attempt twice. The address is still the
    // lockout's key.
    let client_id = client_ip.0;

    match do_login(&state, &session, &request, &client_id, user_agent(&headers)).await {
        Ok(()) => Ok(Json(JsonSuccess {
            success: true,
            message: "Login successful".to_string(),
        })),
        Err(e) => Err(match e {
            LoginError::Locked(_) => AppError::RateLimited {
                retry_after_secs: 60,
                category: "login".into(),
            },
            LoginError::InvalidCredentials => {
                AppError::unauthorized("Invalid username or password")
            }
            LoginError::Blocked(msg) => AppError::forbidden(msg),
            LoginError::Internal(msg) => AppError::internal_ctx(anyhow::anyhow!(msg), "login"),
        }),
    }
}

/// Logout handler.
///
/// GET /user/logout — redirect to login page.
///
/// Logout must be POST (CSRF-protected). A GET here means someone typed
/// the URL directly or followed a stale link. Redirect rather than error.
async fn logout_redirect() -> Response {
    Redirect::to("/user/login").into_response()
}

/// POST /user/logout
/// - Validates CSRF token
/// - Records logout via UserService (dispatches tap_user_logout)
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

    // Extract user_id and the device id before `flush` clears them: the device
    // id is what the session index is keyed by.
    let user_id: Option<uuid::Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let device_id: Option<uuid::Uuid> = session
        .get(crate::services::session_registry::SESSION_DEVICE_ID)
        .await
        .ok()
        .flatten();
    // And the session's id, for `session.ended`: `flush` clears it too.
    let session_id = session.id().map(|id| id.to_string());

    // Record logout (dispatches tap_user_logout)
    if let Some(uid) = user_id {
        let _ = state.users().record_logout(uid).await;
    }

    // `flush`, not `delete`: `delete` removes the record from Redis and leaves
    // the in-memory session as it was: holding data, keeping its id, and
    // marked modified by the CSRF check above, which spent its token. The
    // session layer then saves it after this handler with `SET .. XX`, Redis
    // answers nil because the key is gone, the store reads that reply as a
    // bool and fails, and the layer answers 500. `flush` clears the data and
    // the id as well, so the layer has nothing to save and removes the cookie.
    if let Err(e) = session.flush().await {
        tracing::error!(error = %e, "failed to delete session");
        return crate::error::AppError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
        )
        .into_response();
    }

    // `flush` removed the session record but not the device's entry in the
    // user's session index, which would otherwise stay on `/user/sessions`
    // until the index expired. Dropping it also writes `session.ended`, now
    // that the sign-out has actually happened.
    if let Some(uid) = user_id {
        crate::services::session_registry::session_ended(
            &state,
            uid,
            device_id,
            session_id.as_deref(),
            "logout",
        )
        .await;
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

/// The public registration mode, read once per request: whether the routes are
/// open, and whether a new account waits for approval.
///
/// Reads [`RegistrationMode`](crate::models::RegistrationMode), which is the
/// setting the admin form controls. This
/// used to read the unrelated boolean `allow_user_registration`, so the form's
/// three-mode selector changed nothing and config import of the boolean was the
/// only way to open registration.
async fn registration_mode(state: &AppState) -> crate::models::RegistrationMode {
    crate::models::RegistrationMode::load(state.db()).await
}

/// The refusal of the public registration routes while only administrators
/// create accounts: `403`, as for any page this visitor may not use. It was
/// `404`, which told an old "Create new account" link or a crawler that the
/// page had never existed. A browser is shown the access-denied page, with the
/// login form for a visitor who already has an account.
fn registration_closed() -> AppError {
    AppError::forbidden("Only administrators may create new accounts on this site.")
}

/// Registration form handler.
///
/// GET /user/register
/// - Renders registration form with CSRF token
/// - Refuses with [`registration_closed`] while registration is closed
async fn register_form(State(state): State<AppState>, session: Session) -> Response {
    // Check if already logged in
    if super::helpers::session_user(&state, &session)
        .await
        .is_some()
    {
        return Redirect::to("/").into_response();
    }

    let mode = registration_mode(&state).await;
    if !mode.is_open() {
        return registration_closed().into_response();
    }

    let csrf_token = generate_csrf_token(&session).await;

    render_register_form(&state, &session, &csrf_token, None, None, None).await
}

/// Render the registration form with optional context.
async fn render_register_form(
    state: &AppState,
    session: &Session,
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
    super::helpers::inject_site_context(state, session, &mut context, "/user/register").await;

    match state.theme().tera().render("user/register.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render register form");
            super::helpers::render_server_error("The registration form could not be displayed.")
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
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Form(form): Form<RegisterFormRequest>,
) -> Response {
    let mode = registration_mode(&state).await;
    if !mode.is_open() {
        return registration_closed().into_response();
    }

    // The `register` bucket is counted once, per client IP, by the rate-limit
    // middleware (`categorize_path` puts this path in it). It used to be counted
    // here as well, which spent each attempt twice.

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
    validate_registration_input(
        &state,
        &username,
        &mail,
        &form.password,
        &client_ip.0,
        &mut errors,
    )
    .await;

    if form.password != form.confirm_password {
        errors.push("Passwords do not match.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        return render_register_form(
            &state,
            &session,
            &csrf_token,
            Some(&errors),
            None,
            Some(&values),
        )
        .await;
    }

    // Create inactive user
    match do_register(
        &state,
        &username,
        &mail,
        &form.password,
        mode.needs_approval(),
    )
    .await
    {
        Ok(result) => {
            let csrf_token = generate_csrf_token(&session).await;
            let message = registration_message(&result);
            render_register_form(&state, &session, &csrf_token, None, Some(&message), None).await
        }
        Err(e) => {
            tracing::error!(error = %e, "registration failed");
            let csrf_token = generate_csrf_token(&session).await;
            // Check for DB unique constraint violation (TOCTOU race)
            let msg = if is_unique_violation(&e) {
                "Username or email is already in use."
            } else {
                "An unexpected error occurred. Please try again later."
            };
            let errors = vec![msg.to_string()];
            render_register_form(
                &state,
                &session,
                &csrf_token,
                Some(&errors),
                None,
                Some(&values),
            )
            .await
        }
    }
}

/// Validate registration input fields.
async fn validate_registration_input(
    state: &AppState,
    username: &str,
    mail: &str,
    password: &str,
    client_ip: &str,
    errors: &mut Vec<String>,
) {
    let username = username.trim();
    let mail = mail.trim();

    // Access rules (B108). One message per blocked type; the uniqueness check
    // below still runs, so a blocked value is not told apart from a taken one
    // by what else the form says.
    for rule_type in crate::services::access_rule::blocked_types(
        state.db(),
        Some(username).filter(|u| !u.is_empty()),
        Some(mail).filter(|m| !m.is_empty()),
        Some(client_ip),
    )
    .await
    {
        errors.push(crate::services::access_rule::refusal_message(rule_type).to_string());
    }

    if let Err(msg) = validate_username(username) {
        errors.push(msg.to_string());
    }

    if mail.is_empty() {
        errors.push("Email address is required.".to_string());
    } else if !is_valid_email(mail) || mail.parse::<lettre::Address>().is_err() {
        // The second check is the mail library's own: an address it cannot
        // parse could never receive the verification mail queued with the
        // account, and that queue is in the account's transaction (BG18).
        errors.push("Please enter a valid email address.".to_string());
    }

    if let Err(msg) = validate_password(password) {
        errors.push(msg.to_string());
    }

    // Uniqueness. A taken name is said to be taken, by name: names are public
    // (every post shows its author's), so saying so tells nobody anything, and
    // a visitor who cannot tell which field to change cannot register (B156,
    // Drupal's "The name … is already taken."). A taken e-mail address is
    // still told apart from nothing in particular: only when the name is free
    // does the form say the name or address is in use, exactly as it did, so
    // no address is confirmed that was not before.
    if !username.is_empty()
        && let Ok(Some(_)) = state.users().find_by_name(username).await
    {
        errors.push(name_taken_message(username));
        return;
    }

    if !mail.is_empty()
        && is_valid_email(mail)
        && let Ok(Some(_)) = state.users().find_by_mail(mail).await
    {
        errors.push("Username or email is already in use.".to_string());
    }
}

/// What the registration form says when the name asked for belongs to an
/// account already.
fn name_taken_message(name: &str) -> String {
    format!("The name {name} is already taken.")
}

/// Result of a successful registration.
struct RegistrationResult {
    /// Whether a verification email was queued (mail is configured).
    email_sent: bool,
    /// Whether the account waits for an administrator (approval mode).
    pending_approval: bool,
}

/// The key in a user's `data` that marks an account registered under
/// [`RegistrationMode::Approval`](crate::models::RegistrationMode): confirming
/// its address does not activate it; an administrator does.
pub const PENDING_APPROVAL_KEY: &str = "pending_approval";

/// What the visitor is told once their registration is saved: what happened,
/// and what happens next.
fn registration_message(result: &RegistrationResult) -> String {
    match (result.pending_approval, result.email_sent) {
        (true, true) => "Thank you for applying for an account. Your account is currently \
                         pending approval by the site administrator. In the meantime, check \
                         your email for a link to confirm your address."
            .to_string(),
        (true, false) => "Thank you for applying for an account. Your account is currently \
                          pending approval by the site administrator."
            .to_string(),
        (false, true) => "Registration successful! Check your email for a verification link \
                          to activate your account."
            .to_string(),
        (false, false) => "Registration successful! However, we were unable to send the \
                           verification email. Please contact the site administrator to \
                           activate your account."
            .to_string(),
    }
}

/// Core registration logic shared by form and JSON handlers.
///
/// The account and its mail (the verification link, and the administrator's
/// notice when `notify_admin_on_register` is on) are written in one
/// transaction (BG18): a registration that fails sends nothing, and one that
/// succeeds always sends, whatever the mail server is doing at the time. The
/// verification token is minted by the mail worker when it sends, so it is
/// never stored in the clear.
async fn do_register(
    state: &AppState,
    username: &str,
    mail: &str,
    password: &str,
    needs_approval: bool,
) -> anyhow::Result<RegistrationResult> {
    let input = CreateUser {
        name: username.trim().to_string(),
        password: password.to_string(),
        mail: mail.trim().to_string(),
        is_admin: false,
    };

    // Create user with status=0 (inactive, pending email verification)
    // Registration is an anonymous action — no authenticated user context.
    let user_ctx = crate::tap::UserContext::anonymous();

    let Some(email_service) = state.email() else {
        let user = state
            .users()
            .create_with_status(input, 0, &user_ctx, "registration")
            .await?;
        tracing::warn!(
            user_id = %user.id,
            "SMTP not configured; verification email not sent"
        );
        let pending_approval =
            mark_if_pending_approval(state, user.id, needs_approval, &user_ctx).await?;
        info!(user_id = %user.id, "user registered (pending verification)");
        return Ok(RegistrationResult {
            email_sent: false,
            pending_approval,
        });
    };

    let site_name = SiteConfig::site_name(state.db()).await.unwrap_or_default();
    let notify_admin = SiteConfig::get(state.db(), "notify_admin_on_register")
        .await
        .ok()
        .flatten()
        .is_some_and(|v| v.as_bool().unwrap_or(false));
    let admin_mail = if notify_admin {
        SiteConfig::site_mail(state.db()).await.unwrap_or_default()
    } else {
        String::new()
    };
    let site_url = email_service.site_url().to_string();
    let user_name = username.trim().to_string();
    let user_mail = mail.trim().to_string();
    let tera = state.theme().tera();

    let user = state
        .users()
        .create_with_status_and_mail(
            input,
            0,
            &user_ctx,
            email_service,
            |user| {
                let verify_url = format!(
                    "{site_url}/user/verify/{}",
                    crate::services::email::SECRET_MARKER
                );
                let body = format!(
                    "Welcome to {site_name}!\n\n\
                 To activate your account, visit the following link:\n\
                 {verify_url}\n\n\
                 If you did not register for an account, you can safely ignore this email.\n\n\
                 This link will expire in 24 hours."
                );
                let mut messages = vec![
                    NewMail::account(
                        "registration_verify",
                        &user_mail,
                        &format!("Verify your account at {site_name}"),
                        &body,
                    )
                    .secret(MailSecret::Verification {
                        user_id: user.id,
                        purpose: PURPOSE_REGISTRATION.to_string(),
                    })
                    .dedupe(format!("register:{}", user.id)),
                ];
                if !admin_mail.is_empty() {
                    let action_url = format!("{site_url}/admin/users");
                    let subject = format!("New user registration at {site_name}: {user_name}");
                    let mut ctx = tera::Context::new();
                    ctx.insert("site_name", &site_name);
                    ctx.insert("username", &user_name);
                    ctx.insert("user_email", &user_mail);
                    ctx.insert("action_url", &action_url);
                    ctx.insert("subject", &subject);
                    match crate::services::email_templates::render(tera, "admin_new_user", &ctx) {
                        Ok((html, text)) => {
                            let notice = NewMail::notification(
                                "admin_new_user",
                                &admin_mail,
                                &subject,
                                &text,
                            )
                            .html(html)
                            .dedupe(format!("admin-new-user:{}", user.id));
                            // A notice to the site's own address that cannot be sent
                            // must not refuse the registration.
                            match notice.validate() {
                                Ok(()) => messages.push(notice),
                                Err(e) => tracing::warn!(
                                    error = %e,
                                    "admin registration notification cannot be sent"
                                ),
                            }
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            "admin registration notification: template render failed"
                        ),
                    }
                }
                messages
            },
            "registration",
        )
        .await?;

    let pending_approval =
        mark_if_pending_approval(state, user.id, needs_approval, &user_ctx).await?;
    info!(user_id = %user.id, "user registered (pending verification); verification email queued");
    Ok(RegistrationResult {
        email_sent: true,
        pending_approval,
    })
}

/// Under approval, mark a new account so confirming its address leaves it
/// blocked for an administrator to activate. Returns whether it was marked.
async fn mark_if_pending_approval(
    state: &AppState,
    user_id: uuid::Uuid,
    pending: bool,
    user_ctx: &crate::tap::UserContext,
) -> anyhow::Result<bool> {
    if pending {
        state
            .users()
            .update(
                user_id,
                crate::models::UpdateUser {
                    data: Some(serde_json::json!({ PENDING_APPROVAL_KEY: true })),
                    ..Default::default()
                },
                user_ctx,
            )
            .await?;
    }
    Ok(pending)
}

/// JSON registration handler.
///
/// POST /user/register/json — JSON registration endpoint for API clients.
///
/// Creates an inactive user and sends a verification email.
///
/// # CSRF
///
/// This endpoint does **not** require a CSRF token because it accepts
/// `application/json` bodies, which cannot be submitted by cross-origin
/// HTML forms (the browser's same-origin policy blocks `Content-Type:
/// application/json` from `<form>` elements). The `register` rate limit
/// (applied by the middleware) provides additional abuse protection.
async fn register_json(
    State(state): State<AppState>,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Json(request): Json<RegisterJsonRequest>,
) -> Result<Json<JsonSuccess>, AppError> {
    // The `register` bucket is counted once, per client IP, by the rate-limit
    // middleware (`categorize_path` puts this path in it). It used to be counted
    // here as well, which spent each attempt twice.

    let mode = registration_mode(&state).await;
    if !mode.is_open() {
        return Err(registration_closed());
    }

    // Validate
    let mut errors = Vec::new();
    validate_registration_input(
        &state,
        &request.username,
        &request.mail,
        &request.password,
        &client_ip.0,
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
        return Err(AppError::bad_request(errors.join(" ")));
    }

    match do_register(
        &state,
        &request.username,
        &request.mail,
        &request.password,
        mode.needs_approval(),
    )
    .await
    {
        Ok(result) => {
            let message = if result.pending_approval {
                registration_message(&result)
            } else if result.email_sent {
                "Registration successful. Check your email for a verification link.".to_string()
            } else {
                "Registration successful. However, the verification email could not be sent. \
                 Please contact the site administrator."
                    .to_string()
            };
            Ok(Json(JsonSuccess {
                success: true,
                message,
            }))
        }
        Err(e) => {
            if is_unique_violation(&e) {
                Err(AppError::conflict("Username or email is already in use."))
            } else {
                Err(AppError::internal_ctx(e, "registration"))
            }
        }
    }
}

/// Email verification handler.
///
/// GET /user/verify/{token}
/// - Validates the token, activates the user, redirects to login
async fn verify_email(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Path(token): Path<String>,
) -> Response {
    // Rate limit verification attempts to prevent token brute-force
    let client_id = client_ip.0;
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
                return crate::error::AppError::with_status(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error",
                )
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

    // An account registered under approval stays blocked: confirming the
    // address is the visitor's half, activating it is an administrator's.
    let pending_approval = state
        .users()
        .find_by_id(verification.user_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|u| u.data.get(PENDING_APPROVAL_KEY) == Some(&serde_json::json!(true)));
    if pending_approval {
        info!(user_id = %verification.user_id, "email verified, account awaits approval");
        let _ = session
            .insert(
                SESSION_LOGIN_NOTICE,
                "Your e-mail address is confirmed. Your account is pending approval by the \
                 site administrator; you can sign in once it has been activated.",
            )
            .await;
        return Redirect::to("/user/login").into_response();
    }

    // Activate the user (status=1)
    // Email verification is a system action — use anonymous context since
    // the user doesn't have a session yet.
    let user_ctx = crate::tap::UserContext::anonymous();
    if let Err(e) = state
        .users()
        .update(
            verification.user_id,
            crate::models::UpdateUser {
                status: Some(1),
                ..Default::default()
            },
            &user_ctx,
        )
        .await
    {
        tracing::error!(error = %e, user_id = %verification.user_id, "failed to activate user");
        return crate::error::AppError::with_status(
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
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Path(token): Path<String>,
) -> Response {
    // Rate limit verification attempts to prevent token brute-force
    let client_id = client_ip.0;
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
                return crate::error::AppError::with_status(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error",
                )
                .into_response();
            }
        };

    // Get the user and check for pending_email in data
    let Some(user) = state
        .users()
        .find_by_id(verification.user_id)
        .await
        .ok()
        .flatten()
    else {
        return crate::error::AppError::with_status(StatusCode::NOT_FOUND, "User not found")
            .into_response();
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
        // Clear stale data first — if this fails, the token is still valid for retry.
        clear_pending_email(&state, &user).await;
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
    if let Ok(Some(_)) = state.users().find_by_mail(&new_email).await {
        // Clear stale data first — if this fails, the token is still valid for retry.
        clear_pending_email(&state, &user).await;
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

    // Email change verification is a self-service action: the acting principal
    // is the owning user, carrying their real permissions (see
    // `user_context_for`). It is not an authorization gate — the token above is
    // — but it is the principal every `tap_user_update` listener sees, so it has
    // to be the truth about who acted.
    let user_ctx = super::helpers::user_context_for(&state, &user).await;
    if let Err(e) = state.users().update(user.id, update, &user_ctx).await {
        tracing::error!(error = %e, user_id = %user.id, "failed to update email");
        return crate::error::AppError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update email",
        )
        .into_response();
    }

    info!(user_id = %user.id, "email address updated via verification");

    Redirect::to("/user/profile").into_response()
}

/// Remove stale `pending_email` from a user's JSONB data after a failed
/// email-change verification (invalid address or uniqueness conflict).
///
/// No-ops (without a DB round-trip) if `pending_email` is not present.
async fn clear_pending_email(state: &AppState, user: &User) {
    let has_key = user
        .data
        .as_object()
        .is_some_and(|obj| obj.contains_key("pending_email"));
    if !has_key {
        return;
    }

    let mut data = user.data.clone();
    if let Some(obj) = data.as_object_mut() {
        obj.remove("pending_email");
    }
    let update = crate::models::UpdateUser {
        data: Some(data),
        ..Default::default()
    };
    let user_ctx = super::helpers::user_context_for(state, user).await;
    if let Err(e) = state.users().update(user.id, update, &user_ctx).await {
        tracing::warn!(error = %e, user_id = %user.id, "failed to clear pending_email");
    }
}

// ─── Profile & Password Management ──────────────────────────────────────────

/// Profile edit form request.
struct ProfileFormRequest {
    name: String,
    mail: String,
    timezone: String,
    current_password: String,
}

impl ProfileFormRequest {
    /// The profile fields out of a posted form.
    fn from_posted(posted: &crate::content::submission::PostedForm) -> Self {
        let get = |name: &str| posted.get(name).unwrap_or_default().to_string();
        Self {
            name: get("name"),
            mail: get("mail"),
            timezone: get("timezone"),
            current_password: get("current_password"),
        }
    }
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
    super::helpers::require_login(state, session).await
}

/// The profile form, `user_profile_form`, showing `values` (name, mail,
/// timezone). It goes through `FormService::build` like every other form, so
/// `tap_form_alter` reaches it.
fn build_profile_form(values: &serde_json::Value) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    let text = |key: &str| {
        values
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Form::new("user_profile_form")
        .action("/user/profile")
        .element(
            "name",
            FormElement::textfield()
                .title("Username")
                .required()
                .default_value(text("name"))
                .weight(0),
        )
        .element(
            "mail",
            FormElement::input("email")
                .title("Email address")
                .required()
                .default_value(text("mail"))
                .weight(1),
        )
        .element(
            "timezone",
            FormElement::textfield()
                .title("Timezone")
                .placeholder("e.g., America/New_York")
                .default_value(text("timezone"))
                .weight(2),
        )
        .element(
            "current_password",
            FormElement::password()
                .title("Current password")
                .placeholder("Required to change username or email")
                .description("Required when changing your username or email address.")
                .weight(3),
        )
        .element("op", FormElement::submit("Save profile").weight(100))
}

/// Render the profile page with context.
///
/// When `values` is provided, those values override the user's stored values
/// (used to preserve submitted form data on validation errors).
///
/// The profile form is built through the forms pipeline and carries its own
/// token; the password form gets `password_csrf`, since tokens are single-use
/// (consumed on verification).
// Each optional param (errors/success/error/values) represents a distinct render
// state; bundling them into a struct would obscure call-site intent.
#[allow(clippy::too_many_arguments)]
async fn render_profile(
    state: &AppState,
    session: &Session,
    password_csrf: &str,
    user: &User,
    errors: Option<&[String]>,
    success: Option<&str>,
    error: Option<&str>,
    values: Option<&serde_json::Value>,
) -> Response {
    let stored = serde_json::json!({
        "name": user.name,
        "mail": user.mail,
        "timezone": user.timezone,
    });
    let user_ctx = super::helpers::user_context_for(state, user).await;
    let mut form_state = crate::form::FormState::new("user_profile_form", "");
    form_state.user_id = Some(user.id);
    let form = match state
        .forms()
        .build(
            build_profile_form(values.unwrap_or(&stored)),
            session,
            &crate::tap::RequestState::new(user_ctx, state.tap_services().clone()),
            form_state,
        )
        .await
        .and_then(|form| state.theme().render_form(&form))
    {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the profile form");
            return super::helpers::render_server_error("The profile page could not be displayed.");
        }
    };

    let mut context = tera::Context::new();
    context.insert("profile_form", &form);
    context.insert("password_csrf_token", password_csrf);
    if let Some(errors) = errors {
        context.insert("errors", errors);
    }
    if let Some(success) = success {
        context.insert("success", success);
    }
    if let Some(error) = error {
        context.insert("error", error);
    }
    // The plugins this member opted out of mailing them, each with a
    // Resubscribe button. Nothing is shown when there are none.
    match crate::models::user_mail_preference::opted_out_plugins(state.db(), user.id).await {
        Ok(plugins) if !plugins.is_empty() => {
            let opt_outs: Vec<serde_json::Value> = plugins
                .iter()
                .map(|plugin| {
                    serde_json::json!({
                        "plugin": plugin,
                        "label": super::mail_preference::plugin_label(state, plugin),
                    })
                })
                .collect();
            context.insert("mail_opt_outs", &opt_outs);
            context.insert("mail_csrf_token", &generate_csrf_token(session).await);
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "failed to list mail opt outs"),
    }
    super::helpers::inject_site_context(state, session, &mut context, "/user/profile").await;

    match state.theme().tera().render("user/profile.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render profile form");
            super::helpers::render_server_error("The profile page could not be displayed.")
        }
    }
}

/// Profile view/edit form handler.
///
/// GET /user/profile
async fn profile_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match get_current_user(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let password_csrf = generate_csrf_token(&session).await;
    render_profile(
        &state,
        &session,
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
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = crate::content::submission::PostedForm(pairs);
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

    // CSRF, the form state, and `tap_form_validate`, through the pipeline.
    let tap_state = crate::tap::RequestState::new(
        super::helpers::user_context_for(&state, &user).await,
        state.tap_services().clone(),
    );
    let (form_state, mut errors) = match state
        .forms()
        .process(
            posted.get("_form_build_id").unwrap_or_default(),
            &posted,
            &session,
            &tap_state,
            |_| Ok(()),
        )
        .await
    {
        Ok((form_state, ())) => (Some(form_state), Vec::new()),
        Err(crate::form::ProcessError::Csrf) => {
            return super::helpers::render_error(
                "Invalid or expired form token. Please try again.",
            );
        }
        Err(crate::form::ProcessError::Invalid(errors)) => (
            None,
            errors.into_iter().map(|e| e.message).collect::<Vec<_>>(),
        ),
    };
    let form = ProfileFormRequest::from_posted(&posted);

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

    if let Err(msg) = validate_username(name) {
        errors.push(msg.to_string());
    }

    if mail.is_empty() {
        errors.push("Email address is required.".to_string());
    } else if !is_valid_email(mail) || mail.parse::<lettre::Address>().is_err() {
        // The mail library's own parse too: the confirmation is queued to this
        // address, and one it cannot parse would never be sent (BG18).
        errors.push("Please enter a valid email address.".to_string());
    }

    if !timezone.is_empty() && !is_valid_timezone(timezone) {
        errors.push("Please enter a valid timezone (e.g., America/New_York).".to_string());
    }

    // Require current password when changing username or email (login credentials)
    if email_changing || name_changing {
        // Check lockout before attempting password verification
        if let Ok(true) = state.lockout().is_locked(&user.name, &client_ip.0).await {
            errors.push("Account temporarily locked due to too many failed attempts.".to_string());
        } else if form.current_password.is_empty() {
            errors
                .push("Current password is required to change your username or email.".to_string());
        } else if !user.verify_password(&form.current_password) {
            // Record failed attempt for lockout tracking
            if let Err(e) = state
                .lockout()
                .record_failed_attempt(&user.name, &client_ip.0)
                .await
            {
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
        && let Ok(Some(existing)) = state.users().find_by_name(name).await
        && existing.id != user.id
    {
        uniqueness_conflict = true;
    }

    if email_changing
        && !mail.is_empty()
        && let Ok(Some(existing)) = state.users().find_by_mail(mail).await
        && existing.id != user.id
    {
        uniqueness_conflict = true;
    }

    if uniqueness_conflict {
        errors.push("Username or email is already in use.".to_string());
    }

    if !errors.is_empty() {
        let pwc = generate_csrf_token(&session).await;
        return render_profile(
            &state,
            &session,
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

    // Profile update is a self-service action; the acting principal is the
    // owner, with their real permissions.
    let user_ctx = super::helpers::user_context_for(&state, &user).await;
    match state.users().update(user.id, update, &user_ctx).await {
        Ok(Some(updated_user)) => {
            if let Some(form_state) = &form_state {
                state
                    .forms()
                    .submitted(form_state, &posted, None, &tap_state)
                    .await;
            }

            // Cycle session ID after credential changes to prevent fixation
            if name_changing && let Err(e) = session.cycle_id().await {
                tracing::warn!(error = %e, user_id = %user.id, "failed to cycle session ID after username change");
            }

            // Send verification email if email is changing
            if email_changing {
                // Invalidate any outstanding tokens before creating a new one
                if let Err(e) =
                    EmailVerificationToken::invalidate_user_tokens(state.db(), user.id).await
                {
                    tracing::warn!(error = %e, "failed to invalidate old verification tokens");
                }

                // Queue the confirmation; the worker mints the token when it
                // sends, so it is never stored in the clear (BG18).
                if let Some(email_service) = state.email() {
                    let site_name = SiteConfig::site_name(state.db()).await.unwrap_or_default();
                    let verify_url = format!(
                        "{}/user/verify-email/{}",
                        email_service.site_url(),
                        crate::services::email::SECRET_MARKER
                    );
                    let body = format!(
                        "You requested to change your email address at {site_name}.\n\n\
                         To confirm this change, visit the following link:\n\
                         {verify_url}\n\n\
                         If you did not request this change, you can safely ignore this email.\n\n\
                         This link will expire in 24 hours."
                    );
                    let message = NewMail::account(
                        "email_change_verify",
                        mail,
                        &format!("Confirm email change at {site_name}"),
                        &body,
                    )
                    .secret(MailSecret::Verification {
                        user_id: user.id,
                        purpose: PURPOSE_EMAIL_CHANGE.to_string(),
                    });
                    // No dedupe key: each request invalidates the links sent
                    // before it, so each must send a fresh one.
                    if let Err(e) = email_service.queue_now(message).await {
                        tracing::error!(error = %e, "failed to queue email change verification");
                    }
                }
            }

            let pwc = generate_csrf_token(&session).await;
            let success_msg = if email_changing {
                "Profile updated. A verification email has been sent to your new address. \
                 Your email will be updated after you confirm the change."
            } else {
                "Profile updated successfully."
            };
            render_profile(
                &state,
                &session,
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
            let pwc = generate_csrf_token(&session).await;
            render_profile(
                &state,
                &session,
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
            let pwc = generate_csrf_token(&session).await;
            render_profile(
                &state,
                &session,
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
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
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
    if let Ok(true) = state.lockout().is_locked(&user.name, &client_ip.0).await {
        errors.push("Account temporarily locked due to too many failed attempts.".to_string());
    } else if form.current_password.is_empty() {
        errors.push("Current password is required.".to_string());
    } else if !user.verify_password(&form.current_password) {
        // Record failed attempt for lockout tracking
        if let Err(e) = state
            .lockout()
            .record_failed_attempt(&user.name, &client_ip.0)
            .await
        {
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
        let pwc = generate_csrf_token(&session).await;
        return render_profile(
            &state,
            &session,
            &pwc,
            &user,
            Some(&errors),
            None,
            None,
            None,
        )
        .await;
    }

    // Password change is a self-service action; the acting principal is the
    // owner, with their real permissions.
    let user_ctx = super::helpers::user_context_for(&state, &user).await;
    match state
        .users()
        .update_password(user.id, &form.new_password, &user_ctx, "self_service")
        .await
    {
        Ok(true) => {
            // A changed password ends the account's sessions everywhere else:
            // whoever else held one held it under the old password. The
            // caller's own device is kept, so the change does not sign them
            // out of the page they made it from.
            let own_device = session
                .get::<uuid::Uuid>(crate::services::session_registry::SESSION_DEVICE_ID)
                .await
                .ok()
                .flatten();
            crate::services::session_registry::end_user_sessions(
                &state,
                user.id,
                own_device,
                None,
                "password_changed",
            )
            .await;

            // Rotate session ID so a stolen pre-change session is invalid
            if let Err(e) = session.cycle_id().await {
                tracing::warn!(error = %e, "failed to cycle session after password change");
            }

            info!(user_id = %user.id, "password changed via self-service");

            // The `password.set` / `password.changed` security event is
            // `UserService::update_password`'s, as it is for every other path
            // that sets a password.

            let pwc = generate_csrf_token(&session).await;
            render_profile(
                &state,
                &session,
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
            let pwc = generate_csrf_token(&session).await;
            render_profile(
                &state,
                &session,
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
        .merge(super::user_profile::router())
        .route("/user/login", get(login_form).post(login_form_submit))
        .route("/user/login/json", post(login))
        .route("/user/logout", get(logout_redirect).post(logout))
        .route(
            "/user/register",
            get(register_form).post(register_form_submit),
        )
        .route("/user/register/json", post(register_json))
        .route("/user/verify/{token}", get(verify_email))
        .route("/user/verify-email/{token}", get(verify_email_change))
        .route("/user/profile", get(profile_form).post(profile_update))
        .route("/user/password", post(password_change))
        .route(
            "/user/data-export",
            get(super::user_data_export::export_my_data),
        )
        .merge(super::mail_preference::router())
}
