//! Choose the theme a request renders with (B108).
//!
//! The theme engine renders `/admin` pages with the admin theme and every
//! other page with the default theme (`theme::registry`). Which one a render
//! is for is set here, once per request, from the request path, so every
//! template a handler or an outer layer renders (the page, its forms, an
//! error page) comes from the same theme.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// Run the rest of the request inside its theme scope.
pub async fn scope_theme(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    crate::theme::scope_request(&path, next.run(request)).await
}
