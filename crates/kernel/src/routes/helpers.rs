//! Shared route helpers for page rendering.

use std::collections::HashSet;

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use tower_sessions::Session;
use uuid::Uuid;

use serde::{Deserialize, Serialize};

use crate::models::stage::LIVE_STAGE_ID;
use crate::models::{SiteConfig, User};
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;
use crate::tap::UserContext;

/// Standard JSON success response for API endpoints.
#[derive(Debug, Serialize)]
pub struct JsonSuccess {
    /// Whether the operation succeeded.
    pub success: bool,
    /// Human-readable success message.
    pub message: String,
}

/// Error message for invalid machine names.
///
/// Used by content type, category, and gather admin forms.
pub const MACHINE_NAME_ERROR: &str = "Machine name must start with a letter and contain only lowercase letters, numbers, and underscores.";

/// Generic form struct for POST endpoints that only need a CSRF token.
///
/// Used by delete, approve, unpublish, and similar action-only endpoints.
#[derive(Debug, Deserialize)]
pub struct CsrfOnlyForm {
    #[serde(rename = "_token")]
    pub token: String,
}

/// The session's signed-in user, if there is one: the user the session names,
/// provided that user still exists and is active.
///
/// The one place a request handler reads `SESSION_USER_ID` to decide who is
/// asking. A session naming a blocked user is deleted here, so every guard built
/// on this signs a blocked account out the same way. A session naming a user
/// who no longer exists is simply nobody.
pub async fn session_user(state: &AppState, session: &Session) -> Option<User> {
    let id: Uuid = session.get(SESSION_USER_ID).await.ok().flatten()?;
    let user = state.users().find_by_id(id).await.ok().flatten()?;
    if !user.is_active() {
        // `flush`, not `delete`: see `routes::auth::logout`. A session only
        // deleted from the store is saved again by the session layer if this
        // request writes to it, and that save fails on the missing key.
        let _ = session.flush().await;
        return None;
    }
    Some(user)
}

/// Whether `user` holds `permission`; an administrator holds every permission.
async fn holds(state: &AppState, user: &User, permission: &str) -> bool {
    user.is_admin
        || state
            .permissions()
            .user_has_permission(user, permission)
            .await
            .unwrap_or(false)
}

/// The HTML refusal for a signed-in user who may not do this.
fn html_forbidden() -> Response {
    render_forbidden("You do not have permission to view this page.")
}

/// The HTML refusal for nobody signed in: off to the login form.
fn html_login() -> Response {
    Redirect::to("/user/login").into_response()
}

/// Require an authenticated, active user, or redirect to login.
///
/// Returns the [`User`] if one is logged in and active (`status=1`).
/// Destroys the session and redirects to login if the user is blocked.
///
/// This is the HTML half of the one refusal rule; [`require_login_json`] is
/// the JSON half.
pub async fn require_login(state: &AppState, session: &Session) -> Result<User, Response> {
    session_user(state, session).await.ok_or_else(html_login)
}

/// Require an authenticated, active **admin** user, or redirect/reject.
///
/// Returns the admin [`User`] on success. Redirects to `/user/login` if the
/// session has no valid user or the user is blocked. Returns 403 if the user
/// exists and is active but is not an admin.
pub async fn require_admin(state: &AppState, session: &Session) -> Result<User, Response> {
    let user = require_login(state, session).await?;
    if user.is_admin {
        Ok(user)
    } else {
        Err(html_forbidden())
    }
}

/// Require an authenticated, active user with a specific permission.
///
/// Returns the [`User`] if the session user is active and has the named
/// permission. Admin users (`is_admin == true`) are implicitly granted all
/// permissions. Redirects to `/user/login` if unauthenticated or blocked.
/// Returns 403 if the user exists but lacks the permission.
pub async fn require_permission(
    state: &AppState,
    session: &Session,
    permission: &str,
) -> Result<User, Response> {
    let user = require_login(state, session).await?;
    if holds(state, &user, permission).await {
        Ok(user)
    } else {
        Err(html_forbidden())
    }
}

/// The refusal every JSON caller gets for being nobody: `401` with an
/// [`AppError`](crate::error::AppError) body.
fn json_login() -> crate::error::AppError {
    crate::error::AppError::unauthorized("Authentication required")
}

/// Require an authenticated, active user for a JSON endpoint.
///
/// The JSON half of [`require_login`]: the same question, answered with an
/// [`AppError`](crate::error::AppError) `401` instead of a redirect, because a
/// script cannot follow a redirect to a login form.
pub async fn require_login_json(
    state: &AppState,
    session: &Session,
) -> Result<User, crate::error::AppError> {
    session_user(state, session).await.ok_or_else(json_login)
}

/// Require an authenticated, active user with a specific permission for JSON
/// API endpoints.
///
/// `401` when nobody is signed in, `403` when the signed-in user lacks the
/// permission — the same rule [`require_permission`] applies to HTML, in
/// [`AppError`](crate::error::AppError) form.
pub async fn require_permission_json(
    state: &AppState,
    session: &Session,
    permission: &str,
) -> Result<User, crate::error::AppError> {
    let user = require_login_json(state, session).await?;
    if holds(state, &user, permission).await {
        Ok(user)
    } else {
        Err(crate::error::AppError::forbidden(format!(
            "Permission required: {permission}"
        )))
    }
}

/// The refusal for `viewer`, decided by the one rule: `401` when nobody is
/// signed in, `403` when someone is and may not do this.
///
/// For a handler that has already loaded the viewer's [`UserContext`] and asks
/// a question no guard above can ask for it, such as "may this viewer create
/// this type", rather than a guard reading the session a second time.
pub fn refusal_for(viewer: &UserContext, reason: impl Into<String>) -> crate::error::AppError {
    if viewer.authenticated {
        crate::error::AppError::forbidden(reason)
    } else {
        json_login()
    }
}

