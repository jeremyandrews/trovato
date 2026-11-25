//! Plugin-served HTTP requests (**G-NO-PLUGIN-HTTP**, K1 fix 1).
//!
//! # The gap this closes
//!
//! Before `KERNEL_API_VERSION (0,99)` there was **no surface in Trovato through
//! which a plugin served a request**, and therefore no way for an authenticated
//! user to write a plugin-owned table. Every candidate closed for a different
//! reason, which is why it read as an absence rather than a decision:
//!
//! - `tap_menu` looked like routing and was not. The SDK's `MenuDefinition`
//!   carried a `callback` field; the kernel's did not, so it was dropped on
//!   deserialize. A plugin author reading the SDK believed they had registered
//!   a handler.
//! - The form taps were unreachable: `FormService` is constructed and exposed
//!   on `AppState`, and no route calls `build` or `process`.
//! - `tap_form_ajax` had a route and was closed three times over: `require_admin`,
//!   `RequestState::without_services` (so a dispatched tap had no DB handle),
//!   and a `form_state_cache` lookup nothing ever writes.
//! - `public_functions` + `invoke` is plugin-to-plugin only.
//! - Record-type admin is list and view only.
//!
//! # What this module does
//!
//! At startup [`build_plugin_api_router`] walks the menu registry and registers
//! an axum route for every entry with `handler_type = "api"` and a non-empty
//! `callback`. A matching request is authorized against the entry's
//! `permission`, then dispatched to the **owning plugin's** `tap_api` with a
//! live services handle and the authenticated user, so the tap can write.
//!
//! # Security posture
//!
//! - **Permission.** The menu entry's `permission` field is the gate, checked
//!   before dispatch. Empty means public — the same convention the rest of the
//!   registry uses. An anonymous caller who fails the check gets 403, never a
//!   dispatch, so a plugin cannot be reached by someone the site did not
//!   authorize.
//! - **CSRF / bearer (G-CSRF-NO-BEARER-BYPASS, recorded in `M3-DESIGN.md`).**
//!   A state-changing method requires a CSRF token **unless the request was
//!   authenticated by an `Authorization: Bearer` API token**. The reasoning is
//!   the threat model, not convenience: CSRF exists because a browser attaches
//!   cookies to a cross-site request by itself, so a cookie-authenticated write
//!   can be forged by any page the victim visits. A bearer token is never
//!   attached automatically by any browser — an attacker's page cannot make the
//!   victim's browser send it — so a bearer-authenticated request has no CSRF
//!   exposure to protect against. The exemption is keyed on the
//!   [`ApiTokenAuth`] marker the token middleware sets *only when the token
//!   actually authenticated the request*: when a session cookie identified the
//!   user, the bearer header was ignored and CSRF still applies. This is what
//!   lets a token client (the Argus iOS series) write without a
//!   session-establishing round-trip.
//! - **Forms without JavaScript.** The token is read from the `X-CSRF-Token`
//!   header or from a `_token` field in a form-urlencoded body, and the plugin
//!   is handed a freshly minted token in [`ApiRequest::csrf_token`] to render
//!   into a hidden input. A plain HTML `<form>` cannot set a header, so before
//!   both halves existed a plugin-served form was refused with 403 whatever it
//!   rendered, and a public-facing plugin needed JavaScript to post anything.
//!   The verification is the same one every kernel form uses: single-use,
//!   session-bound, and an hour long.
//! - **Response.** A plugin's `status`, `body` and `content_type` are served as
//!   given, with an out-of-range status served as 500 and the content type
//!   validated as a header value. The kernel does **not** sanitize a plugin's
//!   response body — a plugin that emits HTML is responsible for escaping it,
//!   the same contract view taps have — and `content_type` defaults to
//!   `application/json` for that reason.
//! - **Body size.** Capped at [`MAX_REQUEST_BODY`] — [`trovato_sdk::abi::
//!   PLUGIN_IO_MAX_BYTES`], the one ceiling the whole tap/route memory
//!   protocol shares (B181) — because the body crosses the WASM boundary
//!   through that same channel. Axum refuses a [`Bytes`] body over 2 MB by
//!   default, below this cap, so the router raises its limit explicitly
//!   ([`build_plugin_api_router`]) rather than silently capping every plugin
//!   route at the framework default. A `multipart/form-data` request is the
//!   one exception (**G-FILE-NO-HOST-API**, BL-107): the kernel parses it
//!   itself, stores each file part through [`crate::file::FileService`] — the
//!   same size limit, MIME allowlist and magic-byte check `/file/upload` gets —
//!   and hands the plugin [`ApiRequest::files`], a list of ids and metadata, in
//!   place of the bytes. No file's bytes cross the WASM boundary; the overall
//!   request is capped at [`crate::file::MAX_UPLOAD_BODY_SIZE`] by the router's
//!   own body limit, same as a direct upload. The request's other fields still
//!   reach the plugin, re-encoded as a JSON object in [`ApiRequest::body`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, Multipart, OriginalUri, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use tower_sessions::Session;

