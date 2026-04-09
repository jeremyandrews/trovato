//! Site configuration admin routes.
//!
//! Provides the admin UI for managing site settings like site name,
//! email, language, registration mode, front page configuration,
//! SMTP delivery, and notification preferences.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::SiteConfig;
use crate::state::AppState;

use super::helpers::{render_admin_template, render_server_error, require_admin, require_csrf};

/// Session key for flash messages on the site config page.
const FLASH_KEY: &str = "site_config_flash";

/// Default items per page when not configured.
const DEFAULT_ITEMS_PER_PAGE: &str = "10";

// =============================================================================
// Form data
// =============================================================================

/// Site settings form data.
#[derive(Debug, Deserialize)]
struct SiteConfigFormData {
    #[serde(rename = "_token")]
    token: String,
    site_name: String,
    site_slogan: String,
    site_mail: String,
    front_page: String,
    items_per_page: String,
    registration_mode: String,
    #[serde(default)]
    smtp_host: String,
    #[serde(default)]
    smtp_port: String,
    #[serde(default)]
    smtp_username: String,
    #[serde(default)]
    smtp_password: String,
    #[serde(default)]
    smtp_encryption: String,
    #[serde(default)]
    smtp_from: String,
    #[serde(default)]
    notify_admin_on_register: Option<String>,
}

/// Test email form data (CSRF token only).
#[derive(Debug, Deserialize)]
struct TestEmailForm {
    #[serde(rename = "_token")]
    token: String,
}

// =============================================================================
// Helpers
// =============================================================================

/// Basic email format validation (requires `@` with text on both sides).
fn is_valid_email(email: &str) -> bool {
    let parts: Vec<&str> = email.splitn(2, '@').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].contains('.')
        && !parts[1].starts_with('.')
        && !parts[1].ends_with('.')
}

/// Load a string site config value, returning empty string on missing/error.
async fn load_config_string(pool: &sqlx::PgPool, key: &str) -> String {
    SiteConfig::get(pool, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

// =============================================================================
// Handlers
// =============================================================================

/// Render the site settings form.
///
/// GET /admin/config/site
async fn site_config_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let pool = state.db();

    let site_name = SiteConfig::site_name(pool)
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let site_slogan = SiteConfig::site_slogan(pool).await.unwrap_or_default();
    let site_mail = SiteConfig::site_mail(pool).await.unwrap_or_default();
    let front_page = SiteConfig::front_page(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let items_per_page = SiteConfig::get(pool, "items_per_page")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| DEFAULT_ITEMS_PER_PAGE.to_string());
    let registration_mode = SiteConfig::get(pool, "user_registration")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "admin_only".to_string());

    // SMTP settings
    let smtp_host = load_config_string(pool, "smtp_host").await;
    let smtp_port = {
        let raw = load_config_string(pool, "smtp_port").await;
        if raw.is_empty() {
            "587".to_string()
        } else {
            raw
        }
    };
    let smtp_username = load_config_string(pool, "smtp_username").await;
    let smtp_encryption = load_config_string(pool, "smtp_encryption").await;
    let smtp_from = load_config_string(pool, "smtp_from").await;

    // Password: detect env: prefix to show "configured via env" instead of the value
    let smtp_password_raw = load_config_string(pool, "smtp_password").await;
    let smtp_password_is_env = smtp_password_raw.starts_with("env:");
    let smtp_password = if smtp_password_is_env {
        String::new()
    } else {
        smtp_password_raw
    };

    // Notification preferences
    let notify_admin_on_register = SiteConfig::get(pool, "notify_admin_on_register")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let csrf_token = generate_csrf_token(&session).await;

    // Read and clear flash message
    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear site config flash message");
    }

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("site_name", &site_name);
    context.insert("site_slogan", &site_slogan);
    context.insert("site_mail", &site_mail);
    context.insert("front_page", &front_page);
    context.insert("items_per_page", &items_per_page);
    context.insert("registration_mode", &registration_mode);
    context.insert("smtp_host", &smtp_host);
    context.insert("smtp_port", &smtp_port);
    context.insert("smtp_username", &smtp_username);
    context.insert("smtp_password", &smtp_password);
    context.insert("smtp_password_is_env", &smtp_password_is_env);
    context.insert("smtp_encryption", &smtp_encryption);
    context.insert("smtp_from", &smtp_from);
    context.insert("notify_admin_on_register", &notify_admin_on_register);
    context.insert("flash", &flash);
    context.insert("path", "/admin/config/site");

    render_admin_template(&state, "admin/config/site.html", context).await
}

