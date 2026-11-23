//! One error page: the outer layer every kernel error response passes through.
//!
//! A handler, a helper or [`AppError`] says *that* a response is an error by
//! attaching an [`ErrorPage`] marker to it: the status and a message safe to
//! show. It does not decide what the error looks like. [`render_error_pages`]
//! does, once, from what the caller asked for:
//!
//! - a caller whose `Accept` names `text/html` (a browser) gets the site's
//!   themed `error.html`, with `error--{status}.html` tried first, inside the
//!   normal page wrapper (`page--admin.html` on an admin path), and a link to
//!   `/` or `/admin`. Nobody signed in (`401`) is shown the same page as
//!   `403` "Access denied", with the login form on it carrying the page asked
//!   for as its destination: the HTML half of the refusal rule;
//! - every other caller gets the [`AppError`] JSON body.
//!
//! Status codes and other headers are kept. A response that is already JSON
//! keeps its body for a JSON caller.
//!
//! Axum's own refusals (a body that does not parse, a method not allowed, a
//! wrong content type) carry no marker; they are plain text or empty. An
//! error response that is plain text or empty and carries no marker is treated
//! as one, with its text as the message, so it takes the same two forms. A
//! plugin's own response body is marked [`PluginBody`] and left alone.
//!
//! If the themed page cannot be rendered (the theme itself is what failed),
//! [`fallback_page`] is the one built-in page that remains.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use tower_sessions::Session;

use crate::error::AppError;
use crate::state::AppState;

/// The marker that makes a response an error page.
///
/// Attached by the four page helpers in `routes::helpers`, by
/// [`AppError`]'s `IntoResponse`, and by [`render_error_pages`] itself for an
/// unmarked plain-text error. `message` is shown to the caller.
#[derive(Debug, Clone)]
pub struct ErrorPage {
    /// The status the error answers with.
    pub status: StatusCode,
    /// A message safe to show the caller.
    pub message: String,
}

/// Marks a response whose body a plugin wrote, which this layer never rewrites.
#[derive(Debug, Clone, Copy)]
pub struct PluginBody;

/// Attach an [`ErrorPage`] marker to `response`, keeping its status.
pub fn mark(mut response: Response, message: impl Into<String>) -> Response {
    let status = response.status();
    response.extensions_mut().insert(ErrorPage {
        status,
        message: message.into(),
    });
    response
}

/// The error-page layer. See the module documentation.
pub async fn render_error_pages(
    State(state): State<AppState>,
    session: Session,
    request: Request,
    next: Next,
) -> Response {
    let wants_html = accepts_html(request.headers());
    let path = request.uri().path().to_string();
    // Where the login form on an access-denied page sends the visitor once
    // they are signed in: the page they asked for, query and all. Only a GET
    // is a page to return to; a refused POST has nothing to replay.
    let destination = (request.method() == axum::http::Method::GET)
        .then(|| {
            request
                .uri()
                .path_and_query()
                .map(|pq| pq.as_str().to_string())
        })
        .flatten();

    let response = next.run(request).await;
    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }
    if response.extensions().get::<PluginBody>().is_some() {
        return response;
    }

    let (marker, response) = match response.extensions().get::<ErrorPage>().cloned() {
        Some(marker) => (marker, response),
        None => match unmarked_error(response).await {
            Ok(found) => found,
            Err(untouched) => return untouched,
        },
    };

    if wants_html {
        // Nobody signed in is refused as a page that needs a sign in should
        // refuse them: `403` "Access denied", with the login form on the page.
        // It was a redirect to `/user/login`, which lost the page asked for and
        // answered a crawler or an old link `200`.
        let (response, marker) = if marker.status == StatusCode::UNAUTHORIZED {
            let (mut parts, body) = response.into_parts();
            parts.status = StatusCode::FORBIDDEN;
            parts.headers.remove(header::WWW_AUTHENTICATE);
            let marker = ErrorPage {
                status: StatusCode::FORBIDDEN,
                message: crate::routes::helpers::LOGIN_REQUIRED_MESSAGE.to_string(),
            };
            (Response::from_parts(parts, body), marker)
        } else {
            (response, marker)
        };
        let html = themed_page(&state, &session, &path, destination.as_deref(), &marker)
            .await
            .unwrap_or_else(|| fallback_page(&marker, home_for(&path)));
        return replace_body(response, "text/html; charset=utf-8", html);
    }

    if is_json(response.headers()) {
        return response;
    }
    let body = serde_json::to_string(&AppError::with_status(marker.status, &marker.message).body())
        .unwrap_or_default();
    replace_body(response, "application/json", body)
}

/// Whether the caller asked for HTML: a browser names `text/html` in `Accept`.
/// `*/*` alone (curl, `fetch` by default) is not a request for a page.
fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"))
}

