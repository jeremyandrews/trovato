//! File upload route handlers.

use axum::{
    Json, Router,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use tower_sessions::Session;
use tracing::warn;
use uuid::Uuid;

use crate::file::{ALLOWED_MIME_TYPES, MAX_FILE_SIZE, UploadResult};
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

/// Allowed image MIME types for block editor uploads.
const BLOCK_EDITOR_IMAGE_TYPES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Create the file router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/file/upload", post(upload_file))
        .route("/file/{id}", get(get_file_info))
        .route("/api/block-editor/upload", post(block_editor_upload))
        .route("/api/block-editor/preview", post(block_editor_preview))
}

/// Upload response.
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<UploadResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    mut multipart: Multipart,
) -> Response {
    // Require authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let Some(user_id) = user_id else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(UploadResponse {
                success: false,
                file: None,
                error: Some("Authentication required".to_string()),
            }),
        )
            .into_response();
    };

    // Process multipart form
    let mut filename: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();

        if name == "file" {
            filename = field.file_name().map(|s| s.to_string());
            content_type = field.content_type().map(|s| s.to_string());

            match field.bytes().await {
                Ok(bytes) => {
                    // Check size limit early
                    if bytes.len() > MAX_FILE_SIZE {
                        return (
                            StatusCode::PAYLOAD_TOO_LARGE,
                            Json(UploadResponse {
                                success: false,
                                file: None,
                                error: Some(format!(
                                    "File too large: {} bytes (max {} bytes)",
                                    bytes.len(),
                                    MAX_FILE_SIZE
                                )),
                            }),
                        )
                            .into_response();
                    }
                    data = Some(bytes.to_vec());
                }
                Err(e) => {
                    warn!(error = %e, "failed to read upload data");
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(UploadResponse {
                            success: false,
                            file: None,
                            error: Some("Failed to read file data".to_string()),
                        }),
                    )
                        .into_response();
                }
            }
            break; // Only process first file
        }
    }

    // Validate we got a file
    let Some(filename) = filename else {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                success: false,
                file: None,
                error: Some("No file provided".to_string()),
            }),
        )
            .into_response();
    };

    let Some(data) = data else {
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadResponse {
                success: false,
                file: None,
                error: Some("Empty file".to_string()),
            }),
        )
            .into_response();
    };

    // Determine MIME type
    let mime_type = content_type
        .or_else(|| guess_mime_type(&filename))
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Check allowed MIME types
    if !ALLOWED_MIME_TYPES.contains(&mime_type.as_str()) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(UploadResponse {
                success: false,
                file: None,
                error: Some(format!("File type not allowed: {mime_type}")),
            }),
        )
            .into_response();
    }

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
                error: None,
            }),
        )
            .into_response(),
        Err(e) => {
            warn!(error = %e, "file upload failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UploadResponse {
                    success: false,
                    file: None,
                    error: Some(e.to_string()),
                }),
            )
                .into_response()
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

/// Get file info.
///
/// GET /file/{id}
async fn get_file_info(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match state.files().get(id).await {
        Ok(Some(file)) => {
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
    mut multipart: Multipart,
) -> Response {
    // Require authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let Some(user_id) = user_id else {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "success": 0 })),
        )
            .into_response();
    };

    // Process multipart form
    let mut filename: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();

        if name == "image" || name == "file" {
            filename = field.file_name().map(|s| s.to_string());
            content_type = field.content_type().map(|s| s.to_string());

            match field.bytes().await {
                Ok(bytes) => {
                    if bytes.len() > MAX_FILE_SIZE {
                        return (
                            StatusCode::PAYLOAD_TOO_LARGE,
                            Json(serde_json::json!({ "success": 0 })),
                        )
                            .into_response();
                    }
                    data = Some(bytes.to_vec());
                }
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "success": 0 })),
                    )
                        .into_response();
                }
            }
            break;
        }
    }

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
    let mime_type = content_type
        .or_else(|| guess_mime_type(&filename))
        .unwrap_or_else(|| "application/octet-stream".to_string());

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
            warn!(error = %e, "block editor image upload failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "success": 0 })),
            )
                .into_response()
        }
    }
}

/// Block editor preview endpoint.
///
/// POST /api/block-editor/preview
///
/// Accepts JSON block data and returns rendered HTML.
async fn block_editor_preview(
    State(_state): State<AppState>,
    session: Session,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // Require authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    if user_id.is_none() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let blocks = body
        .get("blocks")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();

    let html = crate::content::block_render::render_blocks(&blocks);

    Json(serde_json::json!({ "html": html })).into_response()
}

/// Guess MIME type from filename extension.
fn guess_mime_type(filename: &str) -> Option<String> {
    let ext = filename.rsplit('.').next()?.to_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "gz" | "gzip" => "application/gzip",
        _ => return None,
    };
    Some(mime.to_string())
}