/// Inject site-wide context variables into a Tera context.
///
/// Adds: `site_name`, `site_slogan`, `menus`, `user_authenticated`, `user_name`
/// (signed-in only), `current_path`,
/// `header_tiles`, `navigation_tiles`, `sidebar_tiles`, `footer_tiles`
///
/// `active_language` and `text_direction` are a *fallback*, not a value: they
/// are filled in only when the context does not already carry them, so a route
/// that knows the language the response was negotiated in keeps its own value.
///
/// **A route that knows the language must insert it before calling this.** The
/// fallback rule alone made the value order-independent, which is not the same
/// as making the call order-independent: this function *reads* the language back
/// out of the context and builds the localized menus from it. A language
/// inserted afterwards still reaches `<html lang>` and reaches nothing derived
/// from it, which renders a page that announces one language over navigation
/// built for another. Anything added here that reads `active_language` inherits
/// that requirement.
///
/// The `path` parameter is the current request path, used for sidebar tile
/// visibility filtering.
///
/// The viewer is loaded once through [`get_user_context`] and drives three
/// things: which plugin-registered menus appear in `menus` (see
/// [`MenuRegistry::root_menus_for`](crate::menu::MenuRegistry::root_menus_for)),
/// the `user_authenticated` / `user_can_access_admin` flags, and the roles that tile
/// visibility is filtered by. `user_authenticated` therefore reflects whether
/// the session's user actually exists, not merely whether the session names
/// one — a session pointing at a deleted user now renders as logged out
/// instead of showing a logout form for an account that is gone.
///
/// It also becomes `viewer` (see [`Viewer`](crate::theme::Viewer)): the
/// viewer's id, whether they are signed in, and their role names, which is all
/// a template is told about who is looking.
///
/// A route that has already loaded the viewer calls
/// [`inject_site_context_for`] instead, so one request loads it once.
pub async fn inject_site_context(
    state: &AppState,
    session: &Session,
    context: &mut tera::Context,
    path: &str,
) {
    let viewer = get_user_context(session, state).await;
    inject_site_context_for(state, session, &viewer, context, path).await;
}

/// [`inject_site_context`] for a route that already holds the viewer's
/// [`UserContext`].
pub async fn inject_site_context_for(
    state: &AppState,
    session: &Session,
    viewer: &UserContext,
    context: &mut tera::Context,
    path: &str,
) {
    // Load site name and slogan in a single query
    let all_config = SiteConfig::all(state.db()).await.unwrap_or_default();
    let site_name = all_config
        .get("site_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Trovato")
        .to_string();
    let site_slogan = all_config
        .get("site_slogan")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    context.insert("site_name", &site_name);
    context.insert("site_slogan", &site_slogan);

    // RSS autodiscovery. Resolved at startup, so this is a slice read rather
    // than a scan of the gather registry per render.
    context.insert("feeds", state.feed_links());

    // Language context variables.
    //
    // The contract: `active_language` and `text_direction` belong to the route,
    // which knows the language the response was actually negotiated in. What is
    // set here is only a fallback for a route that does not set them, so a value
    // already in the context wins and is never overwritten. Inserting them
    // unconditionally made the order of the route's own inserts decide the
    // outcome: a route that set the language before calling this helper had it
    // silently replaced by the site default, and every translated page it served
    // rendered with the wrong `<html lang>`.
    context.insert("available_languages", state.known_languages());
    context.insert("default_language", state.default_language());
    if !context.contains_key("active_language") {
        context.insert("active_language", state.default_language());
    }
    if !context.contains_key("text_direction") {
        context.insert(
            "text_direction",
            crate::middleware::language::text_direction_for_language(state.default_language()),
        );
    }

    // The language the menus below are built for. Read back out of the context
    // rather than taken from `state`, because the line above has just settled
    // whose value wins.
    let active_language = context
        .get("active_language")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| state.default_language())
        .to_string();

    // The address as asked for, kept beside `current_path` rather than instead
    // of it. A route that knows the original sets it before calling this; the
    // fallback is `current_path`, which is right for every route that is never
    // reached through an alias or a language prefix.
    if !context.contains_key("requested_path") {
        context.insert("requested_path", &path);
    }

    // A page served as the front page is canonically `/`, whatever path it is
    // configured as. A route with head metadata of its own (the item route)
    // sets it after this and names `/` itself.
    if crate::routes::front::is_front_page_request() && !context.contains_key("page_meta") {
        context.insert(
            "page_meta",
            &crate::content::PageMeta::for_path(&state.runtime().site_url, "/"),
        );
    }

    // The viewer, with real permissions, loaded once by the caller and used for
    // everything below: navigation visibility, the authentication flags, tile
    // roles, and the template `viewer`.
    context.insert("viewer", &crate::theme::Viewer::of(viewer));

    // Navigation, from the cached config menus and the menu registry, filtered
    // for this viewer and matched against the address as asked for. Every key a
    // theme has always had keeps its shape; `menus_by_name` and
    // `menus_by_parent` are the same links as trees.
    //
    // `main_menu_owned` says whether the site has a `menu_link` main menu at
    // all, which is what decides that the plugin menu is not drawn. Without it
    // a viewer who may follow none of the links would get the plugin menu the
    // site replaced, rather than the site's menu with nothing in it.
    let requested_path = context
        .get("requested_path")
        .and_then(|v| v.as_str())
        .unwrap_or(path)
        .to_string();
    let site_menus =
        crate::menu::navigation::site_menus(state, viewer, &active_language, &requested_path).await;
    context.insert("main_menu_owned", &site_menus.main_menu_owned);
    context.insert("main_menu", &site_menus.main_menu);
    context.insert("footer_menu", &site_menus.footer_menu);
    context.insert("menus", &site_menus.menus);
    context.insert("menus_tree", &site_menus.menus_tree);
    context.insert("menus_by_name", &site_menus.menus_by_name);
    context.insert("menus_by_parent", &site_menus.menus_by_parent);

    // User authentication status and roles for tile visibility
    context.insert("user_authenticated", &viewer.authenticated);

    // The signed-in user's account name, so a theme can say who is signed in
    // on every page, not only the profile. Taken from the user record the
    // viewer was built from, so it costs no query; absent (not empty) for an
    // anonymous visitor, so `{% if user_name is defined %}` is the test. The
    // name and nothing else: no email, roles or permissions travel this way.
    if viewer.authenticated
        && let Some(name) = viewer.name()
    {
        context.insert("user_name", name);
    }

    // Generate CSRF token for authenticated users (used by logout form in page.html)
    if viewer.authenticated {
        let csrf_token = crate::form::csrf::generate_csrf_token(session).await;
        context.insert("csrf_token", &csrf_token);
    }

    let mut user_roles = vec!["anonymous user".to_string()];
    if viewer.authenticated {
        user_roles.push("authenticated user".to_string());
    }
    if viewer.is_admin() {
        user_roles.push("administrator".to_string());
    }
    // Whether to show the "Admin" link, so it asks the question `/admin` itself
    // asks — `require_permission(.., "access administration pages")` — rather
    // than whether the viewer is a site administrator. A role that cannot enter
    // the section is not shown the door, and one that can is not made to guess
    // the address.
    context.insert(
        "user_can_access_admin",
        &viewer.can("access administration pages"),
    );

    // The AI Assistant's launcher context, read out of the site config already
    // loaded above rather than with a query of its own — a launcher that costs a
    // round trip on every page render is not a launcher worth having.
    //
    // `assistant_enabled` gates the partial: a site that has not turned the
    // assistant on never advertises it. `assistant_scopes` is name-to-label for
    // the scopes *this viewer* may open, so a template can offer one without
    // asking whether the person is allowed.
    let assistant_config: crate::services::ai_assistant::AssistantConfig = all_config
        .get(crate::services::ai_assistant::CONFIG_KEY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    context.insert("assistant_enabled", &assistant_config.enabled);
    let mut assistant_scopes: std::collections::BTreeMap<String, String> = Default::default();
    if assistant_config.enabled
        && (viewer.is_admin()
            || (viewer.has_permission("use ai") && viewer.has_permission("use ai assistant")))
    {
        for registered in state.assistant_scopes().scopes() {
            let scope = &registered.scope;
            if !assistant_config.scope_enabled(&scope.name) {
                continue;
            }
            if viewer.is_admin()
                || scope.permission.is_empty()
                || viewer.has_permission(&scope.permission)
            {
                assistant_scopes.insert(scope.name.clone(), scope.label.clone());
            }
        }
    }
    context.insert("assistant_scopes", &assistant_scopes);

    // Load tiles for all regions filtered by request path and user roles, each
    // rendered for this viewer, in this language, from the menus built above.
    let tile_render = crate::services::tile::TileRenderContext {
        state,
        viewer,
        language: &active_language,
        roles: &user_roles,
        menus_by_name: &site_menus.menus_by_name,
    };
    for region in &["header", "navigation", "sidebar", "footer"] {
        let region_html = state
            .tiles()
            .render_region(region, LIVE_STAGE_ID, path, &tile_render)
            .await
            .unwrap_or_default();
        context.insert(format!("{region}_tiles"), &region_html);
    }
    // Keep sidebar_tiles for backwards compatibility (page.html uses it)
    context.insert("current_path", &path);
}

