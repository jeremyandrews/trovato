//! The site's password policy and "administrators require a second factor"
//! setting (B304).
//!
//! `GET`/`POST /admin/config/people/security`, guarded by `administer site`,
//! following the hand-rolled Tera context pattern `admin_config.rs` uses
//! rather than the full forms pipeline.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::form::csrf::generate_csrf_token;
use crate::routes::admin_form::FieldError;
use crate::services::password_policy::{MINIMUM_MIN_LENGTH, PasswordPolicy};
use crate::services::second_factor::ADMIN_REQUIRES_SECOND_FACTOR_KEY;
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_server_error, require_csrf, require_permission,
};

/// Session key for the flash message on this page.
const FLASH_KEY: &str = "security_settings_flash";

#[derive(Debug, Deserialize)]
struct SecurityFormData {
    #[serde(rename = "_token")]
    token: String,
    min_length: String,
    #[serde(default)]
    require_lowercase: Option<String>,
    #[serde(default)]
    require_uppercase: Option<String>,
    #[serde(default)]
    require_digit: Option<String>,
    #[serde(default)]
    require_symbol: Option<String>,
    max_age_days: String,
    #[serde(default)]
    admin_requires_second_factor: Option<String>,
}

async fn admin_requirement_enabled(state: &AppState) -> bool {
    crate::services::second_factor::admin_requirement_enabled(state).await
}

fn insert_policy_context(context: &mut tera::Context, policy: &PasswordPolicy, admin_2fa: bool) {
    context.insert("min_length", &policy.min_length.to_string());
    context.insert("require_lowercase", &policy.require_lowercase);
    context.insert("require_uppercase", &policy.require_uppercase);
    context.insert("require_digit", &policy.require_digit);
    context.insert("require_symbol", &policy.require_symbol);
    context.insert("max_age_days", &policy.max_age_days.to_string());
    context.insert("admin_requires_second_factor", &admin_2fa);
    context.insert("minimum_min_length", &MINIMUM_MIN_LENGTH);
}

/// GET /admin/config/people/security
async fn security_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(resp) = require_permission(&state, &session, "administer site").await {
        return resp;
    }
    let pool = state.db();
    let policy = PasswordPolicy::load(pool).await;
    let admin_2fa = admin_requirement_enabled(&state).await;

    let flash: Option<serde_json::Value> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some() {
        let _ = session.remove::<serde_json::Value>(FLASH_KEY).await;
    }

    let mut context = tera::Context::new();
    context.insert("csrf_token", &generate_csrf_token(&session).await);
    insert_policy_context(&mut context, &policy, admin_2fa);
    context.insert("flash", &flash);
    context.insert("path", "/admin/config/people/security");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/config/security.html",
        context,
    )
    .await
}

/// POST /admin/config/people/security
async fn security_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<SecurityFormData>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    let pool = state.db();

    let mut errors: Vec<FieldError> = Vec::new();

    let min_length: usize = match form.min_length.trim().parse() {
        Ok(n) if n >= MINIMUM_MIN_LENGTH && n <= 128 => n,
        Ok(_) => {
            errors.push(FieldError::on(
                "min_length",
                format!("Minimum length must be between {MINIMUM_MIN_LENGTH} and 128."),
            ));
            MINIMUM_MIN_LENGTH
        }
        Err(_) => {
            errors.push(FieldError::on(
                "min_length",
                "Minimum length must be a number.",
            ));
            MINIMUM_MIN_LENGTH
        }
    };

    let max_age_days: u32 = match form.max_age_days.trim() {
        "" => 0,
        s => match s.parse() {
            Ok(n) => n,
            Err(_) => {
                errors.push(FieldError::on(
                    "max_age_days",
                    "Maximum age must be a number of days, or blank for none.",
                ));
                0
            }
        },
    };

    let previous = PasswordPolicy::load(pool).await;
    let previous_admin_2fa = admin_requirement_enabled(&state).await;

    if !errors.is_empty() {
        let mut context = tera::Context::new();
        context.insert("csrf_token", &generate_csrf_token(&session).await);
        context.insert("min_length", &form.min_length);
        context.insert("require_lowercase", &form.require_lowercase.is_some());
        context.insert("require_uppercase", &form.require_uppercase.is_some());
        context.insert("require_digit", &form.require_digit.is_some());
        context.insert("require_symbol", &form.require_symbol.is_some());
        context.insert("max_age_days", &form.max_age_days);
        context.insert(
            "admin_requires_second_factor",
            &form.admin_requires_second_factor.is_some(),
        );
        context.insert("minimum_min_length", &MINIMUM_MIN_LENGTH);
        context.insert("errors", &errors);
        context.insert("path", "/admin/config/people/security");
        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/config/security.html",
            context,
        )
        .await;
    }

    let new_policy = PasswordPolicy {
        min_length,
        require_lowercase: form.require_lowercase.is_some(),
        require_uppercase: form.require_uppercase.is_some(),
        require_digit: form.require_digit.is_some(),
        require_symbol: form.require_symbol.is_some(),
        max_age_days,
        max_age_enabled_at: previous.max_age_enabled_at,
    }
    .resolved_for_save(&previous);

    if let Err(e) = new_policy.save(pool).await {
        tracing::error!(error = %e, "failed to save the password policy");
        return render_server_error("Failed to save the security settings.");
    }

    let new_admin_2fa = form.admin_requires_second_factor.is_some();
    if let Err(e) = crate::models::SiteConfig::set(
        pool,
        ADMIN_REQUIRES_SECOND_FACTOR_KEY,
        serde_json::json!(new_admin_2fa),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save the administrator second-factor setting");
        return render_server_error("Failed to save the security settings.");
    }

    if previous != new_policy || previous_admin_2fa != new_admin_2fa {
        state
            .security_audit()
            .emit(
                SecurityEvent::new(SecurityEventKind::PasswordPolicyChanged)
                    .actor(user.id)
                    .detail("min_length", new_policy.min_length)
                    .detail("max_age_days", new_policy.max_age_days)
                    .detail("admin_requires_second_factor", new_admin_2fa),
            )
            .await;
    }

    let _ = session
        .insert(FLASH_KEY, "Security settings saved successfully.")
        .await;
    Redirect::to("/admin/config/people/security").into_response()
}

/// Build the security settings admin router.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/admin/config/people/security",
        get(security_form).post(security_submit),
    )
}
