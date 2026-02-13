//! Installation check middleware.
//!
//! Redirects to the installer if the site is not yet installed.

use axum::{
    body::Body,
    extract::State,
    http::Request,
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};

use crate::models::SiteConfig;
use crate::state::AppState;

/// Middleware to check if installation is required.
///
/// Redirects all requests (except /install, /health, /static) to the installer
/// if the site is not yet installed.
pub async fn check_installation(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Allow install routes
    if path.starts_with("/install") {
        return next.run(request).await;
    }

    // Allow health check
    if path == "/health" {
        return next.run(request).await;
    }

    // Allow static files
    if path.starts_with("/static") {
        return next.run(request).await;
    }

    // Check if installed
    let installed = SiteConfig::is_installed(state.db()).await.unwrap_or(false);

    if !installed {
        return Redirect::to("/install").into_response();
    }

    next.run(request).await
}
