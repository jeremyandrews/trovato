//! Installer routes for first-time setup.
//!
//! Provides a multi-step installation wizard:
//! 1. Welcome / requirements check
//! 2. Admin account creation
//! 3. Site configuration
//! 4. Installation complete

use axum::{
    Form, Router,
    extract::State,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::models::{CreateUser, SiteConfig};
use crate::routes::helpers::{
    html_escape, is_valid_email, require_csrf, validate_password, validate_username,
};
use crate::state::AppState;
use crate::tap::UserContext;

// =============================================================================
// Request Types
// =============================================================================

/// Admin account creation form.
#[derive(Debug, Deserialize)]
pub struct CreateAdminForm {
    pub username: String,
    pub email: String,
    pub password: String,
    pub password_confirm: String,
    /// CSRF token.
    #[serde(rename = "_token", default)]
    pub token: String,
}

/// Site configuration form.
#[derive(Debug, Deserialize)]
pub struct SiteConfigForm {
    pub site_name: String,
    pub site_slogan: Option<String>,
    pub site_mail: Option<String>,
    /// CSRF token.
    #[serde(rename = "_token", default)]
    pub token: String,
    /// The administrator's name, asked for only when this session did not
    /// create the account.
    #[serde(default)]
    pub admin_name: Option<String>,
    /// That administrator's password.
    #[serde(default)]
    pub admin_password: Option<String>,
}

/// Session key holding the id of the administrator this session created.
///
/// The site step finishes the installation, so it belongs to whoever created
/// the administrator: the session holding this key, or one that signs in as an
/// administrator on the site form. Without it, anyone who reached the site
/// between the two steps could name the site and close the installer.
const SESSION_INSTALL_ADMIN: &str = "install_admin_id";

// =============================================================================
// Helpers
// =============================================================================

/// Check if the site is already installed.
async fn check_installed(state: &AppState) -> bool {
    SiteConfig::is_installed(state.db()).await.unwrap_or(false)
}

/// Count admin users (excluding anonymous).
async fn count_admin_users(state: &AppState) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM users WHERE is_admin = true AND id != '00000000-0000-0000-0000-000000000000'"
    )
    .fetch_one(state.db())
    .await
    .unwrap_or(0)
}

/// Render installer template.
fn render_installer(
    title: &str,
    step: u8,
    total_steps: u8,
    content: &str,
    error: Option<&str>,
) -> Response {
    let error_html = error
        .map(|e| {
            format!(
                r#"<div class="alert alert--error">{}</div>"#,
                html_escape(e)
            )
        })
        .unwrap_or_default();

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{title} - Trovato Installation</title>
    <!-- The installer renders its own shell rather than extending base.html, so
         the favicon links are repeated here. Keep them in step with
         templates/base.html. -->
    <link rel="icon" type="image/svg+xml" href="/static/brand/trovato-favicon.svg">
    <link rel="icon" type="image/png" sizes="16x16" href="/static/brand/favicon-16.png">
    <link rel="icon" type="image/png" sizes="32x32" href="/static/brand/favicon-32.png">
    <link rel="icon" type="image/png" sizes="64x64" href="/static/brand/favicon-64.png">
    <link rel="apple-touch-icon" sizes="180x180" href="/static/brand/trovato-appicon-180.png">
    <style>
        :root {{
            --primary: #2563eb;
            --primary-dark: #1d4ed8;
            --gray-50: #f9fafb;
            --gray-100: #f3f4f6;
            --gray-200: #e5e7eb;
            --gray-300: #d1d5db;
            --gray-500: #6b7280;
            --gray-700: #374151;
            --gray-900: #111827;
            --red-500: #ef4444;
            --red-100: #fee2e2;
            --green-500: #22c55e;
            --green-100: #dcfce7;
        }}
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: 2rem;
        }}
        .installer {{
            background: white;
            border-radius: 1rem;
            box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 0.25);
            width: 100%;
            max-width: 500px;
            overflow: hidden;
        }}
        .installer-header {{
            background: var(--gray-900);
            color: white;
            padding: 1.5rem 2rem;
            text-align: center;
        }}
        .installer-header h1 {{
            font-size: 1.5rem;
            margin-bottom: 0.5rem;
        }}
        .installer-header .step {{
            font-size: 0.875rem;
            color: var(--gray-300);
        }}
        .installer-content {{
            padding: 2rem;
        }}
        .form-group {{
            margin-bottom: 1.5rem;
        }}
        .form-group label {{
            display: block;
            font-weight: 500;
            margin-bottom: 0.5rem;
            color: var(--gray-700);
        }}
        .form-group input {{
            width: 100%;
            padding: 0.75rem 1rem;
            border: 1px solid var(--gray-300);
            border-radius: 0.5rem;
            font-size: 1rem;
            transition: border-color 0.15s, box-shadow 0.15s;
        }}
        .form-group input:focus {{
            outline: none;
            border-color: var(--primary);
            box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.1);
        }}
        .form-group .help {{
            font-size: 0.875rem;
            color: var(--gray-500);
            margin-top: 0.25rem;
        }}
        .button {{
            display: inline-block;
            padding: 0.75rem 1.5rem;
            background: var(--primary);
            color: white;
            border: none;
            border-radius: 0.5rem;
            font-size: 1rem;
            font-weight: 500;
            cursor: pointer;
            transition: background-color 0.15s;
            text-decoration: none;
        }}
        .button:hover {{
            background: var(--primary-dark);
        }}
        .button--full {{
            width: 100%;
        }}
        .alert {{
            padding: 1rem;
            border-radius: 0.5rem;
            margin-bottom: 1.5rem;
        }}
        .alert--error {{
            background: var(--red-100);
            color: var(--red-500);
        }}
        .alert--success {{
            background: var(--green-100);
            color: var(--green-500);
        }}
        .requirements {{
            list-style: none;
            margin-bottom: 1.5rem;
        }}
        .requirements li {{
            padding: 0.5rem 0;
            display: flex;
            align-items: center;
            gap: 0.5rem;
        }}
        .requirements .check {{
            color: var(--green-500);
            font-weight: bold;
        }}
        .success-icon {{
            font-size: 4rem;
            text-align: center;
            margin-bottom: 1rem;
        }}
    </style>
