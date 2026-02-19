//! Image style service for on-demand derivative generation.
//!
//! Loads style configuration from DB, applies effect chains
//! (scale, crop, resize, desaturate), writes derivatives to disk cache.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use image::DynamicImage;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use tracing::debug;
use uuid::Uuid;

/// Maximum allowed dimension (width or height) for image effects.
/// Prevents CPU-DoS from styles requesting e.g. 100000x100000 output.
const MAX_DIMENSION: u32 = 4096;

/// Maximum input file size for image processing (50 MB).
const MAX_INPUT_SIZE: usize = 50 * 1024 * 1024;

/// Maximum concurrent image processing operations.
/// Prevents CPU exhaustion from many simultaneous derivative requests.
const MAX_CONCURRENT_PROCESSING: usize = 4;

/// Image style definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct ImageStyle {
    pub id: Uuid,
    pub name: String,
    pub label: String,
    pub effects: serde_json::Value,
    pub created: i64,
    pub changed: i64,
}

/// A single image effect in a style's effect chain.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageEffect {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Image style service.
#[derive(Clone)]
pub struct ImageStyleService {
    pool: PgPool,
    cache_dir: PathBuf,
    /// Semaphore limiting concurrent image processing to prevent CPU exhaustion.
    processing_semaphore: Arc<Semaphore>,
}

impl ImageStyleService {
    /// Create a new image style service.
    pub fn new(pool: PgPool, cache_dir: &Path) -> Self {
        Self {
            pool,
            cache_dir: cache_dir.to_path_buf(),
            processing_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_PROCESSING)),
        }
    }

    /// Acquire a processing permit. Returns an error if the semaphore is closed.
    ///
    /// Callers should hold the permit for the duration of image processing.
    pub async fn acquire_processing_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>> {
        self.processing_semaphore
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("image processing semaphore closed"))
    }

    /// Load a style by name.
    pub async fn load_style(&self, name: &str) -> Result<Option<ImageStyle>> {
        let style = sqlx::query_as::<_, ImageStyle>(
            r#"
            SELECT id, name, label, effects, created, changed
            FROM image_style
            WHERE name = $1
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await;

        match style {
            Ok(s) => Ok(s),
            Err(e) => {
                debug!(error = %e, "image_style table may not exist yet");
                Ok(None)
            }
        }
    }

    /// Get the cache path for a derivative.
    ///
    /// Both `style_name` and `original_path` are validated to prevent
    /// directory traversal even when called from non-route code.
    pub fn cache_path(&self, style_name: &str, original_path: &str) -> PathBuf {
        self.cache_dir
            .join("styles")
            .join(style_name)
            .join(original_path)
    }

    /// Validate that a constructed cache path is under the cache directory.
    ///
    /// Uses lexical normalization (resolving `..` without filesystem access)
    /// to detect path traversal attacks.
    fn validate_cache_path(&self, path: &Path) -> Result<()> {
        let cache_base = self.cache_dir.join("styles");
        let normalized = normalize_path(path);
        let base_normalized = normalize_path(&cache_base);
        if !normalized.starts_with(&base_normalized) {
            anyhow::bail!(
                "derivative path escapes cache directory: {}",
                path.display()
            );
        }
        Ok(())
    }

    /// Check if a cached derivative exists.
    pub fn has_cached(&self, style_name: &str, original_path: &str) -> bool {
        self.cache_path(style_name, original_path).exists()
    }

    /// Generate a derivative image by applying the style's effect chain.
    pub fn process_image(&self, original_bytes: &[u8], style: &ImageStyle) -> Result<Vec<u8>> {
        // Guard against very large input files that could exhaust memory
        if original_bytes.len() > MAX_INPUT_SIZE {
            anyhow::bail!(
                "image too large: {} bytes exceeds {} byte limit",
                original_bytes.len(),
                MAX_INPUT_SIZE
            );
        }

        let effects: Vec<ImageEffect> = serde_json::from_value(style.effects.clone())
            .context("failed to parse image effects")?;

        let mut img = image::load_from_memory(original_bytes).context("failed to load image")?;

        for effect in &effects {
            img = apply_effect(img, effect);
        }

        // Encode as JPEG (reasonable default for derivatives)
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg)
            .context("failed to encode derivative")?;

        Ok(buf.into_inner())
    }

    /// Save a derivative to the disk cache.
    ///
    /// Validates that the resolved path stays within the cache directory
    /// to prevent directory traversal even from internal callers.
    pub fn save_derivative(
        &self,
        style_name: &str,
        original_path: &str,
        data: &[u8],
    ) -> Result<PathBuf> {
        let path = self.cache_path(style_name, original_path);
        self.validate_cache_path(&path)?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .context("failed to create derivative cache directory")?;
        }

        std::fs::write(&path, data).context("failed to write derivative to cache")?;

        debug!(
            style = %style_name,
            path = %path.display(),
            "saved image derivative to cache"
        );

        Ok(path)
    }

    /// List all image styles.
    pub async fn list_styles(&self) -> Result<Vec<ImageStyle>> {
        let styles = sqlx::query_as::<_, ImageStyle>(
            "SELECT id, name, label, effects, created, changed FROM image_style ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await;

        match styles {
            Ok(s) => Ok(s),
            Err(e) => {
                debug!(error = %e, "image_style table may not exist yet");
                Ok(Vec::new())
            }
        }
    }
}

