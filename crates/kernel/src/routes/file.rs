//! File upload route handlers.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use tower_sessions::Session;
use tracing::warn;
use uuid::Uuid;

use crate::content::FileAccess;
use crate::file::{
    FileStream, MAX_FILE_SIZE, MAX_UPLOAD_BODY_SIZE, UploadRejected, UploadResult,
    is_range_not_satisfiable, mime_for_filename, parse_range_header, resolve_mime_type,
};
use crate::state::AppState;

/// Allowed image MIME types for block editor uploads.
const BLOCK_EDITOR_IMAGE_TYPES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Create the file router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/file/upload",
            post(upload_file).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY_SIZE)),
        )
        .route("/file/{id}", get(get_file_info))
        .route("/files/{*path}", get(serve_uploaded_file))
}

/// Create the block editor API router (gated by `block_editor` plugin).
pub fn block_editor_api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("block-editor")
        .route(
            axum::http::Method::POST,
            "/block-editor/upload",
            post(block_editor_upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY_SIZE)),
            doc("Upload an image from the block editor (multipart; `X-CSRF-Token`)").alias(),
        )
        .post(
            "/block-editor/preview",
            block_editor_preview,
            doc("Render blocks as the page will").alias(),
        )
        .plugin("trovato_block_editor")
}

/// Upload response.
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<UploadResult>,
}

/// Upload a file.
///
/// POST /file/upload
/// Content-Type: multipart/form-data
///
/// Form fields:
/// - file: The file to upload
async fn upload_file(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    // Require authentication
    let user_id = match crate::routes::helpers::require_login_json(&state, &session).await {
        Ok(user) => user.id,
        Err(refusal) => return refusal.into_response(),
    };

    // Verify CSRF token from header
    if let Err(refusal) = crate::routes::helpers::require_csrf_header(&session, &headers).await {
        return refusal.into_response();
    }

    // Read the first `file` part.
    let (filename, content_type, data) = match read_upload_part(&mut multipart, &["file"]).await {
        Ok(part) => part,
        Err((status, message)) => {
            return crate::error::AppError::with_status(status, message).into_response();
        }
    };

    // Validate we got a file
    let Some(filename) = filename else {
        return crate::error::AppError::with_status(StatusCode::BAD_REQUEST, "No file provided")
            .into_response();
    };

    let Some(data) = data else {
        return crate::error::AppError::with_status(StatusCode::BAD_REQUEST, "Empty file")
            .into_response();
    };

    // The declared type, or the one the name's extension gives. Size, the
    // allowlist, the content and the extension are all checked once, by the
    // service.
    let mime_type = resolve_mime_type(content_type.as_deref(), &filename);

    // Upload file
    match state
        .files()
        .upload(user_id, &filename, &mime_type, &data)
        .await
    {
        Ok(result) => (
            StatusCode::OK,
            Json(UploadResponse {
                success: true,
                file: Some(result),
            }),
        )
            .into_response(),
        Err(e) => {
            let status = upload_error_status(&e);
            if status == StatusCode::INTERNAL_SERVER_ERROR {
                warn!(error = %e, "file upload failed");
            }
            crate::error::AppError::with_status(status, e.to_string()).into_response()
        }
    }
}

/// File info response.
#[derive(Debug, Serialize)]
pub struct FileInfoResponse {
    pub id: Uuid,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    pub url: String,
    pub created: i64,
}

/// `Cache-Control` for a response a shared cache must never keep.
pub(crate) const PRIVATE_NO_STORE: &str = "private, no-store";

/// Pick the `Cache-Control` value for serving the file at `uri` to `viewer`,
/// who has already been allowed to fetch it.
///
/// A shared cache (CDN, reverse proxy) hands a stored response to whoever asks
/// next, so the long public value is only safe when an anonymous visitor would
/// be allowed the same URI. Anything else, including a failed check, is
/// `private, no-store`.
pub(crate) async fn served_file_cache_control(
    state: &AppState,
    uri: &str,
    viewer: &crate::tap::UserContext,
    public_max_age: u32,
) -> String {
    let public = if viewer.authenticated {
        let anonymous = crate::routes::helpers::anonymous_user_context(state).await;
        match state.items().can_serve_file(uri, &anonymous).await {
            Ok(allowed) => allowed,
            Err(e) => {
                warn!(error = %e, uri = %uri, "failed to check anonymous file access");
                false
            }
        }
    } else {
        // The viewer is anonymous and was already allowed.
        true
    };
    if public {
        format!("public, max-age={public_max_age}")
    } else {
        PRIVATE_NO_STORE.to_string()
    }
}

