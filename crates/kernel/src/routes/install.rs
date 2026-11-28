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
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::submission::PostedForm;
use crate::form::ValidationError;
use crate::models::{CreateUser, SiteConfig};
use crate::routes::helpers::{html_escape, is_valid_email, validate_password, validate_username};
use crate::state::AppState;
use crate::tap::UserContext;

// =============================================================================
// Forms
// =============================================================================

/// The id of the administrator account form (`/install/admin`).
const ADMIN_FORM_ID: &str = "install_admin_form";

/// The id of the site configuration form (`/install/site`).
const SITE_FORM_ID: &str = "install_site_form";

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
fn render_installer(title: &str, step: u8, total_steps: u8, content: &str) -> Response {
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
        /* The forms are drawn by the form pipeline (templates/form). */
        .form-item {{
            margin-bottom: 1.5rem;
        }}
        .form-item__label {{
            display: block;
            font-weight: 500;
            margin-bottom: 0.5rem;
            color: var(--gray-700);
        }}
        .form-text {{
            width: 100%;
            padding: 0.75rem 1rem;
            border: 1px solid var(--gray-300);
            border-radius: 0.5rem;
            font-size: 1rem;
            transition: border-color 0.15s, box-shadow 0.15s;
        }}
        .form-text:focus {{
            outline: none;
            border-color: var(--primary);
            box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.1);
        }}
        .form-text[aria-invalid="true"] {{
            border-color: var(--red-500);
        }}
        .form-item__description {{
            font-size: 0.875rem;
            color: var(--gray-500);
            margin-top: 0.25rem;
        }}
        .form-item__error {{
            font-size: 0.875rem;
            color: var(--red-500);
            margin-top: 0.25rem;
        }}
        .form__errors {{
            padding: 1rem;
            border-radius: 0.5rem;
            margin-bottom: 1.5rem;
            background: var(--red-100);
            color: var(--red-500);
            list-style: none;
        }}
        .form__errors ul {{
            list-style: none;
        }}
        .form-submit {{
            width: 100%;
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
            {content}
        </div>
    </div>
</body>
</html>"#,
        title = html_escape(title),
        step = step,
        total_steps = total_steps,
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

    render_installer("Welcome", 1, 4, content)
}

/// Build `form` through the form pipeline for nobody signed in (no account
/// exists yet, or the one that does is not this browser's) and draw it on an
/// installer page under `intro`, with `errors` beside their fields.
///
/// The form service serves the installer: migrations run before the server
/// answers a request, so the table its saved state lives in is there.
async fn render_form_page(
    state: &AppState,
    session: &Session,
    (title, step): (&str, u8),
    intro: &str,
    form: crate::form::Form,
    errors: &[ValidationError],
) -> Response {
    let viewer = UserContext::anonymous();
    match super::admin_form::render_as(state, session, &viewer, form, errors).await {
        Ok(form) => render_installer(
            title,
            step,
            4,
            &format!(
                r#"
        <p style="margin-bottom: 1.5rem; color: var(--gray-700);">
            {}
        </p>
        {form}"#,
                html_escape(intro)
            ),
        ),
        Err(refusal) => refusal,
    }
}

/// The administrator account form.
fn admin_form() -> crate::form::Form {
    use crate::form::{Form, FormElement};
    Form::new(ADMIN_FORM_ID)
        .action("/install/admin")
        .element(
            "username",
            FormElement::textfield()
                .title("Username")
                .required()
                .attribute("autocomplete", "username")
                .weight(0),
        )
        .element(
            "email",
            FormElement::input("email")
                .title("Email Address")
                .required()
                .attribute("autocomplete", "email")
                .weight(1),
        )
        .element(
            "password",
            FormElement::password()
                .title("Password")
                .description("Minimum 12 characters")
                .required()
                .attribute("minlength", "12")
                .attribute("autocomplete", "new-password")
                .weight(2),
        )
        .element(
            "password_confirm",
            FormElement::password()
                .title("Confirm Password")
                .required()
                .attribute("autocomplete", "new-password")
                .weight(3),
        )
        .element("op", FormElement::submit("Create Admin Account").weight(10))
}

/// Render the administrator form, with `errors` when there are some.
async fn render_admin_form(
    state: &AppState,
    session: &Session,
    errors: &[ValidationError],
) -> Response {
    render_form_page(
        state,
        session,
        ("Create Admin Account", 2),
        "Create the administrator account for your site.",
        admin_form(),
        errors,
    )
    .await
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

    render_admin_form(&state, &session, &[]).await
}

/// What is wrong with a submitted administrator form, field by field.
fn admin_form_errors(posted: &PostedForm) -> Vec<ValidationError> {
    let username = posted.get("username").unwrap_or_default().trim();
    let email = posted.get("email").unwrap_or_default().trim();
    let password = posted.get("password").unwrap_or_default();
    let mut errors = Vec::new();
    if let Err(msg) = validate_username(username) {
        errors.push(ValidationError::field("username", msg));
    }
    if email.is_empty() || !is_valid_email(email) {
        errors.push(ValidationError::field("email", "Valid email is required"));
    }
    if let Err(msg) = validate_password(password) {
        errors.push(ValidationError::field("password", msg));
    } else if password != posted.get("password_confirm").unwrap_or_default() {
        errors.push(ValidationError::field(
            "password_confirm",
            "Passwords do not match",
        ));
    }
    errors
}

/// Admin account creation submit.
///
/// POST /install/admin
async fn install_admin_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    // The form pipeline first: the CSRF token and the saved form state, then
    // the fields.
    let posted = PostedForm(pairs);
    let viewer = UserContext::anonymous();
    let errors = admin_form_errors(&posted);
    let checked =
        super::admin_form::check_as(&state, &session, &viewer, &posted, ADMIN_FORM_ID, errors)
            .await;
    if let Err(Err(refusal)) = checked {
        return refusal;
    }

    // The wizard creates one administrator; once there is one, the next step
    // is the site form, not a second account.
    if count_admin_users(&state).await > 0 {
        return Redirect::to("/install/site").into_response();
    }

    let form_state = match checked {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => return render_admin_form(&state, &session, &errors).await,
        Err(Err(refusal)) => return refusal,
    };
    let failed = || [ValidationError::form("Failed to create admin account")];
    let username = posted.get("username").unwrap_or_default().trim();
    let email = posted.get("email").unwrap_or_default().trim();

    // Two submissions at once would both count no administrator and both
    // create one, since hashing the password leaves a wide gap between the
    // count and the insert. Only the submission holding a transaction
    // advisory lock may count and create; one that finds it taken goes on to
    // the site step, as it would once the other's account exists. The lock is
    // tried rather than waited for, so no submission holds a connection while
    // it waits for one the holder needs to write the account. Dropping the
    // transaction on any early return releases it.
    let mut lock = match state.db().begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(error = %e, "failed to begin the installer's admin lock");
            return render_admin_form(&state, &session, &failed()).await;
        }
    };
    match sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtext($1))")
        .bind("trovato_install_admin")
        .fetch_one(&mut *lock)
        .await
    {
        Ok(true) => {}
        Ok(false) => return Redirect::to("/install/site").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to take the installer's admin lock");
            return render_admin_form(&state, &session, &failed()).await;
        }
    }
    if count_admin_users(&state).await > 0 {
        return Redirect::to("/install/site").into_response();
    }

    // Check if username already exists
    if let Ok(Some(_)) = state
        .users()
        .find_by_name(posted.get("username").unwrap_or_default())
        .await
    {
        let errors = [ValidationError::field(
            "username",
            "Username already exists",
        )];
        return render_admin_form(&state, &session, &errors).await;
    }

    // Check if email already exists
    if let Ok(Some(_)) = state
        .users()
        .find_by_mail(posted.get("email").unwrap_or_default())
        .await
    {
        let errors = [ValidationError::field("email", "Email already in use")];
        return render_admin_form(&state, &session, &errors).await;
    }

    // Create admin user (anonymous context — no user exists yet)
    let input = CreateUser {
        name: username.to_string(),
        password: posted.get("password").unwrap_or_default().to_string(),
        mail: email.to_string(),
        is_admin: true,
    };

    let created = state.users().create(input, &viewer, "installer").await;
    // The account is written (or not): the next submission may count.
    if let Err(e) = lock.commit().await {
        tracing::warn!(error = %e, "failed to release the installer's admin lock");
    }
    match created {
        Ok(admin) => {
            if let Err(e) = session.insert(SESSION_INSTALL_ADMIN, admin.id).await {
                tracing::error!(error = %e, "failed to remember the installing administrator");
            }
            submitted(&state, &form_state, &posted).await;
            Redirect::to("/install/site").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create admin user");
            render_admin_form(&state, &session, &failed()).await
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

/// The site form. A session that did not create the administrator
/// (`ask_admin`) is also asked to sign in as one.
fn site_form(ask_admin: bool) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    let mut form = Form::new(SITE_FORM_ID).action("/install/site");
    if ask_admin {
        form = form
            .element(
                "admin_intro",
                FormElement::markup(
                    "<p style=\"margin-bottom: 1.5rem; color: var(--gray-700);\">\
                     The administrator account was created in another browser session. \
                     Sign in as that administrator to finish.</p>",
                )
                .weight(0),
            )
            .element(
                "admin_name",
                FormElement::textfield()
                    .title("Administrator username")
                    .required()
                    .attribute("autocomplete", "username")
                    .weight(1),
            )
            .element(
                "admin_password",
                FormElement::password()
                    .title("Administrator password")
                    .required()
                    .attribute("autocomplete", "current-password")
                    .weight(2),
            );
    }
    form.element(
        "site_name",
        FormElement::textfield()
            .title("Site Name")
            .default_value("Trovato")
            .required()
            .weight(3),
    )
    .element(
        "site_slogan",
        FormElement::textfield()
            .title("Site Slogan")
            .placeholder("Optional tagline")
            .weight(4),
    )
    .element(
        "site_mail",
        FormElement::input("email")
            .title("Site Email")
            .description("Used for system notifications")
            .placeholder("admin@example.com")
            .weight(5),
    )
    .element("op", FormElement::submit("Save and Continue").weight(10))
}

/// Render the site form, with `errors` when there are some.
async fn render_site_form(
    state: &AppState,
    session: &Session,
    errors: &[ValidationError],
) -> Response {
    let ask_admin = installing_admin(state, session).await.is_none();
    render_form_page(
        state,
        session,
        ("Site Configuration", 3),
        "Configure your site's basic information.",
        site_form(ask_admin),
        errors,
    )
    .await
}

/// Tell plugins a submission of an installer form was saved
/// (`tap_form_submit`), once, after the save.
async fn submitted(state: &AppState, form_state: &crate::form::FormState, posted: &PostedForm) {
    let viewer = UserContext::anonymous();
    state
        .forms()
        .submitted(
            form_state,
            posted,
            None,
            &super::item_form::tap_state(state, &viewer),
        )
        .await;
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

    render_site_form(&state, &session, &[]).await
}

/// Site configuration submit.
///
/// POST /install/site
async fn install_site_submit(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if check_installed(&state).await {
        return Redirect::to("/").into_response();
    }

    // The form pipeline first: the CSRF token, the saved form state and the
    // fields.
    let posted = PostedForm(pairs);
    let site_name = posted.get("site_name").unwrap_or_default().trim();
    let mut errors = Vec::new();
    if site_name.is_empty() {
        errors.push(ValidationError::field("site_name", "Site name is required"));
    }
    let form_state = match super::admin_form::check_as(
        &state,
        &session,
        &UserContext::anonymous(),
        &posted,
        SITE_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => return render_site_form(&state, &session, &errors).await,
        Err(Err(refusal)) => return refusal,
    };
    let refuse = |message: &str| [ValidationError::form(message)];

    // The step belongs to the administrator: the session that created them,
    // or one that signs in as an administrator on this form.
    if installing_admin(&state, &session).await.is_none() {
        let name = posted.get("admin_name").unwrap_or_default().trim();
        let password = posted.get("admin_password").unwrap_or_default();
        // The same lockout as the sign-in form, by name and address, so the
        // password cannot be guessed here at a rate sign-in would refuse.
        if let Ok(true) = state.lockout().is_locked(name, &client_ip.0).await {
            let errors = refuse("Too many failed attempts. Try again in a few minutes.");
            return render_site_form(&state, &session, &errors).await;
        }
        let admin = match state.users().find_by_name(name).await {
            Ok(Some(user)) if user.is_admin => Some(user),
            _ => None,
        };
        // Verified against a dummy hash when there is no such administrator,
        // so the answer takes as long either way.
        if !crate::models::User::verify_password_or_dummy(admin.as_ref(), password) {
            if let Err(e) = state
                .lockout()
                .record_failed_attempt(name, &client_ip.0)
                .await
            {
                tracing::warn!(error = %e, "failed to record a failed installer sign in");
            }
            let errors = refuse("Sign in as the site's administrator to finish the installation");
            return render_site_form(&state, &session, &errors).await;
        }
        if let Err(e) = state.lockout().clear_attempts(name, &client_ip.0).await {
            tracing::warn!(error = %e, "failed to clear installer sign in attempts");
        }
        if let Some(admin) = admin
            && let Err(e) = session.insert(SESSION_INSTALL_ADMIN, admin.id).await
        {
            tracing::error!(error = %e, "failed to remember the installing administrator");
        }
    }

    // Save configuration
    if let Err(e) = SiteConfig::set_site_name(state.db(), site_name).await {
        tracing::error!(error = %e, "failed to set site name");
        let errors = refuse("Failed to save configuration");
        return render_site_form(&state, &session, &errors).await;
    }

    if let Some(slogan) = posted.get("site_slogan")
        && let Err(e) = SiteConfig::set_site_slogan(state.db(), slogan.trim()).await
    {
        tracing::error!(error = %e, "failed to set site slogan");
    }

    if let Some(mail) = posted.get("site_mail")
        && let Err(e) = SiteConfig::set_site_mail(state.db(), mail.trim()).await
    {
        tracing::error!(error = %e, "failed to set site mail");
    }

    // Mark as installed
    if let Err(e) = SiteConfig::mark_installed(state.db()).await {
        tracing::error!(error = %e, "failed to mark site as installed");
        let errors = refuse("Failed to complete installation");
        return render_site_form(&state, &session, &errors).await;
    }

    submitted(&state, &form_state, &posted).await;
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

    render_installer("Installation Complete", 4, 4, &content)
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
