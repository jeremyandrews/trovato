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

    // Infallible: Response::builder() with hard-coded valid status and headers cannot fail
    #[allow(clippy::unwrap_used)]
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=86400") // 1 day cache
        .body(Body::from(content))
        .unwrap()
}

fn not_found() -> Response<Body> {
    // Infallible: Response::builder() with hard-coded valid status cannot fail
    #[allow(clippy::unwrap_used)]
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("Not found"))
        .unwrap()
}

/// Build an asset manifest mapping original paths to content-hashed paths.
///
/// Scans the static directory, computes SHA-256 of each file, and creates
/// a mapping like `css/theme.css` → `css/theme.a1b2c3d4.css`.
/// The short hash (first 8 hex chars) is inserted before the extension.
pub fn build_asset_manifest() -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;

    let static_dir = std::env::var("STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./static"));

    let mut manifest = HashMap::new();

    // Walk the static directory
    let Ok(entries) = std::fs::read_dir(&static_dir) else {
        return manifest;
    };

    for entry in entries.flatten() {
        scan_dir_recursive(&static_dir, &entry.path(), &mut manifest);
    }

    manifest
}

fn scan_dir_recursive(
    base: &std::path::Path,
    path: &std::path::Path,
    manifest: &mut std::collections::HashMap<String, String>,
) {
    use sha2::{Digest, Sha256};

    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                scan_dir_recursive(base, &entry.path(), manifest);
            }
        }
        return;
    }

    let Ok(content) = std::fs::read(path) else {
        return;
    };

    let mut hasher = Sha256::new();
    hasher.update(&content);
    let hash = hex::encode(hasher.finalize());
    let short_hash = &hash[..8];

    // Get relative path from static dir
    let Ok(relative) = path.strip_prefix(base) else {
        return;
    };
    let relative_str = relative.to_string_lossy().to_string();

    // Build hashed path: "css/theme.css" → "css/theme.a1b2c3d4.css"
    let hashed = if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let stem = relative_str
            .strip_suffix(&format!(".{ext}"))
            .unwrap_or(&relative_str);
        format!("{stem}.{short_hash}.{ext}")
    } else {
        format!("{relative_str}.{short_hash}")
    };

    manifest.insert(relative_str, hashed);
}

/// Register the `asset_url` Tera function using the manifest.
///
/// Usage in templates: `{{ asset_url(path="css/theme.css") }}`
/// Returns `/static/css/theme.a1b2c3d4.css` if hashed, or `/static/css/theme.css` as fallback.
pub fn register_asset_url_function(
    tera: &mut tera::Tera,
    manifest: std::collections::HashMap<String, String>,
) {
    let manifest = std::sync::Arc::new(manifest);
    tera.register_function(
        "asset_url",
        move |args: &std::collections::HashMap<String, tera::Value>| {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let resolved = manifest.get(path).map(|s| s.as_str()).unwrap_or(path);
            Ok(tera::Value::String(format!("/static/{resolved}")))
        },
    );
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
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}