/// Whether `path` (the part of a `/files/` URL after the prefix) may name an
/// original upload.
///
/// Each `/`-separated component is checked: an empty component or a `.`
/// component would let two spellings name one file, and the `styles/` tree
/// holds image derivatives, which are only served through the image style route
/// (it checks access against the original). Callers still apply their own
/// traversal guard.
///
/// Trovato's own working names are refused too: the startup write check's
/// `.trovato-write-check-*` and a local write's `.trovato-tmp-*`, which
/// a crash can leave behind and which have no `file_managed` row, so the
/// access check would otherwise treat them as unmanaged files anyone may fetch.
pub(crate) fn is_original_upload_path(path: &str) -> bool {
    !path
        .split('/')
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case("styles"))
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && !is_working_name(c))
}

/// Whether a path with no `file_managed` row may be served as an unmanaged
/// file: not when it is a managed file spelled in other letter case, which a
/// case-insensitive directory would read as that (perhaps private) file.
pub(crate) async fn unmanaged_is_servable(state: &AppState, uri: &str) -> anyhow::Result<bool> {
    Ok(!state.files().is_other_spelling_of_managed(uri).await?)
}

/// The `Range: bytes=...` header of a request, parsed to the one range it
/// asks for (`None` for no header, several ranges, or a value this server does
/// not understand: each served as the whole file, `200`).
pub(crate) fn requested_range(headers: &HeaderMap) -> Option<crate::file::ByteRange> {
    headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range_header)
}