use crate::file::MAX_UPLOAD_BODY_SIZE;
use crate::menu::MenuDefinition;
use crate::middleware::api_token::ApiTokenAuth;
use crate::middleware::language::ResolvedLanguage;
use crate::state::AppState;
use crate::tap::RequestState;
use trovato_sdk::types::{ApiRequest, ApiResponse, UploadedFile};

/// Largest request body handed to a plugin, in bytes.
///
/// The body crosses the WASM boundary through the tap I/O buffer, so an
/// unbounded body is a self-inflicted truncation bug. Equal to
/// [`trovato_sdk::abi::PLUGIN_IO_MAX_BYTES`] (B181) rather than an
/// independent number: it is the same channel `tap_api`'s request and
/// response travel over, and a plugin author should not have to remember a
/// different ceiling for each end of one call.
pub const MAX_REQUEST_BODY: usize = trovato_sdk::abi::PLUGIN_IO_MAX_BYTES;

/// The `handler_type` value that marks a menu entry as plugin-served.
const API_HANDLER_TYPE: &str = "api";

/// Menu entries that name a callback the kernel will never dispatch to.
///
/// A `callback` is only ever called when `handler_type` is `"api"`
/// ([`build_plugin_api_router`]). A page-style entry with a callback is therefore
/// a declaration with no consumer: the path 404s, and the plugin's handler
/// function becomes dead code — usually wearing an `#[allow(dead_code)]` comment
/// claiming a route callback calls it at runtime.
///
/// Two shipped plugins were dead exactly this way, `trovato_feeds` and
/// `trovato_scolta`, and nothing said so at any point: not at build time, not at
/// plugin load, not at the first request. This is what says so, logged once at
/// startup.
///
/// Returned as pairs rather than logged here so the rule can be tested directly.
pub fn unreachable_callbacks(menus: &[MenuDefinition]) -> Vec<(String, String)> {
    menus
        .iter()
        .filter(|menu| menu.handler_type != API_HANDLER_TYPE && !menu.callback.trim().is_empty())
        .map(|menu| (menu.plugin.clone(), menu.path.clone()))
        .collect()
}

/// Log every menu entry whose callback the kernel will never reach.
///
/// Called once at startup, next to router construction.
pub fn warn_unreachable_callbacks(menus: &[MenuDefinition]) {
    for (plugin, path) in unreachable_callbacks(menus) {
        tracing::warn!(
            plugin = %plugin,
            path = %path,
            "menu entry declares a callback but handler_type is not \"api\"; \
             the kernel will never dispatch it and this path will 404"
        );
    }
}

/// Build a router serving every `handler_type = "api"` menu entry.
///
/// Called once at startup, after `tap_menu` has populated the registry — the
/// same shape [`crate::routes::gather_routes::build_gather_route_router`] uses
/// for gather route aliases.
///
/// Entries are skipped, with a warning, when they are unusable rather than
/// merely absent: no `callback` (the plugin declared an api route and named no
/// handler), a path that is not absolute, an unsupported method, or a path that
/// collides with one already registered — axum panics on a route conflict, and a
/// plugin must not be able to take the process down by declaring a duplicate.
pub fn build_plugin_api_router(menus: &[MenuDefinition]) -> Router<AppState> {
    let mut router = Router::new();
    let (served, skipped) = served_api_entries(menus);

    for (menu, reason) in skipped {
        tracing::warn!(
            plugin = %menu.plugin,
            path = %menu.path,
            method = %menu.method,
            "skipping api menu entry: {reason}"
        );
    }

    for entry in served {
        let menu = entry.menu;
        let config = Arc::new(menu.clone());
        let handler = {
            let config = Arc::clone(&config);
            move |state: State<AppState>,
                  session: Session,
                  bearer: Option<Extension<ApiTokenAuth>>,
                  language: Option<Extension<ResolvedLanguage>>,
                  headers: HeaderMap,
                  uri: OriginalUri,
                  method: Method,
                  params: Path<HashMap<String, String>>,
                  query: Query<HashMap<String, String>>,
                  http_request: Request| {
                let config = Arc::clone(&config);
                async move {
                    serve(
                        config,
                        state,
                        session,
                        bearer,
                        language,
                        headers,
                        uri,
                        method,
                        params,
                        query,
                        http_request,
                    )
                    .await
                }
            }
        };

        let route = match entry.method.as_str() {
            "GET" => get(handler),
            "POST" => post(handler),
            "PUT" => put(handler),
            _ => delete(handler),
        };

        tracing::debug!(
            plugin = %menu.plugin,
            path = %entry.path,
            method = %entry.method,
            callback = %menu.callback,
            "registered plugin api route"
        );
        router = router.route(&entry.path, route);
    }

    // Axum refuses a body over its 2 MB framework default before any handler
    // runs, whatever this module checks later, so the ceiling is raised here
    // to admit both cases `serve` handles: a body up to `MAX_REQUEST_BODY`
    // (B181), and a `multipart/form-data` upload up to `MAX_UPLOAD_BODY_SIZE`,
    // the number `/file/upload` is layered with. Neither is loosened: `serve`
    // still enforces `MAX_REQUEST_BODY` on a non-multipart body and
    // `MAX_UPLOAD_BODY_SIZE` on a multipart one, after this layer admits it.
    router.layer(DefaultBodyLimit::max(
        MAX_REQUEST_BODY.max(MAX_UPLOAD_BODY_SIZE),
    ))
}

