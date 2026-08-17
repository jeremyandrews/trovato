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

/// Resolve the static-asset search path.
///
/// STATIC_DIR is a search path, not a single directory. It is split on the
/// platform path separator (`:` on unix), so the historical single-directory
/// value keeps parsing to a one-element list and existing deployments are
/// unaffected.
///
/// A later directory wins a collision, the same direction `TEMPLATES_DIR`
/// resolves in, so an application overlays a kernel asset by appending its own
/// directory rather than writing into the kernel tree.
pub(crate) fn static_dirs() -> Vec<PathBuf> {
    crate::config::split_search_path("STATIC_DIR", "./static")
}

/// Serve a static file.
async fn serve_static(Path(path): Path<String>) -> Response<Body> {
    let path = path.trim_start_matches('/');

    let Some((file_path, content)) = resolve_static_file(&static_dirs(), path).await else {
        return not_found();
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

/// Find the highest-precedence readable file for `path` across the search path.
///
/// Directories are searched from the end, so a later directory wins a
/// collision and an application asset overrides a kernel asset of the same
/// name. This matches the direction the theme engine overlays templates in.
///
/// The path-traversal guard runs once on the request path, before any root is
/// joined, so it holds for every root by construction: no ordering of the
/// search path can produce an escape that a single directory would have
/// rejected.
async fn resolve_static_file(dirs: &[PathBuf], path: &str) -> Option<(PathBuf, Vec<u8>)> {
    // Security: prevent path traversal
    if path.contains("..") || path.contains('\0') {
        return None;
    }

    for dir in dirs.iter().rev() {
        let file_path = dir.join(path);
        match fs::read(&file_path).await {
            Ok(content) => return Some((file_path, content)),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(path = %file_path.display(), error = %e, "failed to read static file");
                }
            }
        }
    }

    None
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
/// Scans the static search path, computes SHA-256 of each file, and creates
/// a mapping like `css/theme.css` → `css/theme.a1b2c3d4.css`.
/// The short hash (first 8 hex chars) is inserted before the extension.
pub fn build_asset_manifest() -> std::collections::HashMap<String, String> {
    build_asset_manifest_from(&static_dirs())
}

/// Build the asset manifest from an explicit search path.
///
/// Every directory is scanned in order and a later directory replaces an
/// earlier entry of the same relative path, which is the precedence
/// `resolve_static_file` serves with. Both ends have to agree: if the manifest
/// hashed a shadowed file, `asset_url` would emit a hash of bytes nobody is
/// ever served, and the cache-busting URL would be wrong for the file behind
/// it.
fn build_asset_manifest_from(dirs: &[PathBuf]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;

    let mut manifest = HashMap::new();

    for dir in dirs {
        // A missing directory yields nothing rather than an error, so an
        // optional app overlay directory is not a startup failure.
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };

        for entry in entries.flatten() {
            scan_dir_recursive(dir, &entry.path(), &mut manifest);
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Build a throwaway static root and return its path.
    fn scratch_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trovato_static_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch static root");
        dir
    }

    fn write_asset(root: &std::path::Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create asset subdir");
        }
        std::fs::write(path, body).expect("write asset");
    }

    fn short_hash(body: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(body.as_bytes());
        hex::encode(hasher.finalize())[..8].to_string()
    }

    /// An asset that exists only in the overlay is served, which is the whole
    /// point: an application ships a stylesheet from its own repository
    /// without adding a file to the kernel tree.
    #[tokio::test]
    async fn overlay_only_asset_is_served() {
        let kernel = scratch_root("kernel_only");
        let app = scratch_root("app_only");
        write_asset(&app, "css/app.css", "APP");

        let dirs = vec![kernel.clone(), app.clone()];
        let (path, content) = resolve_static_file(&dirs, "css/app.css")
            .await
            .expect("an asset present only in the overlay must be served");
        assert_eq!(content, b"APP");
        assert!(path.starts_with(&app));

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A later directory wins a collision, the same direction the theme engine
    /// overlays templates in, so an app can restyle a kernel asset in place.
    #[tokio::test]
    async fn later_directory_wins_a_collision() {
        let kernel = scratch_root("kernel_collide");
        let app = scratch_root("app_collide");
        write_asset(&kernel, "css/theme.css", "KERNEL");
        write_asset(&app, "css/theme.css", "APP");

        let dirs = vec![kernel.clone(), app.clone()];
        let (_, content) = resolve_static_file(&dirs, "css/theme.css")
            .await
            .expect("the asset resolves");
        assert_eq!(content, b"APP", "the later root must win");

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// The manifest hash has to be the hash of the bytes actually served. If
    /// the two ends disagreed, `asset_url` would emit a cache-busting URL keyed
    /// to a shadowed file, and a changed overlay asset would keep its old URL.
    #[tokio::test]
    async fn manifest_hash_tracks_the_asset_actually_served() {
        let kernel = scratch_root("kernel_manifest");
        let app = scratch_root("app_manifest");
        write_asset(&kernel, "css/theme.css", "KERNEL");
        write_asset(&app, "css/theme.css", "APP");

        let dirs = vec![kernel.clone(), app.clone()];
        let manifest = build_asset_manifest_from(&dirs);
        assert_eq!(
            manifest.get("css/theme.css"),
            Some(&format!("css/theme.{}.css", short_hash("APP"))),
            "the manifest must hash the overlay copy, not the shadowed base one"
        );
        assert_ne!(short_hash("APP"), short_hash("KERNEL"));

        // And the served bytes are the ones that hash was taken over.
        let (_, content) = resolve_static_file(&dirs, "css/theme.css")
            .await
            .expect("the asset resolves");
        assert_eq!(content, b"APP");

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A one-element search path behaves exactly as the single-directory
    /// `STATIC_DIR` did, both for serving and for the manifest.
    #[tokio::test]
    async fn single_directory_is_unchanged() {
        let only = scratch_root("single");
        write_asset(&only, "css/theme.css", "ONLY");
        write_asset(&only, "js/app.js", "JS");

        let dirs = vec![only.clone()];
        let (_, content) = resolve_static_file(&dirs, "css/theme.css")
            .await
            .expect("the asset resolves");
        assert_eq!(content, b"ONLY");
        assert!(resolve_static_file(&dirs, "css/absent.css").await.is_none());

        let manifest = build_asset_manifest_from(&dirs);
        assert_eq!(
            manifest.get("css/theme.css"),
            Some(&format!("css/theme.{}.css", short_hash("ONLY")))
        );
        assert_eq!(
            manifest.get("js/app.js"),
            Some(&format!("js/app.{}.js", short_hash("JS")))
        );
        assert_eq!(manifest.len(), 2);

        std::fs::remove_dir_all(&only).ok();
    }

    /// Path traversal stays blocked whatever the search path looks like. The
    /// guard runs before any root is joined, so adding roots cannot open an
    /// escape: the target below is reachable from either root by `..`.
    #[tokio::test]
    async fn traversal_is_blocked_on_every_root() {
        let parent = scratch_root("traversal");
        let kernel = parent.join("kernel");
        let app = parent.join("app");
        std::fs::create_dir_all(&kernel).expect("create kernel root");
        std::fs::create_dir_all(&app).expect("create app root");
        write_asset(&parent, "secret.txt", "SECRET");

        // Without the guard, this path resolves to a real file under both roots.
        assert!(kernel.join("../secret.txt").exists());
        assert!(app.join("../secret.txt").exists());

        for dirs in [
            vec![kernel.clone(), app.clone()],
            vec![app.clone(), kernel.clone()],
            vec![kernel.clone()],
        ] {
            assert!(
                resolve_static_file(&dirs, "../secret.txt").await.is_none(),
                "traversal must be refused for every search path"
            );
            assert!(
                resolve_static_file(&dirs, "css/../../secret.txt")
                    .await
                    .is_none()
            );
        }
        assert!(
            resolve_static_file(std::slice::from_ref(&kernel), "css/theme\0.css")
                .await
                .is_none()
        );

        std::fs::remove_dir_all(&parent).ok();
    }

    /// A missing directory in the search path is skipped, not fatal, so an
    /// optional overlay that a deployment does not use costs nothing.
    #[tokio::test]
    async fn missing_directory_is_tolerated() {
        let kernel = scratch_root("kernel_missing");
        write_asset(&kernel, "css/theme.css", "KERNEL");
        let absent = std::env::temp_dir().join("trovato_static_definitely_absent_dir");
        std::fs::remove_dir_all(&absent).ok();

        let dirs = vec![kernel.clone(), absent];
        let (_, content) = resolve_static_file(&dirs, "css/theme.css")
            .await
            .expect("a missing overlay must not hide the base asset");
        assert_eq!(content, b"KERNEL");
        assert_eq!(build_asset_manifest_from(&dirs).len(), 1);

        std::fs::remove_dir_all(&kernel).ok();
    }

    /// `static_dirs` reads `STATIC_DIR`, and a plain single value still parses
    /// to exactly one entry.
    ///
    /// The only test in this crate that mutates the process environment, and it
    /// is here rather than spread over the parsing cases because parsing is
    /// covered without touching anything global by `config::search_path_tests`.
    /// What is left, and can only be tested at the edge, is the wiring: that the
    /// variable is spelled `STATIC_DIR`, that its value reaches the search-path
    /// split, and that the default is `./static`.
    ///
    /// [`EnvGuard`](trovato_test_utils::env::EnvGuard) makes that honest. It
    /// serializes this write against every other environment mutation in the
    /// workspace and restores `STATIC_DIR` when it drops — including while a
    /// failing assert unwinds, which a hand-rolled restore placed after the
    /// asserts does not. What no lock can prevent is a concurrent *read* racing
    /// the write, so this stays one call site rather than a pattern to copy: in
    /// this test binary the only readers of `STATIC_DIR` are `static_dirs` calls
    /// made from here, since `serve_static`, `build_asset_manifest` and the
    /// Pagefind indexer all need a running app or a database to reach.
    #[test]
    fn static_dir_names_the_search_path() {
        let mut env = trovato_test_utils::env::EnvGuard::new();

        env.remove("STATIC_DIR");
        assert_eq!(static_dirs(), vec![PathBuf::from("./static")]);

        env.set("STATIC_DIR", "/srv/static");
        assert_eq!(static_dirs(), vec![PathBuf::from("/srv/static")]);

        let joined = std::env::join_paths([
            PathBuf::from("/srv/static"),
            PathBuf::from("/opt/app/static"),
        ])
        .expect("join search path");
        env.set("STATIC_DIR", &joined);
        assert_eq!(
            static_dirs(),
            vec![
                PathBuf::from("/srv/static"),
                PathBuf::from("/opt/app/static"),
            ]
        );
    }
}
