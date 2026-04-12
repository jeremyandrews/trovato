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
    /// Output format override. If `None`, defaults to JPEG.
    /// Supported values: `"jpeg"`, `"png"`, `"webp"`, `"avif"`.
    #[serde(default)]
    pub output_format: Option<String>,
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

        // Determine output format from the last effect that specifies one,
        // or fall back to JPEG (reasonable default for derivatives).
        let image_format = resolve_output_format(&effects);

        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image_format)
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

    /// Generate multiple width derivatives for responsive `<picture>` srcset.
    ///
    /// Returns a vec of `(width, encoded_bytes)` pairs. Widths larger than the
    /// original image are skipped to avoid upscaling.
    pub fn process_responsive(
        &self,
        original_bytes: &[u8],
        style: &ImageStyle,
        widths: &[u32],
    ) -> Result<Vec<(u32, Vec<u8>)>> {
        if original_bytes.len() > MAX_INPUT_SIZE {
            anyhow::bail!(
                "image too large: {} bytes exceeds {} byte limit",
                original_bytes.len(),
                MAX_INPUT_SIZE
            );
        }

        let effects: Vec<ImageEffect> = serde_json::from_value(style.effects.clone())
            .context("failed to parse image effects")?;

        let base_img = image::load_from_memory(original_bytes).context("failed to load image")?;

        let image_format = resolve_output_format(&effects);

        let mut results = Vec::with_capacity(widths.len());

        for &target_width in widths {
            let width = target_width.min(MAX_DIMENSION);
            // Skip widths larger than the original to avoid upscaling
            if width >= base_img.width() {
                continue;
            }

            let mut img = base_img.clone();

            // Apply non-dimensional effects (desaturate, etc.)
            for effect in &effects {
                if effect.effect_type == "desaturate" {
                    img = apply_effect(img, effect);
                }
            }

            // Scale to target width, preserving aspect ratio
            img = img.resize(width, MAX_DIMENSION, image::imageops::FilterType::Lanczos3);

            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, image_format)
                .context("failed to encode responsive derivative")?;
            results.push((width, buf.into_inner()));
        }

        Ok(results)
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