/// Render an admin template with common context: `enabled_plugins`, and
/// `admin_nav`, the sidebar filtered to what `viewer` may open.
///
/// This is the shared implementation used by all admin route modules
/// (admin, gather_admin, plugin_admin). The `enabled_plugins` list is
/// sorted for deterministic template output. The active sidebar link is read
/// from the context's `path`.
pub async fn render_admin_template(
    state: &AppState,
    viewer: &UserContext,
    template: &str,
    mut context: tera::Context,
) -> Response {
    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    // The sidebar holds only the screens this viewer may open (BL-117).
    let path = context
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or_default()
        .to_string();
    context.insert(
        "admin_nav",
        &super::admin_nav::sidebar(viewer, &enabled, &path),
    );
    context.insert("enabled_plugins", &enabled);
    // Ensure "errors" is always available for form templates that use form::errors().
    if context.get("errors").is_none() {
        let empty: Vec<String> = Vec::new();
        context.insert("errors", &empty);
    }
    match state.theme().tera().render(template, &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template = %template, "failed to render template");
            render_server_error("This page could not be displayed.")
        }
    }
}

/// Verify a CSRF token, returning an error response on failure.
///
/// Call sites use this as:
/// ```ignore
/// if let Err(resp) = require_csrf(&session, &form.token).await {
///     return resp;
/// }
/// ```
pub async fn require_csrf(session: &Session, token: &str) -> Result<(), Response> {
    let valid = crate::form::csrf::verify_csrf_token(session, token)
        .await
        .unwrap_or(false);
    if !valid {
        Err(render_error(
            "Invalid or expired form token. Please try again.",
        ))
    } else {
        Ok(())
    }
}

/// Verify a CSRF token from the `X-CSRF-Token` header, returning a JSON error on failure.
///
/// For use with JSON API endpoints that use cookie-based session auth.
/// The client must include the token in a custom header rather than a form field.
///
/// A request authenticated by an API token passes without one. CSRF exists
/// because a browser attaches a cookie to a cross-site request by itself; no
/// browser attaches a bearer token, so a token request has nothing to forge.
/// The decision rests on
/// [`crate::middleware::api_token::is_token_authenticated`], which only the
/// token middleware can make true and which no session can carry, so a
/// cookie-authenticated request is refused exactly as before.
pub async fn require_csrf_header(
    session: &Session,
    headers: &axum::http::HeaderMap,
) -> Result<(), crate::error::AppError> {
    if crate::middleware::api_token::is_token_authenticated() {
        return Ok(());
    }
    let token = headers
        .get("X-CSRF-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let valid = crate::form::csrf::verify_csrf_token(session, token)
        .await
        .unwrap_or(false);
    if !valid {
        Err(crate::error::AppError::forbidden(
            "Invalid or missing CSRF token. Include X-CSRF-Token header.",
        ))
    } else {
        Ok(())
    }
}

/// The field every Trovato form carries its CSRF token in.
///
/// `form/service.rs` reads it, forty templates write it, and every hand-written
/// form struct renames a field to it. A plugin serving its own HTML form has to
/// be able to use the same name, which is what [`require_csrf_header_or_field`]
/// is for.
pub const CSRF_FORM_FIELD: &str = "_token";

/// Verify a CSRF token from the `X-CSRF-Token` header, or from a `_token` field
/// in a form-urlencoded body.
///
/// This exists because **a plain HTML `<form>` cannot set a header.** A caller
/// posting `application/x-www-form-urlencoded` without JavaScript has exactly
/// one place to put a token, and it is the body;
/// [`require_csrf_header`] cannot read it, so a plugin serving its own form was
/// refused with 403 no matter what it rendered. The check itself is unchanged:
/// both paths end in `form::csrf::verify_csrf_token`, the same single-use,
/// session-bound, hour-limited verification every kernel form uses.
///
/// The header is tried first, so a JavaScript client's behaviour is exactly what
/// it was. Trying it costs nothing when it is absent: `verify_csrf_token`
/// refuses an empty token before touching the session, and a token that does not
/// match is not consumed, so the fallback still has a live token to check.
///
/// The body is only read when the content type says it is a form. A JSON caller
/// that omits the header gets the same 403 it always got, rather than having its
/// body scanned for a field that would not be there.
///
/// A token-authenticated request passes, for the reason given on
/// [`require_csrf_header`], which this calls first.
pub async fn require_csrf_header_or_field(
    session: &Session,
    headers: &axum::http::HeaderMap,
    body: &str,
) -> Result<(), crate::error::AppError> {
    if require_csrf_header(session, headers).await.is_ok() {
        return Ok(());
    }

    if is_form_urlencoded(headers)
        && let Some(token) = single_form_field(body, CSRF_FORM_FIELD)
        && crate::form::csrf::verify_csrf_token(session, &token)
            .await
            .unwrap_or(false)
    {
        return Ok(());
    }

    Err(crate::error::AppError::forbidden(format!(
        "Invalid or missing CSRF token. Include an X-CSRF-Token header, \
         or a {CSRF_FORM_FIELD} field in a form-urlencoded body."
    )))
}

/// Whether the request body is a URL-encoded form.
///
/// Matches on the media type alone, so a `charset` parameter does not change the
/// answer.
fn is_form_urlencoded(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
        .unwrap_or(false)
}

/// Read one field out of a URL-encoded body, requiring it to appear exactly once.
///
/// A repeated name is refused rather than resolved. There is no agreed answer to
/// which duplicate wins — a `HashMap` built from the pairs keeps the last, this
/// function would otherwise keep whichever it was written to keep — and a
/// security check should not depend on that choice. Two tokens in one body is a
/// malformed request, so it is treated as one.
pub(crate) fn single_form_field(body: &str, field: &str) -> Option<String> {
    let mut found = None;
    for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
        if key != field {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(value.into_owned());
    }
    found
}

/// Maximum length for a tag slug (matches `category_tag.slug` `VARCHAR(128)`).
pub const MAX_SLUG_LENGTH: usize = 128;

/// Error message for invalid slug format.
pub const SLUG_FORMAT_ERROR: &str = "Slug must start with a lowercase letter or digit and contain only lowercase letters, digits, and hyphens.";

/// Read an optional sampling temperature from an admin form field.
///
/// Blank means unset: the request then carries no `temperature` at all, which
/// is the only value every current model accepts. A number is clamped to
/// 0.0–2.0. Anything else, NaN and infinity included (`f32::clamp` would let
/// NaN through), is refused with a message for the form.
pub fn parse_optional_temperature(raw: &str) -> Result<Option<f32>, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    match raw.parse::<f32>() {
        Ok(value) if value.is_finite() => Ok(Some(value.clamp(0.0, 2.0))),
        _ => Err("Temperature must be a number, or blank to leave it unset."),
    }
}