</head>
<body>
    <div class="installer">
        <div class="installer-header">
            <h1>{title}</h1>
            <div class="step">Step {step} of {total_steps}</div>
        </div>
        <div class="installer-content">
            {error_html}
            {content}
        </div>
    </div>
</body>
</html>"#,
        title = html_escape(title),
        step = step,
        total_steps = total_steps,
        error_html = error_html,
        content = content,
    );

    Html(html).into_response()
}

// =============================================================================
// Routes
// =============================================================================

/// Installation entry point - redirects to appropriate step.
///
/// GET /install
async fn install_index(State(state): State<AppState>) -> Response {
    // If already installed, redirect to home
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    // Check if we have admin users
    let admin_count = count_admin_users(&state).await;

    if admin_count == 0 {
        // No admin - show welcome page first
        Redirect::to("/install/welcome").into_response()
    } else {
        // Have admin but not marked installed - show site config
        Redirect::to("/install/site").into_response()
    }
}

/// Welcome and requirements check.
///
/// GET /install/welcome
async fn install_welcome(State(state): State<AppState>) -> Response {
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    let content = r#"
        <p style="margin-bottom: 1.5rem; color: var(--gray-700);">
            Welcome to Trovato CMS! Let's get your site set up.
        </p>
        <h3 style="margin-bottom: 1rem; font-size: 1rem;">Requirements Check</h3>
        <ul class="requirements">
            <li><span class="check">✓</span> PostgreSQL connected</li>
            <li><span class="check">✓</span> Redis connected</li>
            <li><span class="check">✓</span> Database migrations applied</li>
        </ul>
        <a href="/install/admin" class="button button--full">Continue</a>
    "#;

    render_installer("Welcome", 1, 4, content, None)
}

/// The administrator form, carrying the session's CSRF token.
fn admin_form_html(token: &str) -> String {
    format!(
        r#"
        <p style="margin-bottom: 1.5rem; color: var(--gray-700);">
            Create the administrator account for your site.
        </p>
        <form method="post" action="/install/admin">
            <input type="hidden" name="_token" value="{token}">
            <div class="form-group">
                <label for="username">Username</label>
                <input type="text" id="username" name="username" required autocomplete="username">
            </div>
            <div class="form-group">
                <label for="email">Email Address</label>
                <input type="email" id="email" name="email" required autocomplete="email">
            </div>
            <div class="form-group">
                <label for="password">Password</label>
                <input type="password" id="password" name="password" required minlength="12" autocomplete="new-password">
                <div class="help">Minimum 12 characters</div>
            </div>
            <div class="form-group">
                <label for="password_confirm">Confirm Password</label>
                <input type="password" id="password_confirm" name="password_confirm" required autocomplete="new-password">
            </div>
            <button type="submit" class="button button--full">Create Admin Account</button>
        </form>
    "#,
        token = html_escape(token),
    )
}