/// A plugin `api` menu entry the kernel serves, with its method upper-cased
/// and its path in axum's (and OpenAPI's) `{name}` syntax.
pub struct ServedApi<'a> {
    /// The menu entry as the plugin declared it.
    pub menu: &'a MenuDefinition,
    /// `GET`, `POST`, `PUT` or `DELETE`.
    pub method: String,
    /// The route path, `/x/{id}` for a declared `/x/:id`.
    pub path: String,
}

/// Which `api` menu entries the kernel serves, and why it skips the rest.
///
/// The one rule both the router ([`build_plugin_api_router`]) and the OpenAPI
/// document read, so the document cannot list a plugin route the router does
/// not serve. An entry is skipped when it names no callback, its path is not
/// absolute, its method is not one of the four the router mounts, or it
/// repeats a method and path already taken: axum panics on a route conflict,
/// and a plugin must not be able to take the process down by declaring one.
/// Entries that are not `api` entries are neither served nor reported.
pub fn served_api_entries(
    menus: &[MenuDefinition],
) -> (Vec<ServedApi<'_>>, Vec<(&MenuDefinition, &'static str)>) {
    let mut served = Vec::new();
    let mut skipped = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for menu in menus {
        if menu.handler_type != API_HANDLER_TYPE {
            continue;
        }
        if menu.callback.trim().is_empty() {
            skipped.push((menu, "no callback"));
            continue;
        }
        if !menu.path.starts_with('/') {
            skipped.push((menu, "path is not absolute"));
            continue;
        }
        let method = menu.method.to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "DELETE") {
            skipped.push((menu, "unsupported method"));
            continue;
        }
        let path = to_axum_path(&menu.path);
        if !seen.insert((method.clone(), path.clone())) {
            skipped.push((menu, "duplicate method and path"));
            continue;
        }
        served.push(ServedApi { menu, method, path });
    }

    (served, skipped)
}