/// An error response nobody marked: plain text or empty becomes a marker with
/// its text as the message; anything else (a JSON or HTML body someone wrote
/// on purpose) is handed back untouched.
async fn unmarked_error(response: Response) -> Result<(ErrorPage, Response), Response> {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let plain = match content_type.as_deref() {
        None => true,
        Some(ct) => ct.starts_with("text/plain"),
    };
    if !plain {
        return Err(response);
    }
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 64 * 1024).await.unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    let message = if text.is_empty() {
        parts
            .status
            .canonical_reason()
            .unwrap_or("Error")
            .to_string()
    } else {
        text
    };
    let marker = ErrorPage {
        status: parts.status,
        message,
    };
    Ok((marker, Response::from_parts(parts, Body::empty())))
}

/// `response` with a new body and content type; status and every other header
/// (`Retry-After`, `WWW-Authenticate`, `Set-Cookie`) kept.
fn replace_body(response: Response, content_type: &'static str, body: String) -> Response {
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    Response::from_parts(parts, Body::from(body))
}

/// Where an error page sends a person: the admin dashboard from an admin path,
/// the front page from everywhere else.
fn home_for(path: &str) -> (&'static str, &'static str) {
    if crate::theme::ThemeEngine::is_admin_path(path) {
        ("/admin", "Return to the administration")
    } else {
        ("/", "Go to the home page")
    }
}

/// The page's heading for a status.
fn title_for(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "That request could not be used",
        403 => "Access denied",
        404 => "Page not found",
        405 => "That is not allowed here",
        409 => "That conflicts with something else",
        413 => "That is too large",
        429 => "Too many requests",
        s if s >= 500 => "Something went wrong",
        _ => status.canonical_reason().unwrap_or("Error"),
    }
}

/// The error page in the site's theme, or `None` when the theme cannot render
/// it.
async fn themed_page(
    state: &AppState,
    session: &Session,
    path: &str,
    destination: Option<&str>,
    marker: &ErrorPage,
) -> Option<String> {
    let viewer = crate::routes::helpers::get_user_context(session, state).await;
    let (home, home_label) = home_for(path);
    let title = title_for(marker.status);

    let mut context = tera::Context::new();
    context.insert("status", &marker.status.as_u16());
    context.insert("error_title", title);
    context.insert("message", &marker.message);
    context.insert("home", home);
    context.insert("home_label", home_label);
    // An access-denied page offers nobody signed in the login form, posting to
    // `/user/login` with the page they asked for as its destination.
    if marker.status == StatusCode::FORBIDDEN && !viewer.authenticated {
        let destination = destination.filter(|d| crate::routes::auth::is_safe_destination(d));
        context.insert(
            "login",
            &serde_json::json!({
                "csrf_token": crate::form::csrf::generate_csrf_token(session).await,
                "destination": destination,
            }),
        );
    }
    let admin_path = crate::theme::ThemeEngine::is_admin_path(path);
    context.insert("admin_path", &admin_path);
    crate::routes::helpers::inject_site_context_for(state, session, &viewer, &mut context, path)
        .await;
    if admin_path {
        let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
        enabled.sort();
        context.insert(
            "admin_nav",
            &crate::routes::admin_nav::sidebar(&viewer, &enabled, path),
        );
        context.insert("enabled_plugins", &enabled);
    }

    let specific = format!("error--{}", marker.status.as_u16());
    let theme = state.theme();
    let template = theme.resolve_template(&[specific.as_str(), "error"])?;
    let content = match theme.tera().render(&template, &context) {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, template = %template, "failed to render the error page");
            return None;
        }
    };
    match theme.render_page(path, title, &content, &mut context) {
        Ok(html) => Some(html),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the error page wrapper");
            None
        }
    }
}

/// The one built-in error page, for when the theme itself cannot render.
pub fn fallback_page(marker: &ErrorPage, (home, home_label): (&str, &str)) -> String {
    let title = title_for(marker.status);
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>{title}</title></head>
<body>
<main style="max-width: 600px; margin: 100px auto; text-align: center; font-family: sans-serif;">
<h1>{title}</h1>
<p>{message}</p>
<p><a href="{home}">{home_label}</a></p>
</main>
</body></html>"#,
        message = crate::routes::helpers::html_escape(&marker.message),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn only_a_browser_accept_asks_for_html() {
        let mut headers = HeaderMap::new();
        assert!(!accepts_html(&headers));
        headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        assert!(!accepts_html(&headers));
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml,*/*;q=0.8"),
        );
        assert!(accepts_html(&headers));
    }

    #[test]
    fn the_fallback_links_home_and_never_to_script() {
        let marker = ErrorPage {
            status: StatusCode::NOT_FOUND,
            message: "<b>gone</b>".to_string(),
        };
        let page = fallback_page(&marker, home_for("/admin/content"));
        assert!(page.contains(r#"href="/admin""#), "{page}");
        assert!(!page.contains("javascript:"), "{page}");
        assert!(
            page.contains("&lt;b&gt;gone"),
            "the message is escaped: {page}"
        );
        let page = fallback_page(&marker, home_for("/blog/x"));
        assert!(page.contains(r#"href="/""#), "{page}");
    }
}
