//! The front page.
//!
//! `/` is whatever the configured front page path renders. [`with_front_page`]
//! wraps the whole application: a request for `/` (or `/{lang}`, the front page
//! read in another language) has its URI replaced with the configured path
//! before routing, and goes through the same router, middleware and handlers as
//! a request for that path would. An item, a gather listing, a plugin route, an
//! alias of any of these, and any route type added later are all served the
//! same way, with their own access checks, cache headers and query string
//! handling. This is Drupal's `system.site:page.front`: an inbound path
//! rewrite, not a renderer per route type.
//!
//! With nothing configured, an invalid path, or a path the site answers 404
//! for, `/` is the stock front page: the promoted items listing.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use axum::{
    Extension, Router,
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower::ServiceExt;
use tower_sessions::Session;

use crate::content::FilterPipeline;
use crate::middleware::language::{ResolvedLanguage, text_direction_for_language};
use crate::models::SiteConfig;
use crate::state::AppState;
use crate::tap::UserContext;

use super::helpers::{html_escape, inject_site_context};

/// How many promoted items the default front page lists.
const PROMOTED_LISTING_LIMIT: i64 = 10;

/// How long a server trusts its cached front page path without a cache
/// generation bump: the bound on staleness when Redis cannot be reached, or
/// when the value was written straight into `site_config`.
const FRONT_PAGE_TTL: Duration = Duration::from_secs(60);

/// Create the front page router: the stock front page at `/`.
pub fn router() -> Router<AppState> {
    Router::new().route("/", get(front_page))
}

/// The stock front page: the promoted items listing.
///
/// Reached when no front page is configured, or the configured one cannot be
/// served. A configured front page never reaches this handler; see
/// [`with_front_page`].
async fn front_page(
    State(state): State<AppState>,
    Extension(lang): Extension<ResolvedLanguage>,
    requested: Option<Extension<crate::middleware::RequestedPath>>,
    session: Session,
) -> Response {
    let active_language = lang.0;
    let requested_path = requested.map(|Extension(r)| r.0);

    let viewer = super::item::get_user_context(&session, &state).await;
    let content = render_promoted_listing(&state, &viewer).await;

    let mut context = tera::Context::new();
    insert_language_context(&mut context, &active_language);
    if let Some(ref requested_path) = requested_path {
        context.insert("requested_path", requested_path);
    }
    inject_site_context(&state, &session, &mut context, "/").await;

    match state
        .theme()
        .render_page("/front", "Home", &content, &mut context)
    {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the front page");
            super::helpers::render_server_error("The front page could not be displayed.")
        }
    }
}

tokio::task_local! {
    /// Set while a request for `/` is being served by the configured path.
    static FRONT_PAGE: ();
}

/// Whether the request being handled is the front page served through its
/// configured path.
///
/// A handler reached this way answers for `/`, not for its own address, which
/// matters to anything that names the page's address: the canonical link, and
/// the language alternates. Request extensions do not reach the shared render
/// helpers, so this is a task-local, set around the dispatch in
/// [`with_front_page`].
pub fn is_front_page_request() -> bool {
    FRONT_PAGE.try_with(|_| ()).is_ok()
}

/// The front page path, cached, and the application it is dispatched through.
///
/// The path is read from `site_config` once and kept until a write bumps the
/// `SiteConfig` cache generation (any server's write clears every server's
/// copy) or `FRONT_PAGE_TTL` (a minute) passes, so `/` costs no database query to find
/// out what it is.
#[derive(Default)]
pub struct FrontPageConfig {
    cached: parking_lot::RwLock<Option<(Option<String>, Instant)>>,
    /// The last path a warning was logged for, so a broken front page logs
    /// once rather than on every request for `/`.
    warned: parking_lot::Mutex<Option<String>>,
    /// The application [`with_front_page`] wraps, for checking a path before
    /// it is saved. Weak: the application holds the state that holds this.
    app: parking_lot::RwLock<Weak<Router>>,
}