/// Build the response for a [`FileStream`] already resolved against a
/// request's range: `200` with the whole body when no `Range` header was
/// understood, `206` with `Content-Range` when one was. Shared by the plain
/// file route and image style derivatives, so both answer a `Range` request
/// the same way.
pub(crate) fn stream_response(
    stream: FileStream,
    had_range_request: bool,
    mut headers: Vec<(axum::http::HeaderName, String)>,
) -> Response {
    let status = if had_range_request {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    headers.push((
        axum::http::header::CONTENT_LENGTH,
        (stream.end - stream.start).to_string(),
    ));
    headers.push((axum::http::header::ACCEPT_RANGES, "bytes".to_string()));
    if had_range_request {
        headers.push((
            axum::http::header::CONTENT_RANGE,
            format!(
                "bytes {}-{}/{}",
                stream.start,
                stream.end.saturating_sub(1),
                stream.total
            ),
        ));
    }
    let mut builder = axum::http::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    // Infallible: a valid status and ASCII header values cannot fail to build.
    #[allow(clippy::unwrap_used)]
    builder
        .body(axum::body::Body::from_stream(stream.stream))
        .unwrap()
        .into_response()
}

/// `416 Range Not Satisfiable`, for a `Range` request naming bytes past the
/// end of a file of `total` bytes.
pub(crate) fn range_not_satisfiable_response(total: u64) -> Response {
    // Infallible: a valid status and ASCII header value cannot fail to build.
    #[allow(clippy::unwrap_used)]
    axum::http::Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(
            axum::http::header::CONTENT_RANGE,
            format!("bytes */{total}"),
        )
        .body(axum::body::Body::empty())
        .unwrap()
        .into_response()
}

/// One of Trovato's own working file names (see [`is_original_upload_path`]).
fn is_working_name(c: &str) -> bool {
    // Case-blind, for a case-insensitive local filesystem.
    let c = c.to_ascii_lowercase();
    c.starts_with(".trovato-write-check-") || c.starts_with(".trovato-tmp-")
}

/// Get file info.
///
/// GET /file/{id}
///
/// Answers only for a file the viewer may fetch; anything else is 404, so a
/// file id reveals nothing about a file the caller cannot see.
async fn get_file_info(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Response {
    match state.files().get(id).await {
        Ok(Some(file)) => {
            let viewer = crate::routes::item::get_user_context(&session, &state).await;
            match state.items().can_serve_file(&file.uri, &viewer).await {
                Ok(true) => {}
                Ok(false) => return StatusCode::NOT_FOUND.into_response(),
                Err(e) => {
                    warn!(error = %e, id = %id, "failed to authorize file info");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
            let url = state.files().storage().public_url(&file.uri);
            Json(FileInfoResponse {
                id: file.id,
                filename: file.filename,
                mime_type: file.filemime,
                size: file.filesize,
                url,
                created: file.created,
            })
            .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            warn!(error = %e, "failed to get file info");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Serve an uploaded file from the uploads directory.
///
/// GET /files/{*path}
///
/// Path segments map to the storage URI after the `local://` prefix.
/// Rejects directory traversal attempts.
async fn serve_uploaded_file(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Response {
    let path = path.trim_start_matches('/');

    // Reject directory traversal and path injection attempts.
    // Axum's Path extractor URL-decodes, so %2e%2e becomes ".." before we see it.
    if path.is_empty()
        || path.contains("..")
        || path.contains('\0')
        || path.contains('\\')
        || path.starts_with('/')
        || !is_original_upload_path(path)
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    let uri = format!("local://{path}");

    // FR-8 Story 3.5 — enforce access before streaming bytes (any-referencing
    // policy, D-29). A denied or unresolvable file returns 404, matching this
    // surface's existing not-found posture (no existence leak).
    //
    // The served type is the managed row's stored `filemime`, validated at
    // upload; only an unmanaged path falls back to the extension table.
    let viewer = crate::routes::item::get_user_context(&session, &state).await;
    let content_type = match state.items().file_access(&uri, &viewer).await {
        Ok(FileAccess::Allowed { filemime }) => filemime,
        Ok(FileAccess::Unmanaged) => {
            // BG16 follow-up: a ported or hosted site that cannot promise
            // every stored object has a `file_managed` row sets
            // `SERVE_UNMANAGED_FILES=false`, so an unregistered private file
            // is refused before storage is ever read.
            if !state.runtime().serve_unmanaged_files {
                return StatusCode::NOT_FOUND.into_response();
            }
            match unmanaged_is_servable(&state, &uri).await {
                Ok(true) => mime_for_filename(path)
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                Ok(false) => return StatusCode::NOT_FOUND.into_response(),
                Err(e) => {
                    warn!(error = %format!("{e:#}"), path = %path, "failed to authorize file access");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        }
        Ok(FileAccess::Denied) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            warn!(error = %e, path = %path, "failed to authorize file access");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let range = requested_range(&headers);
    match state.files().storage().read_stream(&uri, range).await {
        Ok(stream) => {
            let cache_control = served_file_cache_control(&state, &uri, &viewer, 604_800).await;

            // Use inline disposition for images/PDFs, attachment for everything else
            // to prevent browsers from executing downloaded content.
            let disposition =
                if content_type.starts_with("image/") || content_type == "application/pdf" {
                    "inline"
                } else {
                    "attachment"
                };

            stream_response(
                stream,
                range.is_some(),
                vec![
                    (axum::http::header::CONTENT_TYPE, content_type),
                    (axum::http::header::CACHE_CONTROL, cache_control),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        disposition.to_string(),
                    ),
                    (
                        axum::http::header::X_CONTENT_TYPE_OPTIONS,
                        "nosniff".to_string(),
                    ),
                ],
            )
        }
        Err(e) => {
            if let Some(total) = is_range_not_satisfiable(&e) {
                return range_not_satisfiable_response(total);
            }
            if crate::file::is_not_found(&e) {
                return StatusCode::NOT_FOUND.into_response();
            }
            warn!(error = %e, path = %path, "failed to serve uploaded file");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Block editor image upload.
///
/// POST /api/block-editor/upload
///
/// Returns Editor.js-compatible response:
/// `{ success: 1, file: { url: "..." } }` on success
/// `{ success: 0 }` on failure
async fn block_editor_upload(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    // Require authentication
    let user_id = match crate::routes::helpers::require_login_json(&state, &session).await {
        Ok(user) => user.id,
        Err(refusal) => return refusal.into_response(),
    };

    // Verify CSRF token from header
    if let Err(refusal) = crate::routes::helpers::require_csrf_header(&session, &headers).await {
        return refusal.into_response();
    }

    let (filename, content_type, data) =
        match read_upload_part(&mut multipart, &["image", "file"]).await {
            Ok(part) => part,
            Err((status, _)) => {
                return (status, Json(serde_json::json!({ "success": 0 }))).into_response();
            }
        };

    let Some(filename) = filename else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": 0 })),
        )
            .into_response();
    };

    let Some(data) = data else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": 0 })),
        )
            .into_response();
    };

    // Determine and validate MIME type (images only)
    let mime_type = resolve_mime_type(content_type.as_deref(), &filename);

    if !BLOCK_EDITOR_IMAGE_TYPES.contains(&mime_type.as_str()) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({ "success": 0 })),
        )
            .into_response();
    }

    // Upload via FileService
    match state
        .files()
        .upload(user_id, &filename, &mime_type, &data)
        .await
    {
        Ok(result) => {
            // Editor.js format
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": 1,
                    "file": {
                        "url": result.url
                    }
                })),
            )
                .into_response()
        }
        Err(e) => {
            let status = upload_error_status(&e);
            if status == StatusCode::INTERNAL_SERVER_ERROR {
                warn!(error = %e, "block editor image upload failed");
            }
            (status, Json(serde_json::json!({ "success": 0 }))).into_response()
        }
    }
}

/// Block editor preview endpoint.
///
/// POST /api/block-editor/preview
///
/// Accepts JSON block data and returns rendered HTML.
async fn block_editor_preview(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // Require authentication
    if let Err(refusal) = crate::routes::helpers::require_login_json(&state, &session).await {
        return refusal.into_response();
    }

    // Verify CSRF token from header
    if let Err(refusal) = crate::routes::helpers::require_csrf_header(&session, &headers).await {
        return refusal.into_response();
    }

    let blocks = body
        .get("blocks")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();

    let html = crate::content::block_render::render_blocks(&blocks);

    Json(serde_json::json!({ "html": html })).into_response()
}

/// The first multipart part named one of `names`: its filename, declared
/// content type and bytes, each `None` when absent.
///
/// A body past the route's `MAX_UPLOAD_BODY_SIZE` limit can fail on reading the
/// part or on reaching it; either way the answer is 413 saying the file is too
/// large, not "no file" or "failed to read".
async fn read_upload_part(
    multipart: &mut Multipart,
    names: &[&str],
) -> Result<(Option<String>, Option<String>, Option<Vec<u8>>), (StatusCode, String)> {
    let refused = |e: axum::extract::multipart::MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("File too large (max {MAX_FILE_SIZE} bytes)"),
            )
        } else {
            warn!(error = %e, "failed to read upload data");
            (
                StatusCode::BAD_REQUEST,
                "Failed to read file data".to_string(),
            )
        }
    };
    while let Some(field) = multipart.next_field().await.map_err(refused)? {
        if names.contains(&field.name().unwrap_or("")) {
            let filename = field.file_name().map(str::to_string);
            let content_type = field.content_type().map(str::to_string);
            let bytes = field.bytes().await.map_err(refused)?;
            return Ok((filename, content_type, Some(bytes.to_vec())));
        }
    }
    Ok((None, None, None))
}

