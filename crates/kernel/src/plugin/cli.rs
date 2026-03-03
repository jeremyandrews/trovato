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

/// Scaffold a new plugin directory under `{workspace_root}/plugins/{name}/`.
///
/// Creates `Cargo.toml`, `{name}.info.toml`, `src/lib.rs`, and
/// `migrations/.gitkeep`. Also appends the new crate to the workspace
/// `Cargo.toml` members list.
///
/// Must be run from the repository root (the directory containing the
/// workspace `Cargo.toml`). Any failure after directory creation rolls
/// back the partially-written directory.
///
/// # Errors
///
/// Returns an error if the name is invalid, the directory already exists,
/// `workspace_root` is not a Trovato workspace root, or any file write fails.
pub fn cmd_plugin_new(workspace_root: &Path, name: &str) -> Result<()> {
    // Validate: must not be empty.
    if name.is_empty() {
        bail!("plugin name cannot be empty; must match [a-z][a-z0-9_]*");
    }

    // Validate: lowercase alphanumeric + underscores, must start with a letter.
    if !name
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase())
        .unwrap_or(false)
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!(
            "invalid plugin name '{name}': must match [a-z][a-z0-9_]* \
             (lowercase letters, digits, underscores only)"
        );
    }

    // Validate: workspace_root looks like a Trovato repository root.
    let workspace_cargo = workspace_root.join("Cargo.toml");
    if !workspace_cargo.exists() {
        bail!(
            "no Cargo.toml found at '{}' — run `trovato plugin new` from the repository root",
            workspace_root.display()
        );
    }
    let workspace_content = std::fs::read_to_string(&workspace_cargo)
        .with_context(|| format!("failed to read {}", workspace_cargo.display()))?;
    if !workspace_content.contains("[workspace]") {
        bail!(
            "'{}' is not a workspace Cargo.toml — run `trovato plugin new` from the repository root",
            workspace_cargo.display()
        );
    }

    let plugin_dir = workspace_root.join("plugins").join(name);
    if plugin_dir.exists() {
        bail!(
            "directory '{}' already exists — delete it first if you want to regenerate",
            plugin_dir.display()
        );
    }

    // Create directory tree then write all files. On any failure, remove the
    // partially-created directory so the user can retry without manual cleanup.
    let src_dir = plugin_dir.join("src");
    let migrations_dir = plugin_dir.join("migrations");
    std::fs::create_dir_all(&src_dir)
        .with_context(|| format!("failed to create {}", src_dir.display()))?;
    std::fs::create_dir_all(&migrations_dir)
        .with_context(|| format!("failed to create {}", migrations_dir.display()))?;

    let result = write_scaffold_files(name, &plugin_dir, &src_dir, &migrations_dir, workspace_root);
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&plugin_dir);
        return Err(e);
    }

    println!("Plugin '{name}' scaffolded at plugins/{name}/");
    println!();
    println!("Next steps:");
    println!("  1. Edit plugins/{name}/src/lib.rs to implement your taps");
    println!("  2. Build: cargo build --target wasm32-wasip1 -p {name} --release");
    println!("  3. Install: trovato plugin install {name}");

    Ok(())
}

/// Write all scaffold files. Extracted so `cmd_plugin_new` can roll back on error.
fn write_scaffold_files(
    name: &str,
    plugin_dir: &Path,
    src_dir: &Path,
    migrations_dir: &Path,
    workspace_root: &Path,
) -> Result<()> {
    // Write Cargo.toml.
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "TODO: describe this plugin"

[lints]
workspace = true

[lib]
crate-type = ["cdylib"]

[dependencies]
trovato-sdk = {{ path = "../../crates/plugin-sdk" }}
serde = {{ workspace = true }}
serde_json = {{ workspace = true }}
"#
    );
    write_file(&plugin_dir.join("Cargo.toml"), &cargo_toml)?;

    // Write {name}.info.toml.
    let info_toml = format!(
        r#"name = "{name}"
description = "TODO: describe this plugin"
version = "0.1.0"
dependencies = []

[taps]
implements = [
    "tap_install",
    "tap_cron",
    "tap_queue_info",
    "tap_queue_worker",
]
weight = 0

[migrations]
files = []
"#
    );
    write_file(&plugin_dir.join(format!("{name}.info.toml")), &info_toml)?;

    // Write src/lib.rs.
    let lib_rs = format!(
        r#"//! {name} plugin for Trovato.
//!
//! TODO: describe what this plugin does.

use trovato_sdk::prelude::*;

/// Called once when the plugin is first enabled.
///
/// Use this to seed initial data, register content types, or create
/// database records your plugin depends on.
#[plugin_tap]
pub fn tap_install() -> serde_json::Value {{
    serde_json::json!({{ "status": "ok" }})
}}

/// Called on every cron cycle (every 60 seconds).
///
/// Use this to fetch external data, check for updates, or push work
/// onto a queue for `tap_queue_worker` to process.
#[plugin_tap]
pub fn tap_cron(_input: CronInput) -> serde_json::Value {{
    serde_json::json!({{ "status": "ok" }})
}}

/// Declares the queues this plugin owns.
///
/// Return JSON describing queue name(s) and max concurrency. The kernel
/// will call `tap_queue_worker` for each job pushed onto these queues.
///
/// Example: `[{{"name": "{name}_import", "concurrency": 4}}]`
#[plugin_tap]
pub fn tap_queue_info() -> serde_json::Value {{
    serde_json::json!([])
}}

/// Processes a single queue job.
///
/// Called by the kernel for each job dequeued from the queues declared
/// in `tap_queue_info`. Parse `input["payload"]` and perform the work.
#[plugin_tap]
pub fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {{
    let _ = input;
    serde_json::json!({{ "status": "ok" }})
}}
"#
    );
    write_file(&src_dir.join("lib.rs"), &lib_rs)?;

    // Write migrations/.gitkeep.
    write_file(&migrations_dir.join(".gitkeep"), "")?;

    // Append to workspace Cargo.toml members list.
    append_workspace_member(workspace_root, name)?;

    Ok(())
}

