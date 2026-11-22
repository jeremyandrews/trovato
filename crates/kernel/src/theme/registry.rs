//! The theme registry (B108): the stock theme plus every theme a deployment
//! provides.
//!
//! The stock theme is the template search path the kernel has always loaded
//! (`TEMPLATES_DIR`, after the enabled plugins' `templates/` directories). A
//! further theme is a directory under one of the `THEMES_DIR` search path
//! entries (default `./themes`), found the same way the template search path
//! is: entries in order, a later entry's theme replacing an earlier one of the
//! same name. A theme directory holds
//!
//! - `theme.toml`: `name`, `description`, and optionally `screenshot`, a file
//!   in the theme directory;
//! - `templates/`: templates laid over the stock theme's, so a template the
//!   theme does not provide falls back to the stock one;
//! - `static/`: files served at `/static/themes/<machine name>/…`.
//!
//! The directory's name is the theme's machine name. A directory without a
//! readable manifest, or whose name is not `[a-z0-9_-]+`, is skipped with a
//! warning, so one broken theme cannot stop the server.
//!
//! Which theme a site uses is stored in `site_config` (`theme_default` and
//! `theme_admin`), read once at start and kept in the engine; the
//! appearance screen changes it there and on every other server through the
//! `Themes` cache generation, without a restart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The stock theme's machine name.
pub const STOCK: &str = "stock";

/// `site_config` key: the theme for every page outside `/admin`.
pub const DEFAULT_THEME_KEY: &str = "theme_default";

/// `site_config` key: the theme for `/admin` pages. Absent or empty means the
/// stock theme, which is what the administration pages were built for.
pub const ADMIN_THEME_KEY: &str = "theme_admin";

/// One theme as the appearance screen lists it.
#[derive(Debug, Clone, Serialize)]
pub struct ThemeInfo {
    /// The directory name; `stock` for the stock theme.
    pub machine_name: String,
    pub name: String,
    pub description: String,
    /// The screenshot's file name within the theme directory, if it has one.
    pub screenshot: Option<String>,
    /// The theme directory; `None` for the stock theme.
    #[serde(skip)]
    pub dir: Option<PathBuf>,
}

impl ThemeInfo {
    /// The stock theme.
    pub fn stock() -> Self {
        Self {
            machine_name: STOCK.to_string(),
            name: "Trovato".to_string(),
            description: "The stock theme the kernel ships, and the one every other theme falls \
                          back to for a template it does not provide."
                .to_string(),
            screenshot: None,
            dir: None,
        }
    }

    /// Its `templates/` directory, for a theme that is not the stock one.
    pub fn templates_dir(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("templates"))
    }

    /// Its `static/` directory, for a theme that is not the stock one.
    pub fn static_dir(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("static"))
    }

    /// The screenshot's path on disk, if it has one that stays inside the
    /// theme directory.
    pub fn screenshot_path(&self) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let file = self.screenshot.as_ref()?;
        if !is_safe_relative(file) {
            return None;
        }
        Some(dir.join(file))
    }
}

/// `theme.toml`.
#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    screenshot: Option<String>,
}

/// Whether `name` may be a theme's machine name.
pub fn is_machine_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != STOCK
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A relative path with no `..`, no root and no NUL.
pub fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Every theme: the stock theme first, then each theme found under
/// `themes_dirs`, sorted by name. A later search path entry's theme replaces
/// an earlier one with the same machine name.
pub fn discover(themes_dirs: &[PathBuf]) -> Vec<ThemeInfo> {
    let mut found: Vec<ThemeInfo> = Vec::new();
    for root in themes_dirs {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let Some(machine_name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string)
            else {
                continue;
            };
            if !is_machine_name(&machine_name) {
                tracing::warn!(dir = %dir.display(), "skipping a theme whose directory name is not a machine name");
                continue;
            }
            let manifest = match std::fs::read_to_string(dir.join("theme.toml"))
                .map_err(anyhow::Error::from)
                .and_then(|text| toml::from_str::<Manifest>(&text).map_err(anyhow::Error::from))
            {
                Ok(manifest) => manifest,
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "skipping a theme without a readable theme.toml");
                    continue;
                }
            };
            let info = ThemeInfo {
                machine_name: machine_name.clone(),
                name: if manifest.name.trim().is_empty() {
                    machine_name.clone()
                } else {
                    manifest.name
                },
                description: manifest.description,
                screenshot: manifest.screenshot.filter(|s| is_safe_relative(s)),
                dir: Some(dir),
            };
            found.retain(|t| t.machine_name != machine_name);
            found.push(info);
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    let mut themes = vec![ThemeInfo::stock()];
    themes.extend(found);
    themes
}