/// The status for an upload the service refused: 413 or 415 for a file at
/// fault ([`UploadRejected`]), 500 for anything else.
///
/// `pub(crate)`: the plugin-api route reuses this for the same decision on a
/// multipart file part, so the two surfaces answer identically for the same
/// refusal.
pub(crate) fn upload_error_status(e: &anyhow::Error) -> StatusCode {
    match e.downcast_ref::<UploadRejected>() {
        Some(UploadRejected::TooLarge(_)) => StatusCode::PAYLOAD_TOO_LARGE,
        Some(UploadRejected::Type(_)) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        None => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_upload_path_accepts_ordinary_paths() {
        assert!(is_original_upload_path("2026/09/abc_photo.jpg"));
        assert!(is_original_upload_path("photo.jpg"));
        assert!(is_original_upload_path("file..name.txt"));
        assert!(is_original_upload_path("stylesheet/notes.txt"));
    }

    #[test]
    fn original_upload_path_rejects_trovato_working_names() {
        assert!(!is_original_upload_path(".trovato-write-check-0197"));
        assert!(!is_original_upload_path(".Trovato-Write-Check-0197"));
        assert!(!is_original_upload_path("Styles/w400/photo.jpg"));
        assert!(!is_original_upload_path("STYLES/w400/photo.jpg"));
        assert!(!is_original_upload_path("2026/11/.trovato-tmp-0197abc"));
        assert!(is_original_upload_path("2026/11/a_trovato-tmp.jpg"));
        assert!(is_original_upload_path(".well-known/x.txt"));
    }

    #[test]
    fn original_upload_path_rejects_dot_empty_and_styles() {
        assert!(!is_original_upload_path("./styles/w400/photo.jpg"));
        assert!(!is_original_upload_path("2026/./photo.jpg"));
        assert!(!is_original_upload_path("styles//w400/photo.jpg"));
        assert!(!is_original_upload_path("2026//photo.jpg"));
        assert!(!is_original_upload_path("2026/09/"));
        assert!(!is_original_upload_path(""));
        assert!(!is_original_upload_path("styles/w400/photo.jpg"));
        assert!(!is_original_upload_path("styles"));
    }
}