/// Translate a menu path pattern to axum's syntax: `/x/:id` → `/x/{id}`.
///
/// The registry's own matcher (`menu::registry::match_pattern`) uses the `:name`
/// form, so a plugin author writes one syntax for both.
fn to_axum_path(path: &str) -> String {
    path.split('/')
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => format!("{{{name}}}"),
            None => segment.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether a method changes state, and so needs CSRF protection when the caller
/// is authenticated by an ambient session cookie.
fn is_state_changing(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Serve one plugin-owned request.
#[allow(clippy::too_many_arguments)] // One axum handler; each argument is a distinct extractor.
async fn serve(
    menu: Arc<MenuDefinition>,
    State(state): State<AppState>,
    session: Session,
    bearer: Option<Extension<ApiTokenAuth>>,
    language: Option<Extension<ResolvedLanguage>>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    method: Method,
    Path(params): Path<HashMap<String, String>>,
    Query(query): Query<HashMap<String, String>>,
    http_request: Request,
) -> Response {
    // A disabled plugin serves nothing, the same 404 the gated kernel routes
    // give — a route registered at startup must not outlive its plugin.
    if !state.is_plugin_enabled(&menu.plugin) {
        return error_response(StatusCode::NOT_FOUND, "Not found");
    }

    let user = crate::routes::item::get_user_context(&session, &state).await;

    // Permission gate. Empty `permission` means public, matching the registry's
    // convention everywhere else.
    if !menu.permission.is_empty() && !user.has_permission(&menu.permission) && !user.is_admin() {
        let status = if user.authenticated {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        };
        return error_response(status, "Access denied");
    }

    // A `multipart/form-data` request is parsed and its files stored here,
    // before dispatch; everything else keeps the exact body handling this
    // route always had. See `read_multipart_fields` for the CSRF and storage
    // ordering this branch relies on.
    let (body, files) = if is_multipart_content_type(&headers) {
        match read_multipart_fields(
            &state,
            &session,
            &headers,
            is_state_changing(&method) && bearer.is_none(),
            user.id,
            http_request,
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(response) => return response,
        }
    } else {
        let body = match Bytes::from_request(http_request, &state).await {
            Ok(body) => body,
            // `Bytes` only rejects when the body exceeds the router's layered
            // limit or fails to read off the wire; its `IntoResponse` already
            // answers with the right status.
            Err(rejection) => return rejection.into_response(),
        };

        // CSRF, with the bearer exemption argued in the module docs.
        //
        // The token is accepted from the `X-CSRF-Token` header or from a
        // `_token` field in a form-urlencoded body, because a plain HTML
        // `<form>` can only do the second. The body is borrowed here rather
        // than decoded: a non-UTF-8 body yields no field, so it is refused by
        // this gate with 403 exactly as it was before, and the 400 below
        // still names the real problem for a caller that got the token right.
        if is_state_changing(&method) && bearer.is_none() {
            let body_str = std::str::from_utf8(&body).unwrap_or("");
            if crate::routes::helpers::require_csrf_header_or_field(&session, &headers, body_str)
                .await
                .is_err()
            {
                return error_response(
                    StatusCode::FORBIDDEN,
                    "Invalid or missing CSRF token. Include an X-CSRF-Token header, a _token \
                     field in a form-urlencoded body, or authenticate with an \
                     Authorization: Bearer token.",
                );
            }
        }

        if body.len() > MAX_REQUEST_BODY {
            return error_response(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
        }
        let Ok(body) = String::from_utf8(body.to_vec()) else {
            return error_response(StatusCode::BAD_REQUEST, "Request body is not valid UTF-8");
        };
        (body, Vec::new())
    };

    // A token for the plugin to embed in a form it serves.
    //
    // Accepting `_token` is only half of a no-JS form: the plugin also has to be
    // able to *render* a valid token, and `tap_api` is a one-shot call with no
    // way to ask for one. So the kernel mints it here, before dispatch, and the
    // plugin writes it into a hidden input.
    //
    // Minted for a POST as well as a GET. Tokens are single-use, so a submission
    // that fails the plugin's own validation has already spent the one it
    // arrived with, and re-rendering the form needs a fresh one.
    //
    // Not minted for a bearer-authenticated caller: CSRF does not apply to it
    // (see the module docs), it is an API client rather than a browser, and
    // minting writes the session store on every request.
    let csrf_token = if bearer.is_none() {
        crate::form::csrf::generate_csrf_token(&session).await
    } else {
        String::new()
    };

    let mut request = ApiRequest::new(
        menu.callback.clone(),
        method.as_str(),
        uri.path(),
        user.id.to_string(),
        user.authenticated,
    );
    request.params = params;
    request.query = query;
    request.body = body;
    request.csrf_token = csrf_token;
    request.files = files;

    let Ok(payload) = serde_json::to_string(&request) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };

    // Dispatch with services attached — this is the whole point. A tap with no
    // DB handle could not write the table this exists to let it write.
    //
    // The stages and language a `query` read is made in are this request's:
    // the one stage resolver's answer for this viewer, and the negotiated
    // language (none for the site default, which needs no overlay).
    let stages = crate::routes::helpers::read_stages(
        &state,
        &user,
        crate::routes::helpers::session_stage(&session).await,
    )
    .await;
    let language = language.and_then(|Extension(lang)| {
        crate::routes::gather::query_language(&lang.0, state.default_language())
    });
    let tap_state =
        RequestState::new(user, state.tap_services().clone()).with_read_context(stages, language);
    let result = state
        .tap_dispatcher()
        .dispatch_to_plugin("tap_api", &payload, &menu.plugin, tap_state)
        .await;

    match result {
        Some(result) => match parse_api_response(&result.output) {
            Some(parsed) if parsed.theme => {
                themed_response(&state, &session, uri.path(), parsed).await
            }
            Some(parsed) => raw_response(parsed),
            None => {
                tracing::warn!(
                    plugin = %menu.plugin,
                    callback = %menu.callback,
                    "tap_api returned an unparseable response"
                );
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "Plugin returned an invalid response",
                )
            }
        },
        None => {
            tracing::warn!(
                plugin = %menu.plugin,
                callback = %menu.callback,
                "tap_api dispatch failed"
            );
            error_response(StatusCode::BAD_GATEWAY, "Plugin handler failed")
        }
    }
}

/// Whether `headers` declare a `multipart/form-data` body. Matches on the
/// media type alone, the same way `routes::helpers::is_form_urlencoded`
/// matches its content type, so a `boundary` or `charset` parameter does not
/// change the answer.
fn is_multipart_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("multipart/form-data")
        })
        .unwrap_or(false)
}

