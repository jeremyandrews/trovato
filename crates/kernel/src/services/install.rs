//! What first-time setup does, shared by the web installer
//! (`routes::install`) and the `trovato install` command (`main.rs`): create
//! the first administrator, record the site's name and mail, and mark the
//! site installed.
//!
//! The web installer still asks for these in its own two steps, each with its
//! own page and its own error message; this module is the one place that
//! does the work once a step's form has passed its own checks, so the two
//! callers cannot drift apart on what "installed" means.

use anyhow::Result;

use crate::models::{CreateUser, SiteConfig, User};
use crate::routes::helpers::{is_valid_email, validate_password, validate_username};
use crate::state::AppState;
use crate::tap::UserContext;

/// Why [`create_admin`] refused to create the account.
pub enum CreateAdminError {
    /// The username fails [`validate_username`]; the text names what is wrong.
    Username(&'static str),
    /// The email is missing or fails [`is_valid_email`].
    Email(&'static str),
    /// The password fails [`validate_password`]; the text names what is wrong.
    Password(&'static str),
    /// Another account already holds this username.
    UsernameTaken,
    /// Another account already holds this email.
    EmailTaken,
    /// The insert itself failed.
    Failed(anyhow::Error),
}

/// How many administrators exist (excluding the anonymous account).
///
/// Zero means the installer's admin step has not run yet; the web installer
/// and `trovato install` both use this to decide whether to create one.
pub async fn admin_count(pool: &sqlx::PgPool) -> Result<i64> {
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM users WHERE is_admin = true AND id != '00000000-0000-0000-0000-000000000000'"
    )
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Create the first administrator: the work `install_admin_submit` does once
/// its form has matching passwords, now shared with the CLI, which has no
/// confirmation field to check.
///
/// `path` names where the account came from (`installer` or `cli`), passed
/// straight through to [`crate::services::user::UserService::create`] for its
/// security event.
pub async fn create_admin(
    state: &AppState,
    username: &str,
    email: &str,
    password: String,
    path: &str,
) -> Result<User, CreateAdminError> {
    let username = username.trim();
    let email = email.trim();

    validate_username(username).map_err(CreateAdminError::Username)?;
    if email.is_empty() || !is_valid_email(email) {
        return Err(CreateAdminError::Email("Valid email is required"));
    }
    validate_password(&password).map_err(CreateAdminError::Password)?;

    if let Ok(Some(_)) = state.users().find_by_name(username).await {
        return Err(CreateAdminError::UsernameTaken);
    }
    if let Ok(Some(_)) = state.users().find_by_mail(email).await {
        return Err(CreateAdminError::EmailTaken);
    }

    let input = CreateUser {
        name: username.to_string(),
        password,
        mail: email.to_string(),
        is_admin: true,
    };

    // Anonymous context: no user exists yet to act as.
    let anon = UserContext::anonymous();
    state
        .users()
        .create(input, &anon, path)
        .await
        .map_err(CreateAdminError::Failed)
}

/// Why [`finish_site_setup`] could not complete the site step.
pub enum FinishSiteSetupError {
    /// The site name is empty.
    SiteNameRequired,
    /// Saving the site name failed.
    SiteNameSaveFailed(anyhow::Error),
    /// Marking the site installed failed.
    MarkInstalledFailed(anyhow::Error),
}

/// Record the site's name, slogan and mail, and mark it installed: the work
/// `install_site_submit` does once its form has a name, now shared with the
/// CLI.
///
/// The slogan and mail are best-effort, as the web form already treats them:
/// a site is still installed even if one of them fails to save, with the
/// failure logged rather than refused.
pub async fn finish_site_setup(
    pool: &sqlx::PgPool,
    site_name: &str,
    site_slogan: Option<&str>,
    site_mail: Option<&str>,
) -> Result<(), FinishSiteSetupError> {
    let site_name = site_name.trim();
    if site_name.is_empty() {
        return Err(FinishSiteSetupError::SiteNameRequired);
    }

    SiteConfig::set_site_name(pool, site_name)
        .await
        .map_err(FinishSiteSetupError::SiteNameSaveFailed)?;

    if let Some(slogan) = site_slogan
        && let Err(e) = SiteConfig::set_site_slogan(pool, slogan.trim()).await
    {
        tracing::error!(error = %e, "failed to set site slogan");
    }

    if let Some(mail) = site_mail
        && let Err(e) = SiteConfig::set_site_mail(pool, mail.trim()).await
    {
        tracing::error!(error = %e, "failed to set site mail");
    }

    SiteConfig::mark_installed(pool)
        .await
        .map_err(FinishSiteSetupError::MarkInstalledFailed)
}