impl FrontPageConfig {
    /// The configured front page path, if any, from the cache.
    pub async fn path(&self, pool: &sqlx::PgPool) -> Option<String> {
        if let Some((path, loaded)) = self.cached.read().as_ref()
            && loaded.elapsed() < FRONT_PAGE_TTL
        {
            return path.clone();
        }

        match SiteConfig::front_page(pool).await {
            Ok(path) => {
                *self.cached.write() = Some((path.clone(), Instant::now()));
                path
            }
            Err(e) => {
                // Not cached: a database that cannot be read now may be
                // readable on the next request.
                tracing::warn!(error = %e, "failed to read the front page setting");
                None
            }
        }
    }

    /// Forget the cached path, so the next request reads it again.
    pub fn clear(&self) {
        *self.cached.write() = None;
        *self.warned.lock() = None;
    }

    /// Log `message` for `path`, once per configured path.
    fn warn_once(&self, path: &str, message: &str) {
        let mut warned = self.warned.lock();
        if warned.as_deref() != Some(path) {
            tracing::warn!(front_page = %path, "{message}; serving the default front page");
            *warned = Some(path.to_string());
        }
    }

    /// The status an anonymous `GET` of `path` answers with, through the whole
    /// application, or `None` when there is no application to ask (a state
    /// built without [`with_front_page`]).
    ///
    /// What the admin form uses to tell whether a path resolves: the router is
    /// the only thing that knows every route, so it is asked rather than
    /// second-guessed with a list of route types.
    pub(crate) async fn probe(&self, path: &str) -> Option<StatusCode> {
        let app = self.app.read().upgrade()?;
        let mut request = Request::get(path)
            .header(axum::http::header::ACCEPT, "text/html")
            .body(Body::empty())
            .ok()?;
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                0,
            ))));
        let response = app.as_ref().clone().oneshot(request).await;
        Some(response.unwrap_or_else(|err| match err {}).status())
    }
}

/// Serve `/` as the configured front page path.
///
/// Wraps the finished application (every layer included), so the rewrite
/// happens before routing: axum matches a route before any `Router::layer`
/// middleware runs, so a rewrite inside the application could not change which
/// handler answers. The request is otherwise untouched — method, headers,
/// cookies and query string — and the response is the path's own, status and
/// cache headers included.
///
/// A path the site answers 404 for at `/` (a deleted item, a removed gather)
/// falls back to the stock front page, with one warning in the log.
pub fn with_front_page(state: AppState, app: Router) -> Router {
    let app = Arc::new(app);
    *state.front_page().app.write() = Arc::downgrade(&app);
    Router::new().fallback(move |request: Request| {
        let state = state.clone();
        let app = app.clone();
        async move { dispatch(&state, &app, request).await }
    })
}

async fn forward(app: &Router, request: Request) -> Response {
    app.clone()
        .oneshot(request)
        .await
        .unwrap_or_else(|err| match err {})
}

async fn dispatch(state: &AppState, app: &Router, request: Request) -> Response {
    let Some(prefix) = front_page_prefix(state, request.uri().path()) else {
        return forward(app, request).await;
    };
    let config = state.front_page();
    let Some(configured) = config.path(state.db()).await else {
        return forward(app, request).await;
    };
    let Some(target) = local_front_path(&configured) else {
        config.warn_once(&configured, "front page is not a local path");
        return forward(app, request).await;
    };
    let Some(uri) = front_page_uri(&prefix, target, request.uri().query()) else {
        config.warn_once(&configured, "front page is not a valid path");
        return forward(app, request).await;
    };

    // A body cannot be replayed, so only a request without one can fall back
    // to the stock page after the target has answered.
    let retry = matches!(*request.method(), Method::GET | Method::HEAD).then(|| {
        let mut stock = Request::new(Body::empty());
        *stock.method_mut() = request.method().clone();
        *stock.uri_mut() = request.uri().clone();
        *stock.version_mut() = request.version();
        *stock.headers_mut() = request.headers().clone();
        *stock.extensions_mut() = request.extensions().clone();
        stock
    });

    let (mut parts, body) = request.into_parts();
    // The address as asked for, so the menu marks "Home" active: language
    // negotiation records it only when nothing has yet.
    parts.extensions.insert(crate::middleware::RequestedPath(
        parts.uri.path().to_string(),
    ));
    parts.uri = uri;

    let response = FRONT_PAGE
        .scope((), forward(app, Request::from_parts(parts, body)))
        .await;

    match retry {
        Some(stock) if response.status() == StatusCode::NOT_FOUND => {
            config.warn_once(target, "front page answered 404 Not Found");
            forward(app, stock).await
        }
        _ => response,
    }
}