/// Fields and files read out of one multipart part stream, before any
/// authorization decision is made from them.
#[derive(Default)]
struct RawMultipart {
    /// Every non-file field, last value wins on a repeated name — the same
    /// rule [`ApiRequest::query`] and [`ApiRequest::params`] already apply to
    /// the request context.
    fields: HashMap<String, String>,
    /// Every value a field named [`crate::routes::helpers::CSRF_FORM_FIELD`]
    /// carried, in arrival order. More than one is a forged or malformed body,
    /// not a coin flip — resolved the same way the url-encoded body's
    /// `single_form_field` resolves a duplicate.
    token_values: Vec<String>,
    /// File parts, in arrival order: field name, filename, declared content
    /// type, bytes. Not yet stored — storing happens only once the caller's
    /// CSRF token has been checked.
    files: Vec<(String, String, Option<String>, Vec<u8>)>,
}

/// Read every part of a multipart body into memory, without storing any file
/// yet.
///
/// A body past the router's layered limit, or a malformed stream, fails here;
/// either way the answer is the same shape `read_upload_part` in
/// `routes::file` gives: 413 when the limit is what stopped it, 400 otherwise.
async fn read_raw_multipart(multipart: &mut Multipart) -> Result<RawMultipart, Response> {
    let mut raw = RawMultipart::default();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                return Err(error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("Request body too large (max {MAX_UPLOAD_BODY_SIZE} bytes)"),
                ));
            }
            Err(_) => {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "Failed to read multipart data",
                ));
            }
        };
        let name = field.name().unwrap_or("").to_string();
        if let Some(filename) = field.file_name().map(str::to_string) {
            let content_type = field.content_type().map(str::to_string);
            let data = match field.bytes().await {
                Ok(bytes) => bytes.to_vec(),
                Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                    return Err(error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        &format!("Request body too large (max {MAX_UPLOAD_BODY_SIZE} bytes)"),
                    ));
                }
                Err(_) => {
                    return Err(error_response(
                        StatusCode::BAD_REQUEST,
                        "Failed to read multipart data",
                    ));
                }
            };
            raw.files.push((name, filename, content_type, data));
        } else {
            let Ok(value) = field.text().await else {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "Failed to read multipart data",
                ));
            };
            if name == crate::routes::helpers::CSRF_FORM_FIELD {
                raw.token_values.push(value.clone());
            }
            raw.fields.insert(name, value);
        }
    }
    Ok(raw)
}

/// Parse a multipart plugin-served request: check CSRF against whatever
/// `_token` field it carried, then store each file part through
/// [`crate::file::FileService`] and return the non-file fields re-encoded as a
/// JSON object (the body a JSON caller would have sent) alongside the stored
/// files' ids and metadata.
///
/// **Ordering matters.** The whole body is read into memory first (buffering
/// cost no different from the `Bytes` branch, which already reads the whole
/// non-multipart body before anything is checked), but no file is written
/// through `FileService` — and so nothing is durable — until *after* the CSRF
/// check passes. A request failing that check costs memory, never storage.
async fn read_multipart_fields(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    needs_csrf: bool,
    uploader: uuid::Uuid,
    http_request: Request,
) -> Result<(String, Vec<UploadedFile>), Response> {
    let mut multipart = match Multipart::from_request(http_request, state).await {
        Ok(multipart) => multipart,
        Err(rejection) => return Err(rejection.into_response()),
    };
    let raw = read_raw_multipart(&mut multipart).await?;

    if needs_csrf {
        let token = match raw.token_values.as_slice() {
            [one] => Some(one.as_str()),
            _ => None,
        };
        if crate::routes::helpers::require_csrf_header_or_resolved_field(session, headers, token)
            .await
            .is_err()
        {
            return Err(error_response(
                StatusCode::FORBIDDEN,
                "Invalid or missing CSRF token. Include an X-CSRF-Token header, a _token \
                 field in the multipart body, or authenticate with an \
                 Authorization: Bearer token.",
            ));
        }
    }

    let mut files = Vec::with_capacity(raw.files.len());
    for (field, filename, content_type, data) in raw.files {
        let mime_type = crate::file::resolve_mime_type(content_type.as_deref(), &filename);
        match state
            .files()
            .upload(uploader, &filename, &mime_type, &data)
            .await
        {
            Ok(stored) => files.push(UploadedFile::new(
                field,
                stored.id.to_string(),
                stored.filename,
                stored.mime_type,
                stored.size,
                stored.url,
            )),
            Err(e) => {
                let status = crate::routes::file::upload_error_status(&e);
                if status == StatusCode::INTERNAL_SERVER_ERROR {
                    tracing::warn!(error = %e, field = %field, "plugin-served file upload failed");
                }
                return Err(error_response(status, &e.to_string()));
            }
        }
    }

    let mut fields = raw.fields;
    fields.remove(crate::routes::helpers::CSRF_FORM_FIELD);
    let Ok(body) = serde_json::to_string(&fields) else {
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
        ));
    };
    if body.len() > MAX_REQUEST_BODY {
        return Err(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request body too large",
        ));
    }

    Ok((body, files))
}

