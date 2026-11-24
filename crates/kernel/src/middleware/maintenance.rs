//! Maintenance mode (B107).
//!
//! While the `maintenance_mode` site setting is on, every request is answered
//! `503 Service Unavailable` with a themed page and `Retry-After`, except:
//!
//! - from a signed-in user holding [`BYPASS_PERMISSION`] (an administrator
//!   holds it implicitly), so the site can be worked on;
//! - for the paths a site needs to come back: signing in, static files, the
//!   health check, and cron.
//!
//! The setting is read on each request, as the installation check reads its
//! own: an operator who turns maintenance on from one server turns it on for
//! every server at once, with no cache to wait out.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use tower_sessions::Session;

use crate::models::SiteConfig;
use crate::state::AppState;

/// The site setting that turns maintenance mode on (a boolean).
pub const MAINTENANCE_MODE_KEY: &str = "maintenance_mode";

/// The site setting holding the message the maintenance page shows.
pub const MAINTENANCE_MESSAGE_KEY: &str = "maintenance_message";

/// The permission that lets a signed-in user use the site while it is in
/// maintenance mode.
pub const BYPASS_PERMISSION: &str = "bypass maintenance mode";

/// The message shown when none is set.
pub const DEFAULT_MESSAGE: &str = "The site is down for maintenance. Please check back again soon.";

/// Seconds a client is asked to wait before trying again.
const RETRY_AFTER_SECS: u32 = 300;

/// Whether `path` is served whatever the mode: signing in, static files, the
/// health check and cron.
pub fn is_exempt(path: &str) -> bool {
    let under = |prefix: &str| {
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    under("/user/login")
        || under("/static")
        || crate::routes::health::is_probe_path(path)
        || under("/cron")
}

/// Whether maintenance mode is on, and its message.
pub async fn current(state: &AppState) -> (bool, String) {
    let on = SiteConfig::get(state.db(), MAINTENANCE_MODE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let message = SiteConfig::get(state.db(), MAINTENANCE_MESSAGE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MESSAGE.to_string());
    (on, message)
}

/// The middleware. Sits inside the session layer, after the installation
/// check.
pub async fn check_maintenance(
    State(state): State<AppState>,
    session: Session,
    request: Request<Body>,
    next: Next,
) -> Response {
    if is_exempt(request.uri().path()) {
        return next.run(request).await;
    }
    let on = SiteConfig::get(state.db(), MAINTENANCE_MODE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !on {
        return next.run(request).await;
    }
    let viewer = crate::routes::helpers::get_user_context(&session, &state).await;
    if viewer.authenticated && viewer.can(BYPASS_PERMISSION) {
        return next.run(request).await;
    }
    maintenance_response(&state).await
}

/// The 503: the themed maintenance page, or a plain one if the theme cannot
/// render it.
async fn maintenance_response(state: &AppState) -> Response {
    let (_, message) = current(state).await;
    let site_name = SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let mut context = tera::Context::new();
    context.insert("site_name", &site_name);
    context.insert("maintenance_message", &message);
    let body = match state.theme().tera().render("maintenance.html", &context) {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to render the maintenance page");
            format!(
                "<!DOCTYPE html><title>Down for maintenance</title><p>{}</p>",
                crate::routes::helpers::html_escape(&message)
            )
        }
    };
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, Html(body)).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECS));
    // Never cache the page in place of the site.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::is_exempt;

    #[test]
    fn the_paths_a_site_needs_to_come_back_are_exempt() {
        for path in [
            "/user/login",
            "/user/login/passkey",
            "/static/css/admin.css",
            "/health",
            "/cron/abc",
            "/cron/status",
        ] {
            assert!(is_exempt(path), "{path}");
        }
        for path in [
            "/",
            "/admin",
            "/user/loginx",
            "/staticky",
            "/health/deep",
            "/item/1",
            "/api/v1/items",
        ] {
            assert!(!is_exempt(path), "{path}");
        }
    }
}