/// The language prefix of a request for the front page: `""` for `/`, and
/// `/{lang}` for `/{lang}` or `/{lang}/` in a known language (the default
/// included: language negotiation serves or redirects it, B161).
/// `None` for any other path.
fn front_page_prefix(state: &AppState, path: &str) -> Option<String> {
    if path == "/" {
        return Some(String::new());
    }
    let language = path.strip_prefix('/')?;
    let language = language.strip_suffix('/').unwrap_or(language);
    state
        .known_languages()
        .iter()
        .any(|l| l == language)
        .then(|| format!("/{language}"))
}

/// The URI `/` is served as: the language prefix, the configured path, and the
/// query string of the request joined onto any the path carries.
fn front_page_uri(prefix: &str, target: &str, query: Option<&str>) -> Option<Uri> {
    let mut uri = format!("{prefix}{target}");
    if let Some(query) = query.filter(|q| !q.is_empty()) {
        // The configured path may carry a query of its own — a gather with a
        // preset filter, say — so join rather than assume.
        uri.push(if target.contains('?') { '&' } else { '?' });
        uri.push_str(query);
    }
    uri.parse().ok()
}

/// Whether a path is a local absolute path this site can serve.
///
/// Local means absolute, with no scheme and no host, so that a site's front
/// page can never be aimed at another origin. Shared with the admin form so
/// that what is saved and what is served agree on what a path is.
pub(crate) fn is_local_path(path: &str) -> bool {
    // Absolute local path only. This rejects "https://example.com/" and
    // "example.com/path" outright, and "//example.com" is protocol-relative:
    // a browser reads it as another host.
    if !path.starts_with('/') || path.starts_with("//") {
        return false;
    }

    // Browsers fold a backslash into a slash when parsing a URL, so "/\evil"
    // is protocol-relative by another spelling.
    if path.contains('\\') {
        return false;
    }

    // Whitespace and control characters have no place in a path, and a browser
    // may strip them before parsing what is left.
    !path
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '"' || c == '<' || c == '>')
}

/// Validate a configured front page path and return it for serving.
///
/// `/` is rejected along with anything non-local: it is the front page itself,
/// not a path it can be served as.
pub(crate) fn local_front_path(configured: &str) -> Option<&str> {
    let path = configured.trim();

    if !is_local_path(path) || path == "/" {
        return None;
    }

    Some(path)
}

/// Record the language this response was negotiated in.
///
/// Set before `inject_site_context`, which only supplies the site default when
/// the context does not already carry a language. The route knows the answer;
/// the helper only has a fallback.
fn insert_language_context(context: &mut tera::Context, active_language: &str) {
    context.insert("active_language", active_language);
    context.insert(
        "text_direction",
        text_direction_for_language(active_language),
    );
}

/// Render promoted items listing HTML.
///
/// Asks for promoted items directly, with paging, rather than filtering a page
/// of published items: promotion is what decides membership of this list, so
/// it has to decide the query too.
///
/// The list is the viewer's: every item goes through the item access path
/// ([`crate::content::ItemService::filter_page_for_view`]), so a viewer
/// without `access content`, or one a plugin denies an item to, is not shown
/// it, and a field they may not view is not used for its teaser.
async fn render_promoted_listing(state: &AppState, viewer: &UserContext) -> String {
    // A listing of items, same as a gather query's (B303): promoting,
    // demoting or editing any item should purge this page from a CDN, so it
    // carries the `items` tag rather than one `item:<uuid>` tag per row
    // (`list_promoted` is a raw query, not `ItemService::load`, so nothing
    // else records it).
    crate::edge_cache::tags::record_items();

    // Over-fetch so a few items the viewer may not see do not leave the
    // listing short.
    let candidates = state
        .items()
        .list_promoted(PROMOTED_LISTING_LIMIT * 3, 0)
        .await
        .unwrap_or_default();
    let promoted = state
        .items()
        .filter_page_for_view(candidates, viewer, "view", PROMOTED_LISTING_LIMIT as usize)
        .await;

    if promoted.is_empty() {
        return String::new();
    }

    let mut html = String::from("<div class=\"front-listing\">");

    for item in &promoted {
        html.push_str("<div class=\"blog-teaser\">");
        html.push_str(&format!(
            "<h2 class=\"blog-teaser__title\"><a href=\"/item/{}\">{}</a></h2>",
            item.id,
            html_escape(&item.title)
        ));

        let date = chrono::DateTime::from_timestamp(item.created, 0)
            .map(|dt| dt.format("%B %-d, %Y").to_string())
            .unwrap_or_default();
        if !date.is_empty() {
            html.push_str(&format!(
                "<div class=\"blog-teaser__meta\"><time>{date}</time></div>"
            ));
        }

        // Render body field summary if available. The field goes by two names
        // (BL-21), and a promoted blog post carries the one this renderer did
        // not read, so its teaser came out empty on the front page.
        if let Some(field) = crate::content::body_field::body_field_name(&item.fields) {
            let body =
                crate::content::body_field::field_text(&item.fields, field).unwrap_or_default();
            html.push_str(&format!(
                "<div class=\"blog-teaser__summary\">{}</div>",
                teaser(&body)
            ));
        }

        html.push_str(&format!(
            "<a href=\"/item/{}\" class=\"blog-teaser__read-more\">Read more &rarr;</a>",
            item.id
        ));
        html.push_str("</div>");
    }

    html.push_str("</div>");
    html
}

