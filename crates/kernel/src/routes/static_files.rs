//! Static file serving.

use axum::{
    Router,
    body::Body,
    extract::Path,
    http::{Response, StatusCode, header},
    routing::get,
};
use std::path::PathBuf;
use tokio::fs;
use tracing::warn;

use crate::state::AppState;

/// Create the static files router.
pub fn router() -> Router<AppState> {
    Router::new().route("/static/{*path}", get(serve_static))
}

/// Serve a static file.
async fn serve_static(Path(path): Path<String>) -> Response<Body> {
    // Security: prevent path traversal
    let path = path.trim_start_matches('/');
    if path.contains("..") || path.contains('\0') {
        return not_found();
    }

    // Resolve path relative to static directory
    let static_dir = std::env::var("STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./static"));

    let file_path = static_dir.join(path);

    // Read file
    let content = match fs::read(&file_path).await {
        Ok(content) => content,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %file_path.display(), error = %e, "failed to read static file");
            }
            return not_found();
        }
    };

    // Determine content type
    let content_type = mime_from_path(&file_path);

    // SAFETY: Response::builder() with hard-coded valid status and headers cannot fail
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=86400") // 1 day cache
        .body(Body::from(content))
        .unwrap()
}

fn not_found() -> Response<Body> {
    // SAFETY: Response::builder() with hard-coded valid status cannot fail
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("Not found"))
        .unwrap()
}

fn mime_from_path(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("html") => "text/html",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}