/// Validate that a slug is URL-safe for use in gather route aliases.
///
/// A valid slug starts with `[a-z0-9]` and contains only lowercase ASCII
/// letters, digits, and hyphens. Maximum length is [`MAX_SLUG_LENGTH`].
pub fn is_valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LENGTH {
        return false;
    }

    let mut chars = slug.chars();

    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }

    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate that a machine name starts with a lowercase letter and contains
/// only lowercase letters, digits, and underscores.
pub fn is_valid_machine_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let mut chars = name.chars();

    // First character must be lowercase letter
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }

    // Rest must be lowercase letters, digits, or underscores
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// An error page for `status` with `message`, marked for the error-page layer.
///
/// The body is the built-in page, which is what a caller sees only when the
/// layer is absent (a unit test of a handler); with the layer in place a
/// browser gets the site's themed `error.html` and any other caller the
/// `AppError` body (see [`crate::middleware::error_page`]).
fn error_page(status: StatusCode, message: &str) -> Response {
    let marker = crate::middleware::ErrorPage {
        status,
        message: message.to_string(),
    };
    let html = crate::middleware::error_page::fallback_page(&marker, ("/", "Go to the home page"));
    let mut response = (status, Html(html)).into_response();
    response.extensions_mut().insert(marker);
    response
}

/// An error page for a request that could not be used: `400 Bad Request`.
pub fn render_error(message: &str) -> Response {
    error_page(StatusCode::BAD_REQUEST, message)
}

/// A refusal page for an actor who may use a screen but not on this target:
/// `403 Forbidden`.
pub fn render_forbidden(message: &str) -> Response {
    error_page(StatusCode::FORBIDDEN, message)
}

/// An error page for a server-side failure: `500 Internal Server Error`.
///
/// Also the one page a route serves when its own template cannot be rendered;
/// no route writes a fallback document of its own.
pub fn render_server_error(message: &str) -> Response {
    error_page(StatusCode::INTERNAL_SERVER_ERROR, message)
}

/// A `404 Not Found` page.
pub fn render_not_found() -> Response {
    error_page(
        StatusCode::NOT_FOUND,
        "The requested page could not be found.",
    )
}

/// Build local task tab data from the menu registry, merged with hardcoded tabs.
///
/// Looks up plugin-registered local tasks for the given `parent_path` and
/// merges them with `base_tabs`. Plugin tabs are appended after base tabs,
/// sorted by weight. Plugin task paths have `:id` placeholders substituted
/// with `id_value` (if provided) and are marked active when they match
/// `current_path`.
pub fn build_local_tasks(
    state: &crate::state::AppState,
    parent_path: &str,
    current_path: &str,
    id_value: Option<&str>,
    base_tabs: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut tabs = base_tabs;

    // Append plugin-registered local tasks from menu registry
    for task in state.menu_registry().local_tasks(parent_path) {
        let concrete_path = if let Some(id) = id_value {
            task.path.replace(":id", id)
        } else {
            task.path.clone()
        };
        tabs.push(serde_json::json!({
            "title": task.title,
            "path": concrete_path,
            "active": concrete_path == current_path,
        }));
    }

    serde_json::Value::Array(tabs)
}

/// Validate a password meets length requirements.
///
/// Checks: non-empty, minimum 12 characters, maximum 128 characters.
/// Returns `Ok(())` on success, or `Err` with a human-readable error message.
pub fn validate_password(password: &str) -> Result<(), &'static str> {
    if password.is_empty() {
        return Err("Password is required.");
    }
    if password.len() < 12 {
        return Err("Password must be at least 12 characters.");
    }
    if password.len() > 128 {
        return Err("Password must be 128 characters or fewer.");
    }
    Ok(())
}

/// Validate a username meets format requirements.
///
/// Checks: non-empty, 2–60 characters, alphanumeric + underscores + hyphens + periods only.
/// Returns `Ok(())` on success, or `Err` with a human-readable error message.
pub fn validate_username(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Username is required.");
    }
    if name.len() < 2 {
        return Err("Username must be at least 2 characters.");
    }
    if name.len() > 60 {
        return Err("Username must be 60 characters or fewer.");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(
            "Username may only contain letters, numbers, underscores, hyphens, and periods.",
        );
    }
    Ok(())
}

/// Require an authenticated, active **admin** user for JSON API endpoints.
///
/// `401` when nobody is signed in, `403` when the signed-in user is not an
/// administrator.
pub async fn require_admin_json(
    state: &AppState,
    session: &Session,
) -> Result<User, crate::error::AppError> {
    let user = require_login_json(state, session).await?;
    if user.is_admin {
        Ok(user)
    } else {
        Err(crate::error::AppError::forbidden("Admin access required"))
    }
}

/// Build a [`UserContext`] for an admin user, carrying their real permissions.
///
/// Admin routes call [`require_admin`], which returns a [`User`]; service-layer
/// methods need a `UserContext`. This is the sibling of [`get_user_context`] for
/// handlers that already hold the loaded user, and it goes through the same
/// loader: [`user_context_for`].
///
/// It used to return `authenticated(user.id, vec!["administer site"])`, which
/// dropped the admin's real permissions and worked only because `is_admin()`
/// short-circuits every permission check. That is the same
/// fabricate-instead-of-load shape that broke the front page, one short-circuit
/// away from the same failure, so it now loads.
pub async fn admin_user_context(state: &AppState, user: &User) -> UserContext {
    user_context_for(state, user).await
}

