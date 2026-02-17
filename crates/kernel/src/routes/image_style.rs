//! Image style routes.
//!
//! On-demand image derivative generation route.

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};

use crate::state::AppState;

/// Create the image style routes.
pub fn router() -> Router<AppState> {
    Router::new().route("/files/styles/{style_name}/{*path}", get(serve_derivative))
}

/// Validate an image path to prevent directory traversal attacks.
///
/// Uses component-by-component validation rather than substring matching
/// to prevent normalization bypass attacks.
fn validate_image_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    // Reject null bytes
    if path.contains('\0') {
        return false;
    }
    // Reject absolute paths (Unix and Windows)
    if path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return false;
    }
    // Validate each path component individually
    for component in path.split(['/', '\\']) {
        if component == ".." || component == "." {
            return false;
        }
    }
    true
}

/// GET /files/styles/{style_name}/{path} — serve or generate image derivative.
async fn serve_derivative(
    State(state): State<AppState>,
    Path((style_name, file_path)): Path<(String, String)>,
) -> impl IntoResponse {
    if !validate_image_path(&file_path) || !validate_image_path(&style_name) {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    }

    let Some(image_service) = state.image_styles() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Image styles not enabled").into_response();
    };

    // Try reading from disk cache directly (avoids TOCTOU race with separate exists + read)
    let cache_path = image_service.cache_path(&style_name, &file_path);
    match tokio::fs::read(&cache_path).await {
        Ok(data) => {
            // Derivatives are always JPEG regardless of the original file extension.
            let content_type = "image/jpeg".to_string();
            return (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    (
                        header::CACHE_CONTROL,
                        "public, max-age=31536000".to_string(),
                    ),
                ],
                Body::from(data),
            )
                .into_response();
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Not cached yet, generate below
        }
        Err(e) => {
            tracing::debug!(error = %e, "failed to read cached derivative, regenerating");
            // Fall through to regeneration
        }
    }

    // Load style from DB
    let style = match image_service.load_style(&style_name).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "Image style not found").into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load image style");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load style").into_response();
        }
    };

    // Load original file from FileStorage
    let original = state.files().load_file_data(&file_path).await;
    let original = match original {
        Ok(Some(data)) => data,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "Original file not found").into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %file_path, "failed to load original file");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load file").into_response();
        }
    };

    // Acquire processing permit to limit concurrent CPU-intensive operations
    let _permit = match image_service.acquire_processing_permit().await {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Image processing unavailable",
            )
                .into_response();
        }
    };

    // Process through style effects on a blocking thread to avoid starving
    // the Tokio runtime with CPU-intensive image decoding/encoding.
    let svc = image_service.clone();
    let sn = style_name.clone();
    let fp = file_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let derivative = svc.process_image(&original, &style)?;
        // Save to disk cache while still on the blocking thread
        if let Err(e) = svc.save_derivative(&sn, &fp, &derivative) {
            tracing::warn!(error = %e, "failed to cache derivative");
        }
        Ok::<Vec<u8>, anyhow::Error>(derivative)
    })
    .await;

    let derivative = match result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "failed to process image");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to process image").into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "image processing task panicked");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to process image").into_response();
        }
    };

    let content_type = "image/jpeg".to_string(); // Derivatives are always JPEG
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000".to_string(),
            ),
        ],
        Body::from(derivative),
    )
        .into_response()
}

/// Guess content type from file extension.
#[cfg(test)]
fn guess_content_type(path: &str) -> String {
    match path.rsplit('.').next() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_guessing() {
        assert_eq!(guess_content_type("photo.jpg"), "image/jpeg");
        assert_eq!(guess_content_type("photo.png"), "image/png");
        assert_eq!(guess_content_type("photo.webp"), "image/webp");
        assert_eq!(guess_content_type("file.txt"), "application/octet-stream");
    }

    #[test]
    fn path_validation_rejects_traversal() {
        assert!(!validate_image_path(""));
        assert!(!validate_image_path("../etc/passwd"));
        assert!(!validate_image_path("foo/../../etc/passwd"));
        assert!(!validate_image_path("/etc/passwd"));
        assert!(!validate_image_path("\\windows\\system32"));
        assert!(!validate_image_path("C:file.jpg"));
        assert!(!validate_image_path("foo\0bar.jpg"));
        assert!(!validate_image_path("./foo/bar.jpg"));
        assert!(!validate_image_path("foo/./bar.jpg"));
        assert!(!validate_image_path("foo\\..\\bar.jpg"));
    }

    #[test]
    fn path_validation_accepts_valid() {
        assert!(validate_image_path("photo.jpg"));
        assert!(validate_image_path("uploads/2024/photo.jpg"));
        assert!(validate_image_path("my-image_001.png"));
        // Filenames with dots are fine (component-based check allows this)
        assert!(validate_image_path("file..name.jpg"));
    }
}