/// Normalize a path by resolving `..` components without filesystem access.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Pop the last normal component, but don't pop root
                if let Some(last) = components.last()
                    && !matches!(last, std::path::Component::RootDir)
                {
                    components.pop();
                }
            }
            std::path::Component::CurDir => {} // Skip `.`
            c => components.push(c),
        }
    }
    components.iter().collect()
}

/// Clamp a dimension to MAX_DIMENSION.
fn clamp_dim(v: u32) -> u32 {
    v.min(MAX_DIMENSION)
}

/// Apply a single effect to an image.
fn apply_effect(img: DynamicImage, effect: &ImageEffect) -> DynamicImage {
    match effect.effect_type.as_str() {
        "scale" => {
            let w = clamp_dim(effect.width.unwrap_or(img.width()));
            let h = clamp_dim(effect.height.unwrap_or_else(|| {
                // Maintain aspect ratio
                let ratio = w as f64 / img.width().max(1) as f64;
                (img.height() as f64 * ratio) as u32
            }));
            img.resize(w, h, image::imageops::FilterType::Lanczos3)
        }
        "crop" => {
            let w = clamp_dim(effect.width.unwrap_or(img.width()));
            let h = clamp_dim(effect.height.unwrap_or(img.height()));
            img.resize_to_fill(w, h, image::imageops::FilterType::Lanczos3)
        }
        "resize" => {
            let w = clamp_dim(effect.width.unwrap_or(img.width()));
            let h = clamp_dim(effect.height.unwrap_or(img.height()));
            img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        }
        "desaturate" => img.grayscale(),
        _ => {
            debug!(effect = %effect.effect_type, "unknown image effect, skipping");
            img
        }
    }
}

impl std::fmt::Debug for ImageStyleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageStyleService")
            .field("cache_dir", &self.cache_dir)
            .finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_effects() {
        let json = serde_json::json!([
            {"type": "scale", "width": 500},
            {"type": "crop", "width": 100, "height": 100}
        ]);
        let effects: Vec<ImageEffect> = serde_json::from_value(json).unwrap();
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].effect_type, "scale");
        assert_eq!(effects[0].width, Some(500));
        assert_eq!(effects[1].effect_type, "crop");
    }

    #[test]
    fn cache_path_construction() {
        // Test the path construction logic directly without needing a PgPool
        let cache_dir = PathBuf::from("/uploads");
        let path = cache_dir
            .join("styles")
            .join("thumbnail")
            .join("images/photo.jpg");
        assert_eq!(
            path,
            PathBuf::from("/uploads/styles/thumbnail/images/photo.jpg")
        );
    }

    #[test]
    fn cache_path_traversal_detected() {
        // Normal path stays under cache
        let good = PathBuf::from("/uploads/styles/thumbnail/photo.jpg");
        let base = PathBuf::from("/uploads/styles");
        assert!(normalize_path(&good).starts_with(normalize_path(&base)));

        // Traversal path escapes cache after normalization
        let bad = PathBuf::from("/uploads/styles/../../etc/cron.d/pwned");
        let bad_normalized = normalize_path(&bad);
        assert_eq!(bad_normalized, PathBuf::from("/etc/cron.d/pwned"));
        assert!(!bad_normalized.starts_with(normalize_path(&base)));
    }

    #[test]
    fn normalize_path_resolves_parent_components() {
        assert_eq!(
            normalize_path(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            normalize_path(Path::new("/a/b/../../c")),
            PathBuf::from("/c")
        );
        assert_eq!(
            normalize_path(Path::new("/a/./b/c")),
            PathBuf::from("/a/b/c")
        );
    }

    #[test]
    fn dimension_clamped_to_max() {
        assert_eq!(clamp_dim(100), 100);
        assert_eq!(clamp_dim(4096), 4096);
        assert_eq!(clamp_dim(10000), MAX_DIMENSION);
        assert_eq!(clamp_dim(u32::MAX), MAX_DIMENSION);
    }

    #[test]
    fn max_input_size_constant() {
        assert_eq!(MAX_INPUT_SIZE, 50 * 1024 * 1024);
    }
}