/// How many characters of a body a teaser shows.
const TEASER_CHARS: usize = 200;

/// A body's teaser: its plain text, cut to [`TEASER_CHARS`] characters, then
/// escaped. Cutting the filtered HTML instead could end inside a tag or an
/// entity, and left an element open across the rest of the page.
fn teaser(body: &str) -> String {
    let text = crate::content::page_meta::plain_text(body);
    let cut = if text.chars().count() > TEASER_CHARS {
        let truncated: String = text.chars().take(TEASER_CHARS).collect();
        format!("{}...", truncated.trim_end())
    } else {
        text
    };
    FilterPipeline::for_format("plain_text").process(&cut)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn any_internal_path_is_a_valid_front_page() {
        // Nothing here is a route type the front page knows about — that is
        // the point. A gather alias, a plugin route and an aliased path are
        // all just paths.
        for path in [
            "/devices/online",
            "/blog",
            "/item/0192f0c0-0000-7000-8000-000000000000",
            "/topics/rust",
            "/a/deeply/nested/path",
            "/gather/devices?status=online",
            "/percent%20encoded",
        ] {
            assert_eq!(local_front_path(path), Some(path), "rejected {path}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(local_front_path("  /blog\n"), Some("/blog"));
    }

    #[test]
    fn external_and_malformed_paths_are_rejected() {
        for path in [
            "https://example.com/",
            "http://example.com/blog",
            "//example.com/blog",
            "example.com/blog",
            "blog",
            "/\\example.com",
            "/blog\\..",
            "/blog with space",
            "/blog\r\nLocation: https://example.com",
            "",
        ] {
            assert_eq!(local_front_path(path), None, "accepted {path:?}");
        }
    }

    #[test]
    fn the_front_page_itself_is_not_a_front_page_target() {
        assert_eq!(local_front_path("/"), None);
        assert!(is_local_path("/"), "\"/\" is still a local path");
    }

    #[test]
    fn the_query_string_is_carried_to_the_target() {
        let uri = front_page_uri("", "/devices/online", Some("page=2")).unwrap();
        assert_eq!(uri, "/devices/online?page=2");
    }

    #[test]
    fn a_configured_query_string_is_joined_not_overwritten() {
        let uri = front_page_uri("", "/gather/devices?status=online", Some("page=2")).unwrap();
        assert_eq!(uri, "/gather/devices?status=online&page=2");
    }

    #[test]
    fn without_a_query_string_the_target_is_the_bare_path() {
        assert_eq!(
            front_page_uri("", "/devices/online", None).unwrap(),
            "/devices/online"
        );
        assert_eq!(
            front_page_uri("", "/devices/online", Some("")).unwrap(),
            "/devices/online"
        );
    }

    #[test]
    fn a_language_prefix_is_kept() {
        assert_eq!(
            front_page_uri("/it", "/blog", Some("page=2")).unwrap(),
            "/it/blog?page=2"
        );
    }
}