/// Read the stored theme choice and apply it to `engine`. A missing or
/// unreadable setting, or a theme that is no longer installed, is the stock
/// theme.
pub async fn load_active(pool: &sqlx::PgPool, engine: &super::ThemeEngine) {
    let read = |key: &'static str| async move {
        match crate::models::SiteConfig::get(pool, key).await {
            Ok(Some(value)) => value.as_str().unwrap_or(STOCK).to_string(),
            Ok(None) => STOCK.to_string(),
            Err(e) => {
                tracing::warn!(error = %e, key, "failed to read the theme setting");
                STOCK.to_string()
            }
        }
    };
    let default = read(DEFAULT_THEME_KEY).await;
    let admin = read(ADMIN_THEME_KEY).await;
    engine.set_active(super::ActiveThemes { default, admin });
}

/// Store the theme choice, apply it here, and tell every other server.
pub async fn save_active(
    pool: &sqlx::PgPool,
    engine: &super::ThemeEngine,
    active: &super::ActiveThemes,
) -> anyhow::Result<()> {
    crate::models::SiteConfig::set(pool, DEFAULT_THEME_KEY, serde_json::json!(active.default))
        .await?;
    crate::models::SiteConfig::set(pool, ADMIN_THEME_KEY, serde_json::json!(active.admin)).await?;
    engine.set_active(active.clone());
    crate::cache::generation::bump(crate::cache::generation::CacheKind::Themes);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn theme(root: &Path, name: &str, manifest: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("templates")).unwrap();
        std::fs::write(dir.join("theme.toml"), manifest).unwrap();
    }

    #[test]
    fn discovery_finds_themes_with_manifests_and_later_roots_win() {
        let base =
            std::env::temp_dir().join(format!("b108_themes_{}", uuid::Uuid::now_v7().simple()));
        let (a, b) = (base.join("a"), base.join("b"));
        theme(
            &a,
            "lake",
            "name = \"Lake\"\ndescription = \"Blue\"\nscreenshot = \"shot.png\"\n",
        );
        theme(&a, "forest", "name = \"Forest\"\n");
        theme(&b, "lake", "name = \"Lake two\"\n");
        theme(&a, "Bad Name", "name = \"x\"\n");
        theme(&a, "nomanifest", "");
        std::fs::remove_file(a.join("nomanifest/theme.toml")).unwrap();
        theme(
            &a,
            "escape",
            "name = \"Escape\"\nscreenshot = \"../../etc/passwd\"\n",
        );

        let themes = discover(&[a.clone(), b.clone(), base.join("missing")]);
        let names: Vec<&str> = themes.iter().map(|t| t.machine_name.as_str()).collect();
        assert_eq!(names, vec!["stock", "escape", "forest", "lake"]);
        let lake = themes.iter().find(|t| t.machine_name == "lake").unwrap();
        assert_eq!(lake.name, "Lake two", "the later root wins");
        let escape = themes.iter().find(|t| t.machine_name == "escape").unwrap();
        assert!(
            escape.screenshot.is_none(),
            "a screenshot outside the theme is dropped"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn machine_names_and_relative_paths() {
        assert!(is_machine_name("lake_2-dark"));
        assert!(!is_machine_name("stock"), "reserved");
        assert!(!is_machine_name("Lake"));
        assert!(is_safe_relative("css/theme.css"));
        assert!(!is_safe_relative("../x"));
        assert!(!is_safe_relative("/etc/passwd"));
        assert!(!is_safe_relative("a/../../b"));
    }
}