/// Write `content` to `path`, creating the file (error if it already exists).
fn write_file(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

/// Append `"plugins/{name}"` to the `[workspace]` `members` array in the
/// root `Cargo.toml`.
///
/// Only modifies the first `members = [` block, not `default-members`.
/// Returns an error (rather than a warning) if the block cannot be located,
/// so that the caller's rollback logic can clean up the partially-written
/// plugin directory.
fn append_workspace_member(workspace_root: &Path, name: &str) -> Result<()> {
    let cargo_path = workspace_root.join("Cargo.toml");
    let content = std::fs::read_to_string(&cargo_path)
        .with_context(|| format!("failed to read {}", cargo_path.display()))?;

    let entry = format!("\"plugins/{name}\"");

    // Skip if already present (idempotent).
    if content.contains(&entry) {
        return Ok(());
    }

    // Locate the `members = [` block. We operate only within this block
    // to avoid accidentally modifying `default-members`.
    let Some(members_start) = content.find("members = [") else {
        bail!(
            "could not locate `members = [` in {}; \
             workspace Cargo.toml may have an unexpected format",
            cargo_path.display()
        );
    };

    // Find the closing `]` of the members block.
    let members_region_start = members_start + "members = [".len();
    let Some(members_close_rel) = content[members_region_start..].find(']') else {
        bail!(
            "could not locate closing `]` of members block in {}",
            cargo_path.display()
        );
    };
    let members_close = members_region_start + members_close_rel;

    // Find the last `"plugins/` entry within the members block only.
    let members_block = &content[members_region_start..members_close];
    let Some(last_plugin_rel) = members_block.rfind("\"plugins/") else {
        bail!(
            "could not locate any plugin entries in members block of {}; \
             add 'plugins/{name}' to [workspace] members manually",
            cargo_path.display()
        );
    };

    // Find end of that line.
    let after_last_plugin = members_region_start + last_plugin_rel;
    let line_end = content[after_last_plugin..]
        .find('\n')
        .map(|i| after_last_plugin + i + 1)
        .unwrap_or(content.len());

    let insertion = format!("    \"plugins/{name}\",\n");
    let mut new_content = content.clone();
    new_content.insert_str(line_end, &insertion);

    std::fs::write(&cargo_path, new_content)
        .with_context(|| format!("failed to update {}", cargo_path.display()))?;

    Ok(())
}

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
        "{:<20} {:<12} {:<14} {:<13} {:<10}",
        "PLUGIN", "VERSION", "STATUS", "AUTO-ENABLE", "MIGRATIONS"
    );
    println!("{}", "-".repeat(71));

    // Show all discovered plugins
    for (name, (info, _dir)) in &discovered {
        let status_str = match status_map.get(name) {
            Some(s) if s.status == status::STATUS_ENABLED => "enabled",
            Some(_) => "disabled",
            None => "not installed",
        };

        let auto_enable = if info.default_enabled { "yes" } else { "no" };
        let migration_count = info.migrations.files.len();

        println!(
            "{:<20} {:<12} {:<14} {:<13} {}",
            name, info.version, status_str, auto_enable, migration_count
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
                "{:<20} {:<12} {:<14} {:<13} ? (not on disk)",
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

    // Copy the compiled WASM into the plugin directory (always overwrite so
    // reinstalls pick up the latest build).
    // `cargo build --target wasm32-wasip1` outputs to target/wasm32-wasip1/release/;
    // the server expects the file at plugins/{name}/{name}.wasm.
    let wasm_dest = dir.join(format!("{name}.wasm"));
    let workspace_root = plugins_dir.parent().unwrap_or(plugins_dir);
    let wasm_src = workspace_root
        .join("target")
        .join("wasm32-wasip1")
        .join("release")
        .join(format!("{name}.wasm"));
    if wasm_src.exists() {
        std::fs::copy(&wasm_src, &wasm_dest).with_context(|| {
            format!(
                "failed to copy WASM from '{}' to '{}'",
                wasm_src.display(),
                wasm_dest.display()
            )
        })?;
        println!("Installed WASM: {}", wasm_dest.display());
    } else {
        println!(
            "Warning: WASM not found at '{}'. Build it first with:\n  \
             cargo build --target wasm32-wasip1 -p {name} --release",
            wasm_src.display()
        );
    }

    let already_installed = status::is_installed(pool, name).await?;

    if !already_installed {
        // Check that all declared dependencies are installed before proceeding
        let installed_names = status::get_enabled_names(pool).await?;
        let installed_set: std::collections::HashSet<String> =
            installed_names.into_iter().collect();

        for dep in &info.dependencies {
            if !installed_set.contains(dep) {
                bail!(
                    "plugin '{name}' depends on '{dep}' which is not installed. \
                     Install '{dep}' first with: trovato plugin install {dep}",
                );
            }
        }

        for dep in &info.migrations.depends_on {
            if !installed_set.contains(dep) {
                bail!(
                    "plugin '{name}' migration depends on '{dep}' which is not installed. \
                     Install '{dep}' first with: trovato plugin install {dep}",
                );
            }
        }
    }

    // Run any pending migrations (safe to call whether or not already installed;
    // idempotent via ON CONFLICT DO NOTHING in the migration tracker).
    if !info.migrations.files.is_empty() {
        let applied = migration::run_plugin_migrations(pool, name, info, dir).await?;
        if applied.is_empty() {
            if already_installed {
                println!("Plugin '{name}' is already installed. No pending migrations.");
            } else {
                println!("No pending migrations for '{name}'.");
            }
        } else {
            for m in &applied {
                println!("  applied: {m}");
            }
        }
    } else if already_installed {
        println!("Plugin '{name}' is already installed.");
    }

    if !already_installed {
        // Insert into plugin_status
        status::install_plugin(pool, name, &info.version).await?;
        println!("Plugin '{}' v{} installed and enabled.", name, info.version);
    }
    println!("tap_install will fire on next server startup.");
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
            println!("No pending migrations for '{name}'.");
        } else {
            println!("Applied {} migration(s) for '{}':", applied.len(), name);
            for m in &applied {
                println!("  {m}");
            }
        }
    } else {
        // All plugins
        let results = migration::run_all_pending_migrations(pool, &discovered).await?;
        if results.is_empty() {
            println!("No pending migrations.");
        } else {
            for (plugin, applied) in &results {
                println!("{plugin}:");
                for m in applied {
                    println!("  {m}");
                }
            }
        }
    }

    Ok(())
}