/// Save site settings.
///
/// POST /admin/config/site
async fn site_config_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<SiteConfigFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let pool = state.db();

    // Validate
    let mut errors: Vec<String> = Vec::new();

    let site_name = form.site_name.trim();
    if site_name.is_empty() {
        errors.push("Site name is required.".to_string());
    }

    let site_mail = form.site_mail.trim();
    if site_mail.is_empty() {
        errors.push("Site email is required.".to_string());
    } else if !is_valid_email(site_mail) {
        errors.push("Please enter a valid email address.".to_string());
    }

    let items_per_page_str = form.items_per_page.trim();
    if !items_per_page_str.is_empty() {
        match items_per_page_str.parse::<u32>() {
            Ok(n) if n == 0 || n > 100 => {
                errors.push("Items per page must be between 1 and 100.".to_string());
            }
            Err(_) => {
                errors.push("Items per page must be a number.".to_string());
            }
            Ok(_) => {}
        }
    }

    let registration_mode = form.registration_mode.trim();
    if !["open", "admin_only", "closed"].contains(&registration_mode) {
        errors.push("Invalid registration mode.".to_string());
    }

    // Validate SMTP port if provided
    let smtp_port_str = form.smtp_port.trim();
    if !smtp_port_str.is_empty() {
        match smtp_port_str.parse::<u16>() {
            Ok(0) => {
                errors.push("SMTP port must be between 1 and 65535.".to_string());
            }
            Err(_) => {
                errors.push("SMTP port must be a number.".to_string());
            }
            Ok(_) => {}
        }
    }

    // Validate SMTP from address if provided
    let smtp_from = form.smtp_from.trim();
    if !smtp_from.is_empty() && !is_valid_email(smtp_from) {
        errors.push("SMTP from address must be a valid email.".to_string());
    }

    // Validate encryption value
    let smtp_encryption = form.smtp_encryption.trim();
    if !smtp_encryption.is_empty() && !["starttls", "tls", "none"].contains(&smtp_encryption) {
        errors.push("Invalid SMTP encryption mode.".to_string());
    }

    if !errors.is_empty() {
        // Re-render form with errors
        let csrf_token = generate_csrf_token(&session).await;

        let smtp_password_raw = load_config_string(pool, "smtp_password").await;
        let smtp_password_is_env = smtp_password_raw.starts_with("env:");

        let notify_admin_on_register = form.notify_admin_on_register.is_some();

        let mut context = tera::Context::new();
        context.insert("csrf_token", &csrf_token);
        context.insert("site_name", &form.site_name);
        context.insert("site_slogan", &form.site_slogan);
        context.insert("site_mail", &form.site_mail);
        context.insert("front_page", &form.front_page);
        context.insert("items_per_page", &form.items_per_page);
        context.insert("registration_mode", &form.registration_mode);
        context.insert("smtp_host", &form.smtp_host);
        context.insert("smtp_port", &form.smtp_port);
        context.insert("smtp_username", &form.smtp_username);
        context.insert("smtp_password", &form.smtp_password);
        context.insert("smtp_password_is_env", &smtp_password_is_env);
        context.insert("smtp_encryption", &form.smtp_encryption);
        context.insert("smtp_from", &form.smtp_from);
        context.insert("notify_admin_on_register", &notify_admin_on_register);
        context.insert("errors", &errors);
        context.insert("path", "/admin/config/site");

        return render_admin_template(&state, "admin/config/site.html", context).await;
    }

    // Save each setting
    if let Err(e) = SiteConfig::set_site_name(pool, site_name).await {
        tracing::error!(error = %e, "failed to save site_name");
        return render_server_error("Failed to save site settings.");
    }

    if let Err(e) = SiteConfig::set_site_slogan(pool, form.site_slogan.trim()).await {
        tracing::error!(error = %e, "failed to save site_slogan");
        return render_server_error("Failed to save site settings.");
    }

    if let Err(e) = SiteConfig::set_site_mail(pool, site_mail).await {
        tracing::error!(error = %e, "failed to save site_mail");
        return render_server_error("Failed to save site settings.");
    }

    if let Err(e) = SiteConfig::set_front_page(pool, form.front_page.trim()).await {
        tracing::error!(error = %e, "failed to save front_page");
        return render_server_error("Failed to save site settings.");
    }

    let ipp = if items_per_page_str.is_empty() {
        DEFAULT_ITEMS_PER_PAGE
    } else {
        items_per_page_str
    };
    if let Err(e) = SiteConfig::set(pool, "items_per_page", serde_json::json!(ipp)).await {
        tracing::error!(error = %e, "failed to save items_per_page");
        return render_server_error("Failed to save site settings.");
    }

    if let Err(e) = SiteConfig::set(
        pool,
        "user_registration",
        serde_json::json!(registration_mode),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save user_registration");
        return render_server_error("Failed to save site settings.");
    }

    // Save SMTP settings
    let smtp_host = form.smtp_host.trim();
    if let Err(e) = SiteConfig::set(pool, "smtp_host", serde_json::json!(smtp_host)).await {
        tracing::error!(error = %e, "failed to save smtp_host");
    }

    let smtp_port_val = if smtp_port_str.is_empty() {
        "587"
    } else {
        smtp_port_str
    };
    if let Err(e) = SiteConfig::set(pool, "smtp_port", serde_json::json!(smtp_port_val)).await {
        tracing::error!(error = %e, "failed to save smtp_port");
    }

    if let Err(e) = SiteConfig::set(
        pool,
        "smtp_username",
        serde_json::json!(form.smtp_username.trim()),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save smtp_username");
    }

    // Only overwrite password if a non-empty value was submitted and not env-sourced
    let password_trimmed = form.smtp_password.trim();
    if !password_trimmed.is_empty() {
        let current_raw = load_config_string(pool, "smtp_password").await;
        if !current_raw.starts_with("env:")
            && let Err(e) =
                SiteConfig::set(pool, "smtp_password", serde_json::json!(password_trimmed)).await
        {
            tracing::error!(error = %e, "failed to save smtp_password");
        }
    }

    if let Err(e) =
        SiteConfig::set(pool, "smtp_encryption", serde_json::json!(smtp_encryption)).await
    {
        tracing::error!(error = %e, "failed to save smtp_encryption");
    }

    if let Err(e) = SiteConfig::set(pool, "smtp_from", serde_json::json!(smtp_from)).await {
        tracing::error!(error = %e, "failed to save smtp_from");
    }

    // Notification preferences — checkbox: present means true, absent means false
    let notify = form.notify_admin_on_register.is_some();
    if let Err(e) =
        SiteConfig::set(pool, "notify_admin_on_register", serde_json::json!(notify)).await
    {
        tracing::error!(error = %e, "failed to save notify_admin_on_register");
    }

    let _ = session
        .insert(FLASH_KEY, "Settings saved successfully.")
        .await;
    tracing::info!("Site settings updated");
    Redirect::to("/admin/config/site").into_response()
}