/// Render the administrator form, with an error when there is one.
async fn render_admin_form(session: &Session, error: Option<&str>) -> Response {
    let token = crate::form::csrf::generate_csrf_token(session).await;
    render_installer(
        "Create Admin Account",
        2,
        4,
        &admin_form_html(&token),
        error,
    )
}

/// Admin account creation form.
///
/// GET /install/admin
async fn install_admin_form(State(state): State<AppState>, session: Session) -> Response {
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    // Check if admin already exists
    if count_admin_users(&state).await > 0 {
        return Redirect::to("/install/site").into_response();
    }

    render_admin_form(&session, None).await
}

/// Admin account creation submit.
///
/// POST /install/admin
async fn install_admin_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CreateAdminForm>,
) -> Response {
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    if let Err(refusal) = require_csrf(&session, &form.token).await {
        return refusal;
    }

    // The wizard creates one administrator; once there is one, the next step
    // is the site form, not a second account.
    if count_admin_users(&state).await > 0 {
        return Redirect::to("/install/site").into_response();
    }

    // Validate
    if let Err(msg) = validate_username(form.username.trim()) {
        return render_admin_form(&session, Some(msg)).await;
    }

    if form.email.trim().is_empty() || !is_valid_email(form.email.trim()) {
        return render_admin_form(&session, Some("Valid email is required")).await;
    }

    if let Err(msg) = validate_password(&form.password) {
        return render_admin_form(&session, Some(msg)).await;
    }

    if form.password != form.password_confirm {
        return render_admin_form(&session, Some("Passwords do not match")).await;
    }

    // Check if username already exists
    if let Ok(Some(_)) = state.users().find_by_name(&form.username).await {
        return render_admin_form(&session, Some("Username already exists")).await;
    }

    // Check if email already exists
    if let Ok(Some(_)) = state.users().find_by_mail(&form.email).await {
        return render_admin_form(&session, Some("Email already in use")).await;
    }

    // Create admin user (anonymous context — no user exists yet)
    let input = CreateUser {
        name: form.username.trim().to_string(),
        password: form.password,
        mail: form.email.trim().to_string(),
        is_admin: true,
    };

    let anon = UserContext::anonymous();
    match state.users().create(input, &anon, "installer").await {
        Ok(admin) => {
            if let Err(e) = session.insert(SESSION_INSTALL_ADMIN, admin.id).await {
                tracing::error!(error = %e, "failed to remember the installing administrator");
            }
            Redirect::to("/install/site").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create admin user");
            render_admin_form(&session, Some("Failed to create admin account")).await
        }
    }
}

/// The administrator this session created, while they still are one.
async fn installing_admin(state: &AppState, session: &Session) -> Option<Uuid> {
    let id: Uuid = session.get(SESSION_INSTALL_ADMIN).await.ok().flatten()?;
    match state.users().find_by_id(id).await {
        Ok(Some(user)) if user.is_admin => Some(id),
        _ => None,
    }
}

/// The site form, carrying the session's CSRF token. A session that did not
/// create the administrator is also asked to sign in as one.
fn site_form_html(token: &str, ask_admin: bool) -> String {
    let admin_fields = if ask_admin {
        r#"
            <p style="margin-bottom: 1.5rem; color: var(--gray-700);">
                The administrator account was created in another browser session.
                Sign in as that administrator to finish.
            </p>
            <div class="form-group">
                <label for="admin_name">Administrator username</label>
                <input type="text" id="admin_name" name="admin_name" required autocomplete="username">
            </div>
            <div class="form-group">
                <label for="admin_password">Administrator password</label>
                <input type="password" id="admin_password" name="admin_password" required autocomplete="current-password">
            </div>"#
    } else {
        ""
    };
    format!(
        r#"
        <p style="margin-bottom: 1.5rem; color: var(--gray-700);">
            Configure your site's basic information.
        </p>
        <form method="post" action="/install/site">
            <input type="hidden" name="_token" value="{token}">{admin_fields}
            <div class="form-group">
                <label for="site_name">Site Name</label>
                <input type="text" id="site_name" name="site_name" value="Trovato" required>
            </div>
            <div class="form-group">
                <label for="site_slogan">Site Slogan</label>
                <input type="text" id="site_slogan" name="site_slogan" placeholder="Optional tagline">
            </div>
            <div class="form-group">
                <label for="site_mail">Site Email</label>
                <input type="email" id="site_mail" name="site_mail" placeholder="admin@example.com">
                <div class="help">Used for system notifications</div>
            </div>
            <button type="submit" class="button button--full">Save and Continue</button>
        </form>
    "#,
        token = html_escape(token),
    )
}

