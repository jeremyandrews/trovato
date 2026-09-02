//! Front page route handler.

use axum::{
    Extension, Router,
    extract::{RawQuery, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::FilterPipeline;
use crate::middleware::language::{ResolvedLanguage, text_direction_for_language};
use crate::models::{Item, SiteConfig};
use crate::state::AppState;

use super::helpers::{html_escape, inject_site_context};

/// How many promoted items the default front page lists.
const PROMOTED_LISTING_LIMIT: i64 = 10;

/// Create the front page router.
pub fn router() -> Router<AppState> {
    Router::new().route("/", get(front_page))
}

/// Front page handler.
///
/// If `site_front_page` names an item (`/item/{uuid}`), that item is rendered
/// inline at `/`. Any other configured path redirects to itself, so whichever
/// handler owns that route serves it — a gather alias, a plugin route, an
/// aliased path, or a route type that does not exist yet. Nothing here knows
/// about any particular route.
///
/// With nothing configured (or a configured path that cannot be served), the
/// promoted items listing is shown.
async fn front_page(
    State(state): State<AppState>,
    Extension(lang): Extension<ResolvedLanguage>,
    requested: Option<Extension<crate::middleware::RequestedPath>>,
    session: Session,
    RawQuery(query): RawQuery,
) -> Response {
    let active_language = lang.0;
    let requested_path = requested.map(|Extension(r)| r.0);

    // Check for a configured front page
    if let Ok(Some(front_path)) = SiteConfig::front_page(state.db()).await
        && let Some(response) = render_configured_front_page(
            &state,
            &session,
            &front_path,
            query.as_deref(),
            &active_language,
            requested_path.as_deref(),
        )
        .await
    {
        return response;
    }

    // Fall back to promoted items listing
    let content = render_promoted_listing(&state).await;

    let mut context = tera::Context::new();
    insert_language_context(&mut context, &active_language);
    if let Some(ref requested_path) = requested_path {
        context.insert("requested_path", requested_path);
    }
    inject_site_context(&state, &session, &mut context, "/").await;

    let html = state
        .theme()
        .render_page("/front", "Home", &content, &mut context)
        .unwrap_or_else(|_| format!("<html><body>{content}</body></html>"));

    Html(html).into_response()
}

/// Serve the configured front page, or `None` to fall through to the default.
///
/// An `/item/{uuid}` path is rendered inline; anything else redirects.
async fn render_configured_front_page(
    state: &AppState,
    session: &Session,
    front_path: &str,
    query: Option<&str>,
    active_language: &str,
    requested_path: Option<&str>,
) -> Option<Response> {
    let path = local_front_path(front_path)?;

    if let Some(item_id) = path
        .strip_prefix("/item/")
        .and_then(|id_str| Uuid::parse_str(id_str).ok())
    {
        return render_front_page_item(state, session, item_id, active_language, requested_path)
            .await
            .map(|html| Html(html).into_response());
    }

    redirect_to_front_path(path, query)
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
/// `/` is rejected along with anything non-local: it is this handler's own
/// route, and redirecting it to itself would loop.
fn local_front_path(configured: &str) -> Option<&str> {
    let path = configured.trim();

    if !is_local_path(path) || path == "/" {
        return None;
    }

    Some(path)
}

/// Redirect `/` to the configured path, preserving any query string.
///
/// Temporary, not permanent: the front page is a setting an operator can
/// change, and a cached permanent redirect would outlive the change.
fn redirect_to_front_path(path: &str, query: Option<&str>) -> Option<Response> {
    let location = match query {
        // The configured path may carry a query of its own — a gather alias
        // with a preset filter, say — so join rather than assume.
        Some(q) if !q.is_empty() => {
            let separator = if path.contains('?') { '&' } else { '?' };
            format!("{path}{separator}{q}")
        }
        _ => path.to_string(),
    };

    // Built rather than unwrapped: a header value is the last place to trust a
    // string that reached us through a URL.
    let location = HeaderValue::from_str(&location).ok()?;

    Some(
        (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, location)],
        )
            .into_response(),
    )
}

/// Render an item inline as the front page.
async fn render_front_page_item(
    state: &AppState,
    session: &Session,
    item_id: Uuid,
    active_language: &str,
    requested_path: Option<&str>,
) -> Option<String> {
    // Use load_for_view to invoke tap hooks and check access.
    //
    // The same user context the item route itself builds — real permissions,
    // loaded from the database, including the anonymous role's. A hard-coded
    // permission list here meant an item front page was access-checked against
    // permissions nobody actually holds: anonymous visitors were handed none at
    // all, so a default install (whose anonymous role does have "access
    // content") silently fell through to the promoted listing.
    let user = super::item::get_user_context(session, state).await;
    let (mut item, render_outputs) = state.items().load_for_view(item_id, &user).await.ok()??;

    if !item.is_published() {
        return None;
    }

    // Overlay the translation, the same way the item route does. An item shown
    // at `/` is still that item: a translation configured for it is content,
    // not decoration, and skipping the overlay here made it a setting nothing
    // ever read.
    if active_language != state.default_language() {
        super::helpers::apply_translation_overlay(state.items(), &mut item, active_language).await;

        // The overlay may re-materialize a field `load_for_view` dropped (a
        // translation can carry a restricted field's value). Re-apply the
        // field-access filter so the SSR output never leaks it.
        let names: Vec<String> = item
            .fields
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        if !names.is_empty() {
            let allowed: std::collections::HashSet<String> = state
                .items()
                .accessible_fields(&user, &item.item_type, &names, "view")
                .await
                .into_iter()
                .collect();
            if let Some(obj) = item.fields.as_object_mut() {
                obj.retain(|k, _| allowed.contains(k));
            }
        }
    }

    // Render item fields and plugin outputs
    let mut children_html = render_item_fields(&item, state);
    for output in render_outputs {
        children_html.push_str(&output);
    }

    // Resolve item template
    let suggestions = [
        format!("elements/item--{}--{}", item.item_type, item.id),
        format!("elements/item--{}", item.item_type),
        "elements/item".to_string(),
    ];
    let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();
    let template = state
        .theme()
        .resolve_template(&suggestion_refs)
        .unwrap_or_else(|| "elements/item.html".to_string());

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("children", &children_html);
    insert_language_context(&mut context, active_language);

    let item_html = state.theme().tera().render(&template, &context).ok()?;

    // Wrap in page layout
    if let Some(requested_path) = requested_path {
        context.insert("requested_path", requested_path);
    }
    inject_site_context(state, session, &mut context, "/").await;

    // Every language the front page can be read in, and where, the same facts
    // the item route gives every other page: `available_translations` for a
    // switcher, `hreflang_links` for a crawler, left out entirely when there is
    // only one language to name.
    //
    // The address is `/`, not the item's own alias. The item is being read as
    // the front page, and a switcher that moves a reader to `/it/why` has moved
    // them off the front page they asked for. `/it/` is how the language prefix
    // negotiator reads that back.
    let translations = super::helpers::available_translations(state, item_id, "/").await;
    if translations.len() > 1 {
        context.insert(
            "hreflang_links",
            &super::helpers::build_hreflang_links(&translations, state.default_language()),
        );
    }
    context.insert("available_translations", &translations);

    state
        .theme()
        .render_page("/front", &item.title, &item_html, &mut context)
        .ok()
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
async fn render_promoted_listing(state: &AppState) -> String {
    let promoted = state
        .items()
        .list_promoted(PROMOTED_LISTING_LIMIT, 0)
        .await
        .unwrap_or_default();

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

        // Render body field summary if available
        if let Some(body) = item
            .fields
            .get("body")
            .and_then(|f| f.get("value"))
            .and_then(|v| v.as_str())
        {
            let format = item
                .fields
                .get("body")
                .and_then(|f| f.get("format"))
                .and_then(|v| v.as_str())
                .unwrap_or("plain_text");
            let filtered = FilterPipeline::for_format_safe(format).process(body);
            // Truncate for teaser (char-boundary safe)
            let summary = if filtered.chars().count() > 200 {
                let truncated: String = filtered.chars().take(200).collect();
                format!("{truncated}...")
            } else {
                filtered
            };
            html.push_str(&format!(
                "<div class=\"blog-teaser__summary\">{summary}</div>"
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

/// Render item fields to HTML.
fn render_item_fields(item: &Item, state: &AppState) -> String {
    let mut html = String::new();

    if let Some(fields) = item.fields.as_object() {
        for (name, value) in fields {
            // PageBuilder field: Puck JSON with root + content keys
            if value.get("root").is_some() && value.get("content").is_some() {
                match state.theme().render_page_builder_content(value) {
                    Ok(rendered) => {
                        html.push_str(&format!(
                            "<div class=\"field field--page-builder field-{}\">{}</div>",
                            html_escape(name),
                            rendered
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(field = %name, error = %e, "failed to render page builder field on front page");
                    }
                }
                continue;
            }

            // Text field with {value, format}
            if let Some(text_val) = value.get("value").and_then(|v| v.as_str()) {
                let format = value
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plain_text");
                let filtered = FilterPipeline::for_format_safe(format).process(text_val);
                let safe_name = html_escape(name);
                html.push_str(&format!(
                    "<div class=\"field field-{safe_name}\">{filtered}</div>"
                ));
            }
        }
    }

    html
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
        // Otherwise "/" redirects to "/" forever.
        assert_eq!(local_front_path("/"), None);
        assert!(is_local_path("/"), "\"/\" is still a local path");
    }

    #[test]
    fn redirect_is_temporary_and_carries_the_query_string() {
        let response = redirect_to_front_path("/devices/online", Some("page=2"))
            .expect("valid path redirects");

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/devices/online?page=2")
        );
    }

    #[test]
    fn a_configured_query_string_is_joined_not_overwritten() {
        let response = redirect_to_front_path("/gather/devices?status=online", Some("page=2"))
            .expect("valid path redirects");

        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/gather/devices?status=online&page=2")
        );
    }

    #[test]
    fn redirect_without_a_query_string_is_the_bare_path() {
        let response =
            redirect_to_front_path("/devices/online", None).expect("valid path redirects");

        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/devices/online")
        );
    }
}