/// Send a test email to the configured site email address.
///
/// POST /admin/config/site/test-email
async fn test_email(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<TestEmailForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let site_mail = SiteConfig::site_mail(state.db()).await.unwrap_or_default();
    if site_mail.is_empty() {
        let _ = session
            .insert(
                FLASH_KEY,
                "Cannot send test email: site email is not configured.",
            )
            .await;
        return Redirect::to("/admin/config/site").into_response();
    }

    if let Some(email_service) = state.email() {
        match email_service
            .send(
                &site_mail,
                "Trovato test email",
                "This is a test email from your Trovato site.\n\n\
                 If you received this, SMTP is configured correctly.",
            )
            .await
        {
            Ok(()) => {
                let _ = session
                    .insert(FLASH_KEY, &format!("Test email sent to {site_mail}."))
                    .await;
            }
            Err(e) => {
                tracing::error!(error = %e, "test email failed");
                let _ = session
                    .insert(FLASH_KEY, &format!("Failed to send test email: {e}"))
                    .await;
            }
        }
    } else {
        let _ = session
            .insert(
                FLASH_KEY,
                "Email service is not configured. Set SMTP_HOST environment variable \
                 or configure SMTP settings above and restart the server.",
            )
            .await;
    }

    Redirect::to("/admin/config/site").into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the site configuration admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/config/site",
            get(site_config_form).post(site_config_submit),
        )
        .route("/admin/config/site/test-email", post(test_email))
}