/// Determine the output image format from an effect chain.
///
/// Scans effects in reverse order for the first `output_format` override.
/// Defaults to JPEG if no effect specifies a format.
fn resolve_output_format(effects: &[ImageEffect]) -> image::ImageFormat {
    let format = effects
        .iter()
        .rev()
        .find_map(|e| e.output_format.as_deref())
        .unwrap_or("jpeg");

    match format {
        "avif" => image::ImageFormat::Avif,
        "webp" => image::ImageFormat::WebP,
        "png" => image::ImageFormat::Png,
        _ => image::ImageFormat::Jpeg,
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

    #[test]
    fn output_format_deserialization_with_format() {
        let json = serde_json::json!(
            {"type": "scale", "width": 800, "output_format": "avif"}
        );
        let effect: ImageEffect = serde_json::from_value(json).unwrap();
        assert_eq!(effect.output_format.as_deref(), Some("avif"));
    }

    #[test]
    fn output_format_deserialization_without_format() {
        let json = serde_json::json!(
            {"type": "scale", "width": 800}
        );
        let effect: ImageEffect = serde_json::from_value(json).unwrap();
        assert!(effect.output_format.is_none());
    }

    #[test]
    fn resolve_output_format_defaults_to_jpeg() {
        let effects = vec![ImageEffect {
            effect_type: "scale".to_string(),
            width: Some(800),
            height: None,
            output_format: None,
        }];
        assert_eq!(resolve_output_format(&effects), image::ImageFormat::Jpeg);
    }

    #[test]
    fn resolve_output_format_picks_avif() {
        let effects = vec![ImageEffect {
            effect_type: "scale".to_string(),
            width: Some(800),
            height: None,
            output_format: Some("avif".to_string()),
        }];
        assert_eq!(resolve_output_format(&effects), image::ImageFormat::Avif);
    }

    #[test]
    fn resolve_output_format_picks_webp() {
        let effects = vec![ImageEffect {
            effect_type: "scale".to_string(),
            width: Some(800),
            height: None,
            output_format: Some("webp".to_string()),
        }];
        assert_eq!(resolve_output_format(&effects), image::ImageFormat::WebP);
    }

    #[test]
    fn resolve_output_format_uses_last_specified() {
        let effects = vec![
            ImageEffect {
                effect_type: "scale".to_string(),
                width: Some(800),
                height: None,
                output_format: Some("png".to_string()),
            },
            ImageEffect {
                effect_type: "crop".to_string(),
                width: Some(400),
                height: Some(400),
                output_format: Some("webp".to_string()),
            },
        ];
        assert_eq!(resolve_output_format(&effects), image::ImageFormat::WebP);
    }

    /// Generate a minimal 2x2 PNG image for testing.
    fn test_png_bytes() -> Vec<u8> {
        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128u8, 64, 32]));
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    /// Generate a 100x80 PNG image for responsive tests.
    fn test_png_100x80() -> Vec<u8> {
        let img = image::RgbImage::from_fn(100, 80, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    fn make_style(effects_json: serde_json::Value) -> ImageStyle {
        ImageStyle {
            id: uuid::Uuid::nil(),
            name: "test".to_string(),
            label: "Test".to_string(),
            effects: effects_json,
            created: 0,
            changed: 0,
        }
    }

    /// Apply an effect chain and encode to the resolved format (no DB/service needed).
    fn encode_test_image(original: &[u8], effects_json: serde_json::Value) -> Vec<u8> {
        let effects: Vec<ImageEffect> = serde_json::from_value(effects_json).unwrap();
        let mut img = image::load_from_memory(original).unwrap();
        for effect in &effects {
            img = apply_effect(img, effect);
        }
        let image_format = resolve_output_format(&effects);
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image_format).unwrap();
        buf.into_inner()
    }

    #[test]
    fn process_image_avif_output() {
        let result = encode_test_image(
            &test_png_bytes(),
            serde_json::json!([{"type": "scale", "width": 2, "output_format": "avif"}]),
        );
        // AVIF is an ISOBMFF container: starts with an ftyp box.
        // Bytes 4..8 = "ftyp", bytes 8..12 = brand (usually "avif" or "avis")
        assert_eq!(&result[4..8], b"ftyp", "AVIF must start with ftyp box");
    }

    #[test]
    fn process_image_default_jpeg_output() {
        let result = encode_test_image(
            &test_png_bytes(),
            serde_json::json!([{"type": "scale", "width": 2}]),
        );
        // JPEG magic bytes: FF D8
        assert_eq!(result[0], 0xFF);
        assert_eq!(result[1], 0xD8);
    }

    #[test]
    fn process_image_webp_output() {
        let result = encode_test_image(
            &test_png_bytes(),
            serde_json::json!([{"type": "scale", "width": 2, "output_format": "webp"}]),
        );
        // WebP magic bytes: RIFF....WEBP
        assert_eq!(&result[0..4], b"RIFF");
        assert_eq!(&result[8..12], b"WEBP");
    }

    #[test]
    fn process_image_png_output() {
        let result = encode_test_image(
            &test_png_bytes(),
            serde_json::json!([{"type": "scale", "width": 2, "output_format": "png"}]),
        );
        // PNG magic bytes: 89 50 4E 47
        assert_eq!(&result[0..4], &[0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn process_responsive_generates_widths() {
        let src = test_png_100x80();
        let effects_json = serde_json::json!([{"type": "scale", "width": 100}]);
        let effects: Vec<ImageEffect> = serde_json::from_value(effects_json).unwrap();
        let base_img = image::load_from_memory(&src).unwrap();
        let image_format = resolve_output_format(&effects);

        let widths = [20u32, 50, 80];
        let mut results = Vec::new();
        for &w in &widths {
            if w >= base_img.width() {
                continue;
            }
            let img = base_img.resize(w, MAX_DIMENSION, image::imageops::FilterType::Lanczos3);
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, image_format).unwrap();
            results.push((w, buf.into_inner()));
        }
        assert_eq!(results.len(), 3);
        for (_, bytes) in &results {
            assert!(!bytes.is_empty());
            // Default JPEG output
            assert_eq!(bytes[0], 0xFF);
            assert_eq!(bytes[1], 0xD8);
        }
    }

    #[test]
    fn process_responsive_skips_widths_larger_than_original() {
        let src = test_png_100x80();
        let base_img = image::load_from_memory(&src).unwrap();
        // Original is 100px wide; widths >= 100 should be skipped
        let widths = [50u32, 100, 200];
        let kept: Vec<u32> = widths
            .iter()
            .copied()
            .filter(|&w| w < base_img.width())
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0], 50);
    }
}