/// Enable a plugin (database only).
///
/// CLI commands run in a separate process without access to the server's
/// in-memory `AppState`. Changes take effect on the next server restart.
/// For immediate effect, use the admin UI (`/admin/plugins`) instead.
pub async fn cmd_plugin_enable(pool: &PgPool, name: &str) -> Result<()> {
    if !status::is_installed(pool, name).await? {
        bail!("plugin '{name}' is not installed. Run `trovato plugin install {name}` first.");
    }

    let updated = status::set_status(pool, name, status::STATUS_ENABLED).await?;
    if updated {
        println!("Plugin '{name}' enabled.");
        println!("Note: if the server is running, restart it for CLI changes to take effect.");
    } else {
        println!("Plugin '{name}' not found.");
    }
    Ok(())
}

/// Disable a plugin (database only).
///
/// CLI commands run in a separate process without access to the server's
/// in-memory `AppState`. Changes take effect on the next server restart.
/// For immediate effect, use the admin UI (`/admin/plugins`) instead.
pub async fn cmd_plugin_disable(pool: &PgPool, name: &str) -> Result<()> {
    if !status::is_installed(pool, name).await? {
        bail!("plugin '{name}' is not installed.");
    }

    let updated = status::set_status(pool, name, status::STATUS_DISABLED).await?;
    if updated {
        println!("Plugin '{name}' disabled.");
        println!("Note: if the server is running, restart it for CLI changes to take effect.");
    } else {
        println!("Plugin '{name}' not found.");
    }
    Ok(())
}