/// Decode a plugin's `tap_api` output.
///
/// `#[plugin_tap]` serializes a tap's return with `serde_json::to_string`, so an
/// `ApiResponse` arrives as a JSON object — but a `String`-returning tap would
/// arrive as a JSON *string*, so the same decode the view path needs applies
/// here (G-VIEW-OUTPUT-JSON-ENCODED). Both are accepted.
fn parse_api_response(raw: &str) -> Option<ApiResponse> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    // A JSON string envelope wrapping the real object.
    let value = match value {
        serde_json::Value::String(ref inner) => {
            serde_json::from_str::<serde_json::Value>(inner).unwrap_or(value)
        }
        other => other,
    };
    serde_json::from_value(value).ok()
}

/// Serve a plugin's body verbatim. The default, and unchanged behaviour.
fn raw_response(parsed: ApiResponse) -> Response {
    // A status outside the valid range is a plugin saying something
    // meaningless; serve 500 rather than silently clamping it to a code the
    // plugin never chose.
    let status = status_or_500(parsed.status);
    let content_type = HeaderValue::from_str(&parsed.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/json"));

    let mut response = (status, parsed.body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    // The plugin's own body, whatever its status: never rewritten into an
    // error page.
    response
        .extensions_mut()
        .insert(crate::middleware::PluginBody);
    response
}

/// Serve a plugin's body as page content inside the site's page template.
///
/// This is the seam a public-facing plugin page needs. `tap_api` output is served
/// as-is by default, which is right for an admin screen and wrong for a page a
/// visitor reaches: it arrives with no navigation, no header and no styling, and a
/// plugin has no way to reproduce those (`page.html` is the theme's, and the
/// site's may not be `page.html` at all).
///
/// The rendering is the same path an item page takes:
/// [`crate::routes::helpers::inject_site_context`] for the site context, then
/// [`crate::theme::ThemeEngine::render_page`] for template resolution, so a site
/// that overrides `page--contact.html` gets its override here too.
///
/// The body is **not** sanitized, exactly as it is not on the raw path — the
/// contract every view tap has. Theming changes what surrounds a plugin's HTML,
/// not what it is.
///
/// A template failure is the one server error page, as it is for every other
/// route: the theme that failed would be needed to make the plugin's body a page.
async fn themed_response(
    state: &AppState,
    session: &Session,
    path: &str,
    parsed: ApiResponse,
) -> Response {
    let mut context = tera::Context::new();
    let viewer = crate::routes::helpers::get_user_context(session, state).await;
    crate::routes::helpers::inject_site_context_for(state, session, &viewer, &mut context, path)
        .await;
    // A plugin's admin page gets the kernel's own sidebar context, the same
    // one `render_admin_template` builds for a kernel admin page: before this,
    // `page--admin.html` rendered for a plugin path with no `admin_nav` at
    // all, because this function is the one other place that template is
    // reached from and it never set it (BL-162). The rule is
    // `ThemeEngine::is_admin_path`, the same one `render_page` below uses to
    // pick `page--admin.html` as a suggestion and the themed error page uses
    // to decide the same thing.
    let shell = if crate::theme::ThemeEngine::is_admin_path(path) {
        Some(crate::routes::admin_nav::insert_context(state, &viewer, path, &mut context).await)
    } else {
        None
    };
    // The title is the page's heading as well as its `<title>`: a plugin page
    // arrived with no `<h1>` at all, because `render_page` puts the title in the
    // context and the theme's page template only used it in the document title.
    // `page_heading` is how a caller whose content carries no heading of its own
    // asks the template for one — an item or a gather page renders its own, so
    // they do not set it and do not get a second.
    if !parsed.title.trim().is_empty() {
        context.insert("page_heading", &parsed.title);
    }

    let mut render = || {
        state
            .theme()
            .render_page(path, &parsed.title, &parsed.body, &mut context)
    };
    let rendered = match &shell {
        // In the viewer's admin language, with the heading copied into the
        // title, as every kernel admin page is (`AdminShell::render`).
        Some(shell) => crate::i18n::scope(&shell.lang, render)
            .map(|html| crate::routes::admin_nav::finish_page(&html)),
        None => render(),
    };
    let html = match rendered {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(
                path = %path,
                error = %e,
                "failed to render a plugin page into the site theme"
            );
            return crate::routes::helpers::render_server_error(
                "This page could not be displayed.",
            );
        }
    };

    let mut response = (status_or_500(parsed.status), html).into_response();
    response
        .extensions_mut()
        .insert(crate::middleware::PluginBody);
    // A themed page is HTML whatever the plugin put in `content_type`.
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

/// A plugin's status code, or 500 when it named something that is not one.
fn status_or_500(status: u16) -> StatusCode {
    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// A kernel-authored JSON error, distinct in shape from a plugin's own body.
///
/// The `AppError` body every kernel route answers with (`code`, `message`,
/// `request_id`), plus `error` carrying the same string as `message`. Plugin
/// clients written before the shapes were unified read `error`; the key stays
/// beside `message` until 2.0 so they keep working.
fn error_response(status: StatusCode, message: &str) -> Response {
    let error = crate::error::AppError::with_status(status, message);
    let mut body = serde_json::to_value(error.body()).unwrap_or_default();
    if let Some(object) = body.as_object_mut() {
        object.insert("error".to_string(), serde_json::Value::from(message));
    }
    let mut response = (status, axum::Json(body)).into_response();
    // A browser gets the error page (or the login form for a 401); a script
    // keeps this body, `error` key included.
    response
        .extensions_mut()
        .insert(crate::middleware::ErrorPage {
            status,
            message: message.to_string(),
        });
    response
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn menu(path: &str, method: &str, callback: &str, handler_type: &str) -> MenuDefinition {
        MenuDefinition {
            path: path.to_string(),
            title: "t".into(),
            plugin: "p".into(),
            permission: String::new(),
            callback: callback.to_string(),
            parent: None,
            weight: 0,
            visible: false,
            method: method.to_string(),
            handler_type: handler_type.to_string(),
            local_task: false,
        }
    }

    #[test]
    fn menu_paths_translate_to_axum_syntax() {
        assert_eq!(to_axum_path("/argus/stories"), "/argus/stories");
        assert_eq!(
            to_axum_path("/argus/story/:id/react"),
            "/argus/story/{id}/react"
        );
        assert_eq!(to_axum_path("/a/:x/b/:y"), "/a/{x}/b/{y}");
    }

    #[test]
    fn only_state_changing_methods_need_a_token() {
        assert!(!is_state_changing(&Method::GET));
        assert!(!is_state_changing(&Method::HEAD));
        assert!(!is_state_changing(&Method::OPTIONS));
        assert!(is_state_changing(&Method::POST));
        assert!(is_state_changing(&Method::PUT));
        assert!(is_state_changing(&Method::DELETE));
    }

    #[test]
    fn the_router_registers_only_usable_api_entries() {
        // Building must not panic on any of these, and a duplicate must not
        // reach axum's route table (it panics on a conflict, and a plugin must
        // not be able to take the process down).
        let menus = vec![
            menu("/ok", "POST", "handler", "api"),
            menu("/ok", "POST", "handler", "api"), // duplicate
            menu("/page", "GET", "handler", "page"), // not an api entry
            menu("/nocallback", "POST", "", "api"),
            menu("relative", "POST", "handler", "api"),
            menu("/patch", "PATCH", "handler", "api"), // unsupported method
            menu("/ok", "GET", "handler", "api"),      // same path, other method
        ];
        let _router = build_plugin_api_router(&menus);
    }

    /// The defect two shipped plugins had: a callback on a page-style entry. The
    /// path 404s and the handler function is dead code, and nothing said so.
    #[test]
    fn a_page_entry_with_a_callback_is_reported_as_unreachable() {
        let menus = vec![
            MenuDefinition {
                path: "/rss/insights.xml".to_string(),
                title: "RSS".to_string(),
                plugin: "trovato_feeds".to_string(),
                permission: String::new(),
                callback: "feed_insights".to_string(),
                parent: None,
                weight: 0,
                visible: true,
                method: "GET".to_string(),
                handler_type: "page".to_string(),
                local_task: false,
            },
            MenuDefinition {
                path: "/tpa/notes".to_string(),
                title: "Notes".to_string(),
                plugin: "test_plugin_api".to_string(),
                permission: String::new(),
                callback: "list_notes".to_string(),
                parent: None,
                weight: 0,
                visible: true,
                method: "GET".to_string(),
                handler_type: "api".to_string(),
                local_task: false,
            },
            MenuDefinition {
                path: "/admin/config/seo".to_string(),
                title: "SEO".to_string(),
                plugin: "trovato_seo".to_string(),
                permission: "administer seo".to_string(),
                callback: String::new(),
                parent: None,
                weight: 0,
                visible: true,
                method: "GET".to_string(),
                handler_type: "page".to_string(),
                local_task: false,
            },
        ];

        let reported = unreachable_callbacks(&menus);

        assert_eq!(
            reported,
            vec![("trovato_feeds".to_string(), "/rss/insights.xml".to_string())],
            "only the page entry that names a callback is unreachable"
        );
    }

    /// A whitespace-only callback is not a declaration, and must not be reported:
    /// the router skips it for the same reason.
    #[test]
    fn a_blank_callback_is_not_reported() {
        let menus = vec![MenuDefinition {
            path: "/blank".to_string(),
            title: "Blank".to_string(),
            plugin: "p".to_string(),
            permission: String::new(),
            callback: "   ".to_string(),
            parent: None,
            weight: 0,
            visible: true,
            method: "GET".to_string(),
            handler_type: "page".to_string(),
            local_task: false,
        }];

        assert!(unreachable_callbacks(&menus).is_empty());
    }

    #[test]
    fn a_plugin_response_is_parsed_from_either_envelope() {
        let object = r#"{"status":201,"body":"{\"ok\":true}","content_type":"application/json"}"#;
        let response = raw_response(parse_api_response(object).expect("object envelope"));
        assert_eq!(response.status(), StatusCode::CREATED);

        // What a `String`-returning tap would produce.
        let wrapped = serde_json::to_string(object).unwrap();
        let response = raw_response(parse_api_response(&wrapped).expect("string envelope"));
        assert_eq!(response.status(), StatusCode::CREATED);

        assert!(parse_api_response("not json").is_none());
        assert!(parse_api_response(r#"{"nope":1}"#).is_none());
    }

    #[test]
    fn an_out_of_range_status_does_not_panic() {
        let raw = r#"{"status":9999,"body":"","content_type":"application/json"}"#;
        let response = raw_response(parse_api_response(raw).expect("parses"));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn a_missing_content_type_defaults_to_json() {
        let raw = r#"{"status":200,"body":"{}"}"#;
        let response = raw_response(parse_api_response(raw).expect("parses"));
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    /// Theming is opt-in, so a response that does not ask for it is not themed —
    /// which is what keeps every existing admin screen and JSON endpoint byte
    /// identical.
    #[test]
    fn a_response_that_does_not_ask_for_the_theme_does_not_get_it() {
        let raw = r#"{"status":200,"body":"<p>hi</p>","content_type":"text/html"}"#;
        let parsed = parse_api_response(raw).expect("parses");
        assert!(!parsed.theme, "theme must default to false");

        // And the whole existing corpus of plugin responses predates the field.
        let older = r#"{"status":200,"body":"{}"}"#;
        assert!(!parse_api_response(older).expect("parses").theme);
    }

    #[test]
    fn a_response_can_ask_for_the_theme_and_carry_a_title() {
        let raw = r#"{"status":200,"body":"<p>hi</p>","content_type":"text/html","theme":true,"title":"Contact"}"#;
        let parsed = parse_api_response(raw).expect("parses");
        assert!(parsed.theme);
        assert_eq!(parsed.title, "Contact");
    }

    /// The SDK constructor and the wire format have to agree, or a plugin asks for
    /// theming and the kernel does not hear it.
    #[test]
    fn the_sdk_themed_constructor_serializes_to_what_the_kernel_reads() {
        let response = ApiResponse::themed("Contact", "<p>hi</p>");
        let json = serde_json::to_string(&response).expect("serialize");

        let parsed = parse_api_response(&json).expect("parses");
        assert!(parsed.theme);
        assert_eq!(parsed.title, "Contact");
        assert_eq!(parsed.body, "<p>hi</p>");
        assert_eq!(parsed.status, 200);
    }

    #[test]
    fn a_themed_response_can_carry_a_status_of_its_own() {
        let response = ApiResponse::themed_with_status(422, "Try again", "<p>bad input</p>");
        let json = serde_json::to_string(&response).expect("serialize");

        let parsed = parse_api_response(&json).expect("parses");
        assert!(parsed.theme);
        assert_eq!(parsed.status, 422);
        assert_eq!(parsed.title, "Try again");
    }
}