/// Build a request-scoped [`UserContext`] for an already-loaded [`User`].
///
/// The sanctioned loader for handlers past [`require_admin`], [`require_login`]
/// or [`require_permission`], all of which hand back the `User`. Session-derived
/// callers want [`get_user_context`] instead.
///
/// Delegates the context shape to
/// [`PermissionService::user_context`](crate::permissions::PermissionService::user_context)
/// and applies the web failure policy from [`permissions_or_deny_all`].
pub async fn user_context_for(state: &AppState, user: &User) -> UserContext {
    let access = state.permissions().user_access(user).await;
    // The role names ride on the same load; on a failure they are as empty as
    // the deny-all permission set, never guessed.
    let roles = access
        .as_ref()
        .map(|access| access.roles.to_vec())
        .unwrap_or_default();
    let permissions = permissions_or_deny_all(
        access.map(|access| access.permissions),
        state.metrics(),
        user.id,
    );
    crate::permissions::context_from_permissions(user, permissions).with_roles(roles)
}

/// Get the current viewer's context from the session, with **real** permissions
/// loaded from the database.
///
/// This is the sanctioned way for a request handler to obtain a
/// [`UserContext`], and the only one that starts from a session. Around thirty
/// read and render handlers use it (item, gather, search, file, comment, batch,
/// api_v1, and the navigation builder in [`inject_site_context`]). Handlers that
/// already hold a loaded [`User`] use [`user_context_for`] instead. Nothing in a
/// request path should build a context from a literal permission list.
///
/// A session naming a user who no longer exists yields the anonymous context,
/// carrying the anonymous role's permissions, exactly as no session does.
pub async fn get_user_context(session: &Session, state: &AppState) -> UserContext {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    // Both branches load a real user and go through the same builder: the
    // session user, or the anonymous user row that carries the anonymous role's
    // permissions.
    let Some(id) = user_id else {
        return anonymous_user_context(state).await;
    };
    match crate::models::User::find_by_id(state.db(), id)
        .await
        .ok()
        .flatten()
    {
        // A blocked account is nobody: its session is deleted and the request
        // is answered as anonymous, exactly as `require_login` does for the
        // routes that gate on it. Without this every render and read handler
        // went on treating a blocked user as themselves.
        Some(user) if !user.is_active() => {
            // `flush`, not `delete`, as in `session_user` above.
            let _ = session.flush().await;
            anonymous_user_context(state).await
        }
        Some(user) => user_context_for(state, &user).await,
        // A deleted user is nobody, and nobody has the anonymous role.
        None => anonymous_user_context(state).await,
    }
}

/// The stage the session asked to read, if any: the active stage stored by
/// `/admin/stage/switch`, parsed. Live, nothing stored, or a value that is not a
/// stage id are all `None`. Whether the session may read it is
/// [`read_stages`]'s decision, not this one's.
pub async fn session_stage(session: &Session) -> Option<Uuid> {
    let value: Option<String> = session
        .get::<Option<String>>(crate::routes::auth::SESSION_ACTIVE_STAGE)
        .await
        .ok()
        .flatten()
        .flatten();
    value
        .and_then(|s| s.parse::<Uuid>().ok())
        .filter(|id| *id != LIVE_STAGE_ID)
}

/// The stages a read draws on for `viewer`: the one stage resolver.
///
/// Live, for everyone. A holder of `use stages` (an administrator holds it
/// implicitly) who asked for another stage, by a request parameter or the
/// session's active stage, reads that stage with Live beneath it. A stage that
/// does not exist is Live. Deny wherever there is a choice: without the
/// permission, what was asked for is ignored rather than refused, so a link
/// carrying `?stage=` still works and shows Live.
///
/// No query on the common path: the permission is the viewer's, and the stage
/// is checked against the item service's cached stage view.
pub async fn read_stages(
    state: &AppState,
    viewer: &UserContext,
    requested: Option<Uuid>,
) -> Vec<Uuid> {
    let Some(stage) = requested.filter(|id| *id != LIVE_STAGE_ID) else {
        return vec![LIVE_STAGE_ID];
    };
    if !viewer.can(crate::content::USE_STAGES) {
        return vec![LIVE_STAGE_ID];
    }
    match state.items().stage_view().await {
        Ok(view) if view.stage(stage).is_some() => vec![stage, LIVE_STAGE_ID],
        Ok(_) => vec![LIVE_STAGE_ID],
        Err(e) => {
            tracing::warn!(error = %e, "stage lookup failed, reading Live");
            vec![LIVE_STAGE_ID]
        }
    }
}

/// The context of a visitor with no session user: the anonymous user row with
/// the anonymous role's real permissions.
///
/// This is what [`get_user_context`] returns for a request without a login, so a
/// handler asking "could an anonymous visitor see this too?" gets exactly the
/// answer an anonymous request would.
pub async fn anonymous_user_context(state: &AppState) -> UserContext {
    match crate::models::User::find_by_id(state.db(), Uuid::nil())
        .await
        .ok()
        .flatten()
    {
        Some(user) => user_context_for(state, &user).await,
        None => UserContext::anonymous(),
    }
}

/// The web failure policy for a permission load: **degrade to the deny-all set,
/// loudly**.
///
/// [`get_user_context`] cannot propagate an error — it returns a `UserContext`
/// to two dozen render handlers with a dozen different return types — so the
/// policy has to be chosen here rather than fall out of an `unwrap_or_default`.
/// It is chosen deliberately:
///
/// - **Degrade, not fail closed.** A permission-load error almost always means
///   the database is unreachable, and the handler is about to query it anyway;
///   turning it into a 500 here buys nothing over the 500 the handler produces
///   on its own, at the cost of routing an error type through every read path.
/// - **Loudly.** The reason the old `unwrap_or_default()` was dangerous is not
///   the empty set, which denies rather than grants; it is that an empty set is
///   *indistinguishable from a working permission model*. Content vanishes, the
///   site looks fine, and nothing says why — the exact signature of the
///   front-page bug. So each failure logs at ERROR with the user id and
///   increments `trovato_permission_load_failures_total`, which should alert on
///   any non-zero value.
///
/// Callers that *can* propagate should fail closed instead: use
/// [`PermissionService::user_context`](crate::permissions::PermissionService::user_context),
/// which returns the error.
pub fn permissions_or_deny_all(
    loaded: anyhow::Result<HashSet<String>>,
    metrics: &crate::metrics::Metrics,
    user_id: Uuid,
) -> HashSet<String> {
    match loaded {
        Ok(permissions) => permissions,
        Err(e) => {
            tracing::error!(
                error = %e,
                user_id = %user_id,
                "failed to load user permissions; degrading this request to the deny-all \
                 permission set. The viewer sees only what is public, so content they are \
                 entitled to will be missing"
            );
            metrics.permission_load_failures.inc();
            HashSet::new()
        }
    }
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Basic email format validation.
///
/// Checks that the address has the structure `local@domain.tld` where:
/// - Local part is non-empty
/// - Domain contains at least one dot
/// - Domain labels are non-empty
/// - Total length is within RFC 5321 limits (254 chars)
///
/// This is deliberately lenient — full RFC 5322 compliance is not
/// attempted. The goal is to reject obviously invalid addresses while
/// accepting the vast majority of real-world addresses.
pub fn is_valid_email(email: &str) -> bool {
    if email.len() > 254 {
        return false;
    }

    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };

    // Local part must be non-empty and <= 64 chars
    if local.is_empty() || local.len() > 64 {
        return false;
    }

    // Domain must contain at least one dot with non-empty labels
    if !domain.contains('.') {
        return false;
    }

    // All domain labels must be non-empty
    domain.split('.').all(|label| !label.is_empty())
}

