//! Site configuration admin routes.
//!
//! Provides the admin UI for managing site settings like site name,
//! email, language, registration mode, front page configuration and
//! notification preferences. SMTP delivery is configured in the environment
//! only (`SMTP_*`); the page shows what the kernel is using, read only.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::SiteConfig;
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_server_error, require_csrf, require_permission,
};

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
    notify_admin_on_register: Option<String>,
    #[serde(default)]
    update_check: Option<String>,
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

// =============================================================================
// Handlers
// =============================================================================

/// Render the site settings form.
///
/// GET /admin/config/site
async fn site_config_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

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
    // The effective mode, so a site that opened registration through the legacy
    // boolean sees "open" here rather than a form that disagrees with its own
    // register route.
    let registration_mode = crate::models::RegistrationMode::load(pool)
        .await
        .as_str()
        .to_string();

    // Notification preferences
    let update_check = crate::update_status::setting_allows_check(pool).await;
    let update_status = crate::update_status::stored_status(pool).await;
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
    context.insert("smtp", state.smtp_summary());
    context.insert("notify_admin_on_register", &notify_admin_on_register);
    context.insert("update_check", &update_check);
    context.insert("update_status", &update_status);
    context.insert("running_version", crate::update_status::running_version());
    context.insert("flash", &flash);
    context.insert("path", "/admin/config/site");
    if let Err(resp) = insert_maintenance_form(&state, &session, &user, &mut context).await {
        return resp;
    }

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/config/site.html",
        context,
    )
    .await
}