/// Render the site form, with an error when there is one.
async fn render_site_form(state: &AppState, session: &Session, error: Option<&str>) -> Response {
    let ask_admin = installing_admin(state, session).await.is_none();
    let token = crate::form::csrf::generate_csrf_token(session).await;
    render_installer(
        "Site Configuration",
        3,
        4,
        &site_form_html(&token, ask_admin),
        error,
    )
}

/// Site configuration form.
///
/// GET /install/site
async fn install_site_form(State(state): State<AppState>, session: Session) -> Response {
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    // Require admin user
    if count_admin_users(&state).await == 0 {
        return Redirect::to("/install/admin").into_response();
    }

    render_site_form(&state, &session, None).await
}

/// Site configuration submit.
///
/// POST /install/site
async fn install_site_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<SiteConfigForm>,
) -> Response {
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    if let Err(refusal) = require_csrf(&session, &form.token).await {
        return refusal;
    }

    // The step belongs to the administrator: the session that created them,
    // or one that signs in as an administrator on this form.
    if installing_admin(&state, &session).await.is_none() {
        let name = form.admin_name.as_deref().unwrap_or("").trim();
        let password = form.admin_password.as_deref().unwrap_or("");
        let admin = match state.users().find_by_name(name).await {
            Ok(Some(user)) if user.is_admin => Some(user),
            _ => None,
        };
        // Verified against a dummy hash when there is no such administrator,
        // so the answer takes as long either way.
        if !crate::models::User::verify_password_or_dummy(admin.as_ref(), password) {
            return render_site_form(
                &state,
                &session,
                Some("Sign in as the site's administrator to finish the installation"),
            )
            .await;
        }
        if let Some(admin) = admin
            && let Err(e) = session.insert(SESSION_INSTALL_ADMIN, admin.id).await
        {
            tracing::error!(error = %e, "failed to remember the installing administrator");
        }
    }

    // Validate
    if form.site_name.trim().is_empty() {
        return render_site_form(&state, &session, Some("Site name is required")).await;
    }

    // Save configuration
    if let Err(e) = SiteConfig::set_site_name(state.db(), form.site_name.trim()).await {
        tracing::error!(error = %e, "failed to set site name");
        return render_site_form(&state, &session, Some("Failed to save configuration")).await;
    }

    if let Some(ref slogan) = form.site_slogan
        && let Err(e) = SiteConfig::set_site_slogan(state.db(), slogan.trim()).await
    {
        tracing::error!(error = %e, "failed to set site slogan");
    }

    if let Some(ref mail) = form.site_mail
        && let Err(e) = SiteConfig::set_site_mail(state.db(), mail.trim()).await
    {
        tracing::error!(error = %e, "failed to set site mail");
    }

    // Mark as installed
    if let Err(e) = SiteConfig::mark_installed(state.db()).await {
        tracing::error!(error = %e, "failed to mark site as installed");
        return render_site_form(&state, &session, Some("Failed to complete installation")).await;
    }

    Redirect::to("/install/complete").into_response()
}

/// Installation complete page.
///
/// GET /install/complete
async fn install_complete(State(state): State<AppState>) -> Response {
    // This page is accessible even after installation to show success
    let site_name = SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());

    let content = format!(
        r#"
        <div class="success-icon">🎉</div>
        <h2 style="text-align: center; margin-bottom: 1rem;">Congratulations!</h2>
        <p style="text-align: center; color: var(--gray-700); margin-bottom: 1.5rem;">
            <strong>{}</strong> has been successfully installed.
        </p>
        <div style="display: flex; gap: 1rem;">
            <a href="/" class="button" style="flex: 1; text-align: center;">View Site</a>
            <a href="/admin" class="button" style="flex: 1; text-align: center; background: var(--gray-700);">Admin Dashboard</a>
        </div>
    "#,
        html_escape(&site_name)
    );

    render_installer("Installation Complete", 4, 4, &content, None)
}

// =============================================================================
// Router
// =============================================================================

/// Create the installer router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/install", get(install_index))
        .route("/install/welcome", get(install_welcome))
        .route("/install/admin", get(install_admin_form))
        .route("/install/admin", post(install_admin_submit))
        .route("/install/site", get(install_site_form))
        .route("/install/site", post(install_site_submit))
        .route("/install/complete", get(install_complete))
}