/// Basic timezone format validation.
///
/// Accepts IANA timezone identifiers (e.g. `America/New_York`, `UTC`,
/// `Europe/London`) by checking that the value contains only ASCII
/// alphanumeric characters, `/`, `_`, `+`, and `-`.
pub fn is_valid_timezone(tz: &str) -> bool {
    !tz.is_empty()
        && tz.len() <= 64
        && tz
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '+' || c == '-')
}

/// Escape the five XML predefined entities in text content.
///
/// Shared by every kernel route that writes XML by hand — the RSS feeds and the
/// sitemap. A URL alias may legitimately contain an `&`, and an unescaped one
/// makes the whole document unparseable rather than making one entry wrong.
pub fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// One language a page can be read in, and the address it is read at.
///
/// Goes into the render context as `available_translations`, which is what a
/// theme builds a language switcher out of, and is the input to
/// [`build_hreflang_links`]. The two must not disagree: a switcher offering an
/// address that the `hreflang` tags do not name is a page telling a person and a
/// crawler different things.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TranslationLink {
    /// The language code.
    pub language: String,
    /// The address this page is served at in that language.
    pub path: String,
}

/// Build `<link rel="alternate" hreflang="...">` data for template rendering.
///
/// Takes the languages a page actually exists in, rather than every language the
/// site knows: `hreflang` naming an address that 404s is worse than no tag,
/// because a crawler acts on it. `x-default` points at the default language's
/// address, falling back to the first entry when the page has no default-language
/// version.
pub fn build_hreflang_links(
    translations: &[TranslationLink],
    default_language: &str,
) -> Vec<serde_json::Value> {
    if translations.is_empty() {
        return Vec::new();
    }

    let mut links = Vec::with_capacity(translations.len() + 1);

    for alternate in translations {
        links.push(serde_json::json!({ "lang": alternate.language, "href": alternate.path }));
    }

    let x_default = translations
        .iter()
        .find(|t| t.language == default_language)
        .unwrap_or(&translations[0]);
    links.push(serde_json::json!({ "lang": "x-default", "href": x_default.path }));

    links
}

/// Every language a page can be read in, with the address for each.
///
/// The default language's address is the canonical alias verbatim; every other
/// language is that same address behind a `/{lang}` prefix, which is how the
/// language prefix negotiator reads it back. Existence comes from
/// `item_translation` in one query, and a translation in a language the site no
/// longer configures is dropped rather than offered.
pub async fn available_translations(
    state: &crate::state::AppState,
    item_id: uuid::Uuid,
    canonical_path: &str,
) -> Vec<TranslationLink> {
    let default_language = state.default_language();

    let mut links = vec![TranslationLink {
        language: default_language.to_string(),
        path: canonical_path.to_string(),
    }];

    let translated = state
        .items()
        .translated_languages(item_id)
        .await
        .unwrap_or_default();

    for language in translated {
        if language == default_language || !state.known_languages().contains(&language) {
            continue;
        }
        links.push(TranslationLink {
            path: format!("/{language}{canonical_path}"),
            language,
        });
    }

    links
}

/// Apply a translation overlay to an item's title and fields.
///
/// If a translation exists for `language`, the item's title is replaced
/// (when the translated title is non-empty) and translated JSONB fields
/// are merged into the item's existing fields. Errors are logged and
/// silently ignored so the caller falls back to the default language.
pub async fn apply_translation_overlay(
    items: &crate::content::ItemService,
    item: &mut crate::models::Item,
    language: &str,
) {
    match items.load_translation(item.id, language).await {
        Ok(Some(translation)) => {
            if !translation.title.is_empty() {
                item.title = translation.title;
            }
            if let Some(translated_fields) = translation.fields.as_object()
                && let Some(item_fields) = item.fields.as_object_mut()
            {
                for (key, value) in translated_fields {
                    item_fields.insert(key.clone(), value.clone());
                }
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                item_id = %item.id,
                language = %language,
                error = %e,
                "failed to load item translation, falling back to default"
            );
        }
    }
}

/// The most references one page resolves; any beyond are left unresolved,
/// so a page cannot turn one view into an unbounded number of queries.
const MAX_PAGE_REFERENCES: usize = 20;