/// Save site settings.
///
/// POST /admin/config/site
async fn site_config_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<SiteConfigFormData>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
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

    // Any internal route may be the front page, but only an internal one:
    // rejected here as well as at serve time, so an operator finds out at the
    // form rather than at the home page.
    let front_page = form.front_page.trim();
    if !front_page.is_empty() && !super::front::is_local_path(front_page) {
        errors.push(
            "Front page must be a path on this site, beginning with \"/\" (for example /blog)."
                .to_string(),
        );
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

        let notify_admin_on_register = form.notify_admin_on_register.is_some();

        let mut context = tera::Context::new();
        context.insert("csrf_token", &csrf_token);
        context.insert("site_name", &form.site_name);
        context.insert("site_slogan", &form.site_slogan);
        context.insert("site_mail", &form.site_mail);
        context.insert("front_page", &form.front_page);
        context.insert("items_per_page", &form.items_per_page);
        context.insert("registration_mode", &form.registration_mode);
        context.insert("smtp", state.smtp_summary());
        context.insert("notify_admin_on_register", &notify_admin_on_register);
        context.insert("update_check", &form.update_check.is_some());
        context.insert(
            "update_status",
            &crate::update_status::stored_status(pool).await,
        );
        context.insert("running_version", crate::update_status::running_version());
        context.insert("errors", &errors);
        context.insert("path", "/admin/config/site");
        if let Err(resp) = insert_maintenance_form(&state, &session, &user, &mut context).await {
            return resp;
        }

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/config/site.html",
            context,
        )
        .await;
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

    if let Err(e) = SiteConfig::set_front_page(pool, front_page).await {
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

    // Saving also clears the boolean this setting supersedes, so a site cannot end
    // up with two registration settings that disagree.
    let mode = match registration_mode {
        "open" => crate::models::RegistrationMode::Open,
        "admin_only" | "closed" => crate::models::RegistrationMode::AdminOnly,
        other => {
            tracing::warn!(value = %other, "rejected unknown registration mode");
            return render_server_error("Unknown registration mode.");
        }
    };
    if let Err(e) = mode.save(pool).await {
        tracing::error!(error = %e, "failed to save user_registration");
        return render_server_error("Failed to save site settings.");
    }

    // Notification preferences — checkbox: present means true, absent means false
    let notify = form.notify_admin_on_register.is_some();
    if let Err(e) =
        SiteConfig::set(pool, "notify_admin_on_register", serde_json::json!(notify)).await
    {
        tracing::error!(error = %e, "failed to save notify_admin_on_register");
    }

    // Update check — checkbox: present means on, absent means off. Stored as a
    // boolean rather than absent-means-on, so turning it off survives.
    if let Err(e) = SiteConfig::set(
        pool,
        crate::update_status::UPDATE_CHECK_KEY,
        serde_json::json!(form.update_check.is_some()),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save update_check");
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
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
                "Email service is not configured. Set the SMTP_HOST environment \
                 variable and restart the server.",
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
        .route("/admin/config/site/maintenance", post(maintenance_submit))
}

// =============================================================================
// Maintenance mode
// =============================================================================

/// The form id of the maintenance mode form.
const MAINTENANCE_FORM_ID: &str = "site_maintenance_form";

/// The maintenance mode form: on or off, and the message visitors see.
fn maintenance_form(on: bool, message: &str) -> crate::form::Form {
    use crate::form::FormElement;
    crate::form::Form::new(MAINTENANCE_FORM_ID)
        .action("/admin/config/site/maintenance")
        .element(
            "maintenance_mode",
            FormElement::checkbox()
                .title("Put the site into maintenance mode")
                .description(
                    "Everyone without the \"bypass maintenance mode\" permission gets a 503 \
                     with this message. Signing in, static files, the health check and cron \
                     still work, and administrators can use the whole site.",
                )
                .default_value(on)
                .weight(0),
        )
        .element(
            "maintenance_message",
            FormElement::textarea(3)
                .title("Message")
                .default_value(message.to_string())
                .weight(1),
        )
        .element(
            "op",
            FormElement::submit("Save maintenance mode").weight(10),
        )
}

/// Build the maintenance mode form through the forms pipeline and put it,
/// rendered, and whether the mode is on into `context`.
async fn insert_maintenance_form(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    context: &mut tera::Context,
) -> Result<(), Response> {
    let (on, message) = crate::middleware::maintenance::current(state).await;
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut form_state = crate::form::FormState::new(MAINTENANCE_FORM_ID, "");
    form_state.user_id = Some(user.id);
    let html = state
        .forms()
        .build(
            maintenance_form(on, &message),
            session,
            &super::item_form::tap_state(state, &viewer),
            form_state,
        )
        .await
        .and_then(|form| state.theme().render_form(&form))
        .map_err(|e| {
            tracing::error!(error = %e, "failed to build the maintenance form");
            render_server_error("The site settings could not be displayed.")
        })?;
    context.insert("maintenance_form", &html);
    context.insert("maintenance_on", &on);
    Ok(())
}

/// Turn maintenance mode on or off, and set its message.
///
/// POST /admin/config/site/maintenance
async fn maintenance_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = crate::content::submission::PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    match state
        .forms()
        .process(
            posted.get("_form_build_id").unwrap_or_default(),
            &posted,
            &session,
            &super::item_form::tap_state(&state, &viewer),
            |form_state| {
                if form_state.form_id == MAINTENANCE_FORM_ID {
                    Ok(())
                } else {
                    Err(vec![crate::form::ValidationError::form(
                        crate::form::FORM_EXPIRED,
                    )])
                }
            },
        )
        .await
    {
        Ok(_) => {}
        Err(crate::form::ProcessError::Csrf) => {
            return super::helpers::render_error(
                "Invalid or expired form token. Please try again.",
            );
        }
        Err(crate::form::ProcessError::Invalid(_)) => {
            return super::helpers::render_error(
                "The form has expired. Reload the page and try again.",
            );
        }
    }

    let on = posted.contains("maintenance_mode");
    let message = posted
        .get("maintenance_message")
        .unwrap_or_default()
        .trim()
        .to_string();
    let pool = state.db();
    if let Err(e) = SiteConfig::set(
        pool,
        crate::middleware::maintenance::MAINTENANCE_MESSAGE_KEY,
        serde_json::json!(message),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save the maintenance message");
        return render_server_error("Failed to save maintenance mode.");
    }
    if let Err(e) = SiteConfig::set(
        pool,
        crate::middleware::maintenance::MAINTENANCE_MODE_KEY,
        serde_json::json!(on),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save maintenance mode");
        return render_server_error("Failed to save maintenance mode.");
    }
    tracing::info!(
        log_type = "config",
        user_id = %user.id,
        maintenance_mode = on,
        "maintenance mode changed"
    );
    let notice = if on {
        "Maintenance mode is on."
    } else {
        "Maintenance mode is off."
    };
    let _ = session.insert(FLASH_KEY, notice).await;
    Redirect::to("/admin/config/site#maintenance").into_response()
}
