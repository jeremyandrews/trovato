//! Site configuration admin routes.
//!
//! Provides the admin UI for managing site settings like site name,
//! email, language, registration mode, and front page configuration.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
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

    if !errors.is_empty() {
        // Re-render form with errors
        let csrf_token = generate_csrf_token(&session).await;

        let mut context = tera::Context::new();
        context.insert("csrf_token", &csrf_token);
        context.insert("site_name", &form.site_name);
        context.insert("site_slogan", &form.site_slogan);
        context.insert("site_mail", &form.site_mail);
        context.insert("front_page", &form.front_page);
        context.insert("items_per_page", &form.items_per_page);
        context.insert("registration_mode", &form.registration_mode);
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

    let _ = session
        .insert(FLASH_KEY, "Settings saved successfully.")
        .await;
    tracing::info!("Site settings updated");
    Redirect::to("/admin/config/site").into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the site configuration admin router.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/admin/config/site",
        get(site_config_form).post(site_config_submit),
    )
}
