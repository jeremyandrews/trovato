//! CLI command implementations for plugin management.
//!
//! These commands operate with a minimal context (database pool only),
//! without starting the full server or loading WASM modules.

use std::path::Path;

use anyhow::{Context, Result, bail};
use sqlx::PgPool;

use super::migration;
use super::runtime::PluginRuntime;
use super::status;

/// List all discovered plugins and their status.
pub async fn cmd_plugin_list(pool: &PgPool, plugins_dir: &Path) -> Result<()> {
    let discovered = PluginRuntime::discover_plugins(plugins_dir);
    let statuses = status::get_all_statuses(pool).await?;
    let status_map: std::collections::HashMap<String, &status::PluginStatus> =
        statuses.iter().map(|s| (s.name.clone(), s)).collect();

    if discovered.is_empty() && statuses.is_empty() {
        println!("No plugins found.");
        return Ok(());
    }

    println!(
        "{:<20} {:<12} {:<12} {:<10}",
        "PLUGIN", "VERSION", "STATUS", "MIGRATIONS"
    );
    println!("{}", "-".repeat(56));

    // Show all discovered plugins
    for (name, (info, _dir)) in &discovered {
        let status_str = match status_map.get(name) {
            Some(s) if s.status == status::STATUS_ENABLED => "enabled",
            Some(_) => "disabled",
            None => "not installed",
        };

        let migration_count = info.migrations.files.len();

        println!(
            "{:<20} {:<12} {:<12} {}",
            name, info.version, status_str, migration_count
        );
    }

    // Show installed plugins not on disk
    for ps in &statuses {
        if !discovered.contains_key(&ps.name) {
            let status_str = if ps.status == status::STATUS_ENABLED {
                "enabled"
            } else {
                "disabled"
            };
            println!(
                "{:<20} {:<12} {:<12} {} (not on disk)",
                ps.name, ps.version, status_str, "?"
            );
        }
    }

    Ok(())
}

/// Install a plugin: validate dependencies, run migrations, set status to enabled.
pub async fn cmd_plugin_install(pool: &PgPool, plugins_dir: &Path, name: &str) -> Result<()> {
    let discovered = PluginRuntime::discover_plugins(plugins_dir);

    let (info, dir) = discovered
        .get(name)
        .with_context(|| format!("plugin '{}' not found in {}", name, plugins_dir.display()))?;

    if status::is_installed(pool, name).await? {
        bail!("plugin '{}' is already installed", name);
    }

    // Check that all declared dependencies are installed before proceeding
    let installed_names = status::get_enabled_names(pool).await?;
    let installed_set: std::collections::HashSet<String> = installed_names.into_iter().collect();

    for dep in &info.dependencies {
        if !installed_set.contains(dep) {
            bail!(
                "plugin '{}' depends on '{}' which is not installed. \
                 Install '{}' first with: trovato plugin install {}",
                name,
                dep,
                dep,
                dep,
            );
        }
    }

    for dep in &info.migrations.depends_on {
        if !installed_set.contains(dep) {
            bail!(
                "plugin '{}' migration depends on '{}' which is not installed. \
                 Install '{}' first with: trovato plugin install {}",
                name,
                dep,
                dep,
                dep,
            );
        }
    }

    // Run migrations if any
    if !info.migrations.files.is_empty() {
        let applied = migration::run_plugin_migrations(pool, name, info, dir).await?;
        if applied.is_empty() {
            println!("No pending migrations for '{}'.", name);
        } else {
            for m in &applied {
                println!("  applied: {}", m);
            }
        }
    }

    // Insert into plugin_status
    status::install_plugin(pool, name, &info.version).await?;

    println!("Plugin '{}' v{} installed and enabled.", name, info.version);
    Ok(())
}

/// Run pending migrations for one or all plugins.
pub async fn cmd_plugin_migrate(
    pool: &PgPool,
    plugins_dir: &Path,
    name: Option<&str>,
) -> Result<()> {
    let discovered = PluginRuntime::discover_plugins(plugins_dir);

    if let Some(name) = name {
        // Single plugin
        let (info, dir) = discovered
            .get(name)
            .with_context(|| format!("plugin '{}' not found in {}", name, plugins_dir.display()))?;

        let applied = migration::run_plugin_migrations(pool, name, info, dir).await?;
        if applied.is_empty() {
            println!("No pending migrations for '{}'.", name);
        } else {
            println!("Applied {} migration(s) for '{}':", applied.len(), name);
            for m in &applied {
                println!("  {}", m);
            }
        }
    } else {
        // All plugins
        let results = migration::run_all_pending_migrations(pool, &discovered).await?;
        if results.is_empty() {
            println!("No pending migrations.");
        } else {
            for (plugin, applied) in &results {
                println!("{}:", plugin);
                for m in applied {
                    println!("  {}", m);
                }
            }
        }
    }

    Ok(())
}

/// Enable a plugin.
pub async fn cmd_plugin_enable(pool: &PgPool, name: &str) -> Result<()> {
    if !status::is_installed(pool, name).await? {
        bail!(
            "plugin '{}' is not installed. Run `trovato plugin install {}` first.",
            name,
            name
        );
    }

    let updated = status::set_status(pool, name, status::STATUS_ENABLED).await?;
    if updated {
        println!("Plugin '{}' enabled.", name);
    } else {
        println!("Plugin '{}' not found.", name);
    }
    Ok(())
}

/// Disable a plugin.
pub async fn cmd_plugin_disable(pool: &PgPool, name: &str) -> Result<()> {
    if !status::is_installed(pool, name).await? {
        bail!("plugin '{}' is not installed.", name);
    }

    let updated = status::set_status(pool, name, status::STATUS_DISABLED).await?;
    if updated {
        println!(
            "Plugin '{}' disabled. Restart the server for changes to take effect.",
            name
        );
    } else {
        println!("Plugin '{}' not found.", name);
    }
    Ok(())
}