/// Render a page builder field's value for `viewer`, with its reference
/// fields resolved as that viewer sees them.
///
/// An item reference resolves to `{ id, title, type, url }` if the item exists
/// and `viewer` may view it; a gather reference to `{ id, label, url, total,
/// rows }` (the first page, at most the field's `limit` rows, each with its
/// `url`) if the gather exists and `viewer` may run it. Anything else is left
/// out, and the template renders without it. The item page, the front page and
/// the revision preview all render through here.
pub async fn render_page_builder_field(
    state: &AppState,
    viewer: &UserContext,
    value: &serde_json::Value,
) -> anyhow::Result<crate::content::page_builder::RenderedPage> {
    use crate::content::page_builder::{PuckPage, ResolvedReferences, collect_references};
    use crate::content::page_builder_components::ReferenceTarget;
    use anyhow::Context as _;

    let page: PuckPage =
        serde_json::from_value(value.clone()).context("failed to parse page builder JSON")?;
    let mut resolved = ResolvedReferences::default();
    for key in collect_references(&page, state.theme().components())
        .iter()
        .take(MAX_PAGE_REFERENCES)
    {
        match key.target {
            ReferenceTarget::Item => {
                let Ok(id) = Uuid::parse_str(&key.id) else {
                    continue;
                };
                let Ok(Some(item)) = state.items().load(id).await else {
                    continue;
                };
                if !state
                    .items()
                    .check_access(&item, "view", viewer)
                    .await
                    .unwrap_or(false)
                {
                    continue;
                }
                let source = format!("/item/{id}");
                let url = crate::models::UrlAlias::find_by_source(state.db(), &source)
                    .await
                    .ok()
                    .and_then(|aliases| aliases.into_iter().next())
                    .map(|a| a.alias)
                    .unwrap_or(source);
                resolved.insert(
                    key,
                    serde_json::json!({
                        "id": item.id,
                        "title": item.title,
                        "type": item.item_type,
                        "url": url,
                    }),
                );
            }
            ReferenceTarget::Gather => {
                let Some(query) = state.gather().get_query(&key.id) else {
                    continue;
                };
                if crate::gather::authorize_gather(viewer, &query).is_err() {
                    continue;
                }
                let context = crate::gather::QueryContext {
                    current_user_id: viewer.authenticated.then_some(viewer.id),
                    viewer: Some(viewer.clone()),
                    url_args: std::collections::HashMap::new(),
                    language: None,
                };
                let Ok(result) = state
                    .gather()
                    .execute(
                        &key.id,
                        1,
                        std::collections::HashMap::new(),
                        LIVE_STAGE_ID,
                        &context,
                    )
                    .await
                else {
                    continue;
                };
                let rows: Vec<serde_json::Value> = result
                    .items
                    .into_iter()
                    .take(key.limit as usize)
                    .map(|mut row| {
                        if let Some(id) = row.get("id").and_then(|v| v.as_str()).map(String::from)
                            && let Some(obj) = row.as_object_mut()
                        {
                            obj.entry("url").or_insert_with(|| {
                                serde_json::Value::String(format!("/item/{id}"))
                            });
                        }
                        row
                    })
                    .collect();
                resolved.insert(
                    key,
                    serde_json::json!({
                        "id": query.query_id,
                        "label": query.label,
                        "url": format!("/gather/{}", query.query_id),
                        "total": result.total,
                        "rows": rows,
                    }),
                );
            }
        }
    }
    state.theme().render_page_builder_content(value, &resolved)
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_temperature_is_unset_and_a_number_is_clamped() {
        assert_eq!(parse_optional_temperature(""), Ok(None));
        assert_eq!(parse_optional_temperature("   "), Ok(None));
        assert_eq!(parse_optional_temperature("0.3"), Ok(Some(0.3)));
        assert_eq!(parse_optional_temperature("7"), Ok(Some(2.0)));
        assert_eq!(parse_optional_temperature("-1"), Ok(Some(0.0)));
        assert!(parse_optional_temperature("NaN").is_err());
        assert!(parse_optional_temperature("inf").is_err());
        assert!(parse_optional_temperature("warm").is_err());
    }

    #[test]
    fn test_html_escape_special_chars() {
        assert_eq!(
            html_escape("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
    }

    fn headers_with_content_type(value: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    #[test]
    fn a_form_content_type_is_recognized_with_or_without_parameters() {
        assert!(is_form_urlencoded(&headers_with_content_type(
            "application/x-www-form-urlencoded"
        )));
        // A browser is entitled to add a charset, and does.
        assert!(is_form_urlencoded(&headers_with_content_type(
            "application/x-www-form-urlencoded; charset=UTF-8"
        )));
        // Case is not significant in a media type.
        assert!(is_form_urlencoded(&headers_with_content_type(
            "Application/X-WWW-Form-Urlencoded"
        )));
    }

    #[test]
    fn anything_else_is_not_a_form() {
        assert!(!is_form_urlencoded(&headers_with_content_type(
            "application/json"
        )));
        assert!(!is_form_urlencoded(&headers_with_content_type(
            "multipart/form-data; boundary=x"
        )));
        // No content type at all: a body of unknown shape is not scanned.
        assert!(!is_form_urlencoded(&axum::http::HeaderMap::new()));
    }

    #[test]
    fn a_form_field_is_read_and_percent_decoded() {
        assert_eq!(
            single_form_field("_token=abc123&message=hi", "_token").as_deref(),
            Some("abc123")
        );
        // Order does not matter, and `+` and `%xx` decode as a form encodes them.
        assert_eq!(
            single_form_field("message=hello+world&_token=a%2Fb", "_token").as_deref(),
            Some("a/b")
        );
    }

    #[test]
    fn a_missing_or_empty_field_is_none_or_empty_and_never_a_pass() {
        assert_eq!(single_form_field("message=hi", "_token"), None);
        assert_eq!(single_form_field("", "_token"), None);
        // Present but empty is Some(""), which `verify_csrf_token` refuses. The
        // distinction is kept here so the caller does the refusing.
        assert_eq!(single_form_field("_token=", "_token").as_deref(), Some(""));
    }

    #[test]
    fn a_repeated_field_is_refused_rather_than_resolved() {
        assert_eq!(single_form_field("_token=a&_token=b", "_token"), None);
        assert_eq!(
            single_form_field("_token=a&message=hi&_token=a", "_token"),
            None
        );
    }

    #[test]
    fn test_html_escape_ampersand() {
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn test_html_escape_quotes() {
        assert_eq!(html_escape(r#"say "hello""#), "say &quot;hello&quot;");
    }

    #[test]
    fn test_html_escape_plain_text() {
        assert_eq!(html_escape("hello world"), "hello world");
    }

    #[test]
    fn test_html_escape_empty() {
        assert_eq!(html_escape(""), "");
    }

    #[test]
    fn test_is_valid_email_valid() {
        assert!(is_valid_email("user@example.com"));
        assert!(is_valid_email("user@sub.domain.co.uk"));
        assert!(is_valid_email("a@b.c"));
        assert!(is_valid_email("user+tag@example.com"));
    }

    #[test]
    fn test_is_valid_email_invalid() {
        assert!(!is_valid_email(""));
        assert!(!is_valid_email("user"));
        assert!(!is_valid_email("@."));
        assert!(!is_valid_email(".@"));
        assert!(!is_valid_email("user@"));
        assert!(!is_valid_email("@domain.com"));
        assert!(!is_valid_email("user@domain"));
        assert!(!is_valid_email("user@.com"));
        assert!(!is_valid_email("user@domain."));
        assert!(!is_valid_email("user@domain..com"));
    }

    #[test]
    fn test_is_valid_timezone_valid() {
        assert!(is_valid_timezone("UTC"));
        assert!(is_valid_timezone("America/New_York"));
        assert!(is_valid_timezone("Europe/London"));
        assert!(is_valid_timezone("Etc/GMT+5"));
        assert!(is_valid_timezone("US/Eastern"));
    }

    #[test]
    fn test_is_valid_timezone_invalid() {
        assert!(!is_valid_timezone(""));
        assert!(!is_valid_timezone("America/New York"));
        assert!(!is_valid_timezone("<script>"));
        assert!(!is_valid_timezone("a".repeat(65).as_str()));
    }

    #[test]
    fn test_validate_password_valid() {
        assert!(validate_password("abcdefghijkl").is_ok());
        assert!(validate_password("a".repeat(128).as_str()).is_ok());
    }

    #[test]
    fn test_validate_password_invalid() {
        assert!(validate_password("").is_err());
        assert!(validate_password("short").is_err());
        assert!(validate_password("a".repeat(129).as_str()).is_err());
    }

    #[test]
    fn test_validate_username_valid() {
        assert!(validate_username("ab").is_ok());
        assert!(validate_username("user_name").is_ok());
        assert!(validate_username("user-name").is_ok());
        assert!(validate_username("user.name").is_ok());
        assert!(validate_username("Admin123").is_ok());
    }

    #[test]
    fn test_validate_username_invalid() {
        assert!(validate_username("").is_err());
        assert!(validate_username("a").is_err());
        assert!(validate_username("a".repeat(61).as_str()).is_err());
        assert!(validate_username("user name").is_err());
        assert!(validate_username("user@name").is_err());
        assert!(validate_username("user<script>").is_err());
    }

    #[test]
    fn test_is_valid_slug_valid() {
        assert!(is_valid_slug("rust"));
        assert!(is_valid_slug("ai-data"));
        assert!(is_valid_slug("123"));
        assert!(is_valid_slug("a"));
        assert!(is_valid_slug("web-3-tools"));
        assert!(is_valid_slug(&"a".repeat(128)));
    }

    #[test]
    fn test_is_valid_slug_invalid() {
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("-starts-with-dash"));
        assert!(!is_valid_slug("UPPERCASE"));
        assert!(!is_valid_slug("has spaces"));
        assert!(!is_valid_slug("under_score"));
        assert!(!is_valid_slug("special!char"));
        assert!(!is_valid_slug("Capitalized"));
        assert!(!is_valid_slug(&"a".repeat(129)));
    }

    // --- is_valid_machine_name tests ---

    #[test]
    fn valid_machine_name_accepts_alphanumeric_underscore() {
        assert!(is_valid_machine_name("my_plugin_name"));
        assert!(is_valid_machine_name("trovato_blog"));
        assert!(is_valid_machine_name("a"));
        assert!(is_valid_machine_name("abc123"));
        assert!(is_valid_machine_name("field_1_value"));
    }

    #[test]
    fn valid_machine_name_rejects_special_chars() {
        assert!(!is_valid_machine_name("my-plugin"));
        assert!(!is_valid_machine_name("my plugin"));
        assert!(!is_valid_machine_name("../traversal"));
        assert!(!is_valid_machine_name(""));
    }

    #[test]
    fn valid_machine_name_rejects_leading_digit() {
        assert!(!is_valid_machine_name("1field"));
        assert!(!is_valid_machine_name("9_start"));
    }

    #[test]
    fn valid_machine_name_rejects_leading_underscore() {
        assert!(!is_valid_machine_name("_private"));
    }

    #[test]
    fn valid_machine_name_rejects_uppercase() {
        assert!(!is_valid_machine_name("MyPlugin"));
        assert!(!is_valid_machine_name("ALLCAPS"));
        assert!(!is_valid_machine_name("mixedCase"));
    }

    // --- build_hreflang_links tests ---

    fn translation(language: &str, path: &str) -> TranslationLink {
        TranslationLink {
            language: language.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn hreflang_links_default_language_unprefixed() {
        let links = build_hreflang_links(
            &[translation("en", "/about"), translation("fr", "/fr/about")],
            "en",
        );
        // en (default) is the unprefixed address
        assert_eq!(links[0]["lang"], "en");
        assert_eq!(links[0]["href"], "/about");
        // fr carries its prefix
        assert_eq!(links[1]["lang"], "fr");
        assert_eq!(links[1]["href"], "/fr/about");
        // x-default points at the default language
        assert_eq!(links[2]["lang"], "x-default");
        assert_eq!(links[2]["href"], "/about");
    }

    #[test]
    fn hreflang_links_single_language() {
        let links = build_hreflang_links(&[translation("en", "/")], "en");
        assert_eq!(links.len(), 2); // en + x-default
        assert_eq!(links[0]["lang"], "en");
        assert_eq!(links[0]["href"], "/");
        assert_eq!(links[1]["lang"], "x-default");
        assert_eq!(links[1]["href"], "/");
    }

    #[test]
    fn hreflang_links_name_only_the_languages_the_page_exists_in() {
        // The site may know four languages; this page is written in three, and a
        // tag for the fourth would point a crawler at a 404.
        let links = build_hreflang_links(
            &[
                translation("en", "/contact"),
                translation("fr", "/fr/contact"),
                translation("de", "/de/contact"),
            ],
            "en",
        );
        assert_eq!(links.len(), 4); // 3 languages + x-default
        assert_eq!(links[1]["href"], "/fr/contact");
        assert_eq!(links[2]["href"], "/de/contact");
    }

    #[test]
    fn a_page_with_nothing_to_offer_gets_no_tags() {
        assert!(build_hreflang_links(&[], "en").is_empty());
    }

    #[test]
    fn x_default_falls_back_when_there_is_no_default_language_version() {
        let links = build_hreflang_links(&[translation("fr", "/fr/about")], "en");
        assert_eq!(links.last().unwrap()["lang"], "x-default");
        assert_eq!(links.last().unwrap()["href"], "/fr/about");
    }

    // --- permissions_or_deny_all: the chosen failure policy ---
    //
    // `admin_user_context` now loads from the database, so what it produces is
    // pinned in `tests/user_context_test.rs` against a real user and role.

    #[test]
    fn permission_load_success_passes_the_set_through_untouched() {
        let metrics = crate::metrics::Metrics::new();
        let loaded: HashSet<String> = ["access content".to_string()].into_iter().collect();

        let permissions = permissions_or_deny_all(Ok(loaded), &metrics, Uuid::now_v7());

        assert!(permissions.contains("access content"));
        assert_eq!(metrics.permission_load_failures.get(), 0);
    }

    #[test]
    fn permission_load_failure_denies_all_and_is_counted() {
        let metrics = crate::metrics::Metrics::new();

        let permissions = permissions_or_deny_all(
            Err(anyhow::anyhow!("connection closed")),
            &metrics,
            Uuid::now_v7(),
        );

        // Deny-all, not grant-all: the degraded request is safe.
        assert!(permissions.is_empty());
        // And it is observable, which is the whole point — an empty permission
        // set is otherwise indistinguishable from a working permission model.
        assert_eq!(metrics.permission_load_failures.get(), 1);
    }
}
